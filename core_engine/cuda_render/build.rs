use std::env;
use std::path::PathBuf;

/// Parse `pub const FLOATS_PER_PRIM: usize = N;` from core_math/src/lib.rs
fn parse_floats_per_prim(path: &PathBuf) -> i32 {
    let src = std::fs::read_to_string(path)
        .expect("Failed to read core_math/src/lib.rs for FLOATS_PER_PRIM");
    for line in src.lines() {
        if let Some(rest) = line.strip_prefix("pub const FLOATS_PER_PRIM") {
            if let Some(val) = rest.split('=').nth(1) {
                let val = val.trim().trim_end_matches(';').trim();
                return val.parse::<i32>().expect("FLOATS_PER_PRIM is not a valid integer");
            }
        }
    }
    panic!("FLOATS_PER_PRIM not found in {:?}", path);
}

fn main() {
    // Only compile CUDA kernels when the `cuda` feature is active
    if env::var("CARGO_FEATURE_CUDA").is_err() {
        return;
    }

    let cuda_path = env::var("CUDA_PATH")
        .or_else(|_| env::var("CUDA_HOME"))
        .unwrap_or_else(|_| {
            // Default paths per platform — prefer newest installed toolkit
            if cfg!(windows) {
                let candidates = [
                    "C:\\Program Files\\NVIDIA GPU Computing Toolkit\\CUDA\\v12.8",
                    "C:\\Program Files\\NVIDIA GPU Computing Toolkit\\CUDA\\v12.6",
                    "C:\\Program Files\\NVIDIA GPU Computing Toolkit\\CUDA\\v12.4",
                ];
                for c in &candidates {
                    if PathBuf::from(c).join("bin").join("nvcc.exe").exists() {
                        return c.to_string();
                    }
                }
                candidates[0].to_string()
            } else {
                "/usr/local/cuda".to_string()
            }
        });

    let nvcc = PathBuf::from(&cuda_path).join("bin").join(if cfg!(windows) { "nvcc.exe" } else { "nvcc" });

    if !nvcc.exists() {
        if env::var("CARGO_FEATURE_OPTIX").is_ok() {
            panic!("OptiX requires nvcc; set CUDA_PATH or CUDA_HOME to a CUDA toolkit installation");
        }
        println!("cargo:warning=nvcc not found at {:?}. CUDA kernels will not be compiled.", nvcc);
        println!("cargo:warning=Set CUDA_PATH or CUDA_HOME to your CUDA toolkit installation.");
        return;
    }

    let kernel_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("kernels");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // List of .cu kernel files to compile to PTX
    let kernels = [
        "raymarch_gbuf",
        "deferred_shade",
        "pathtrace",
        "bloom",
        "ssr",
        "fog",
        "tonemap",
        "upsample",
    ];

    // Read FLOATS_PER_PRIM from core_math (single source of truth)
    let core_math_lib = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("..").join("core_math").join("src").join("lib.rs");
    let floats_per_prim = parse_floats_per_prim(&core_math_lib);
    println!("cargo:rerun-if-changed={}", core_math_lib.display());

    for kernel_name in &kernels {
        let cu_path = kernel_dir.join(format!("{}.cu", kernel_name));
        if !cu_path.exists() {
            // Kernel not yet created — skip silently during incremental development
            continue;
        }

        let ptx_path = out_dir.join(format!("{}.ptx", kernel_name));

        let status = std::process::Command::new(&nvcc)
            .args([
                "--ptx",
                "-O3",
                "--use_fast_math",
                "-allow-unsupported-compiler", // VS 2022+ host compilers
                "-arch=sm_50",            // Minimum: Maxwell (GTX 900 series)
                "-maxrregcount=64",
                &format!("-DFLOATS_PER_PRIM={}", floats_per_prim),
                &format!("-I{}", kernel_dir.display()),
                "-o", ptx_path.to_str().unwrap(),
                cu_path.to_str().unwrap(),
            ])
            .status()
            .expect("Failed to launch nvcc");

        if !status.success() {
            panic!("nvcc failed to compile {}.cu (exit code: {:?})", kernel_name, status.code());
        }

        println!("cargo:rerun-if-changed={}", cu_path.display());
    }

    // Rerun if any header changes
    if kernel_dir.exists() {
        for entry in std::fs::read_dir(&kernel_dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "cuh" || e == "cu") {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }

    // ================================================================
    // OptiX compilation (only when `optix` feature is active)
    // ================================================================
    if env::var("CARGO_FEATURE_OPTIX").is_ok() {
        compile_optix(&cuda_path, &nvcc, &kernel_dir, &out_dir, floats_per_prim);
    }

    println!("cargo:rerun-if-env-changed=CUDA_PATH");
    println!("cargo:rerun-if-env-changed=CUDA_HOME");
    println!("cargo:rerun-if-env-changed=OPTIX_PATH");
}

/// Compile OptiX C shim (optix_host.c) and OptiX device programs (optix_programs.cu).
fn compile_optix(
    cuda_path: &str,
    nvcc: &PathBuf,
    kernel_dir: &PathBuf,
    out_dir: &PathBuf,
    floats_per_prim: i32,
) {
    let optix_path = env::var("OPTIX_PATH").unwrap_or_else(|_| {
        // Prefer vendored headers (third_party/optix from NVIDIA/optix-dev)
        let vendored = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("..")
            .join("third_party")
            .join("optix");
        if vendored.join("include").join("optix.h").exists() {
            return vendored.to_string_lossy().into_owned();
        }
        if cfg!(windows) {
            // Common default install locations on Windows
            let candidates = [
                "C:\\ProgramData\\NVIDIA Corporation\\OptiX SDK 9.0.0",
                "C:\\ProgramData\\NVIDIA Corporation\\OptiX SDK 8.1.0",
                "C:\\ProgramData\\NVIDIA Corporation\\OptiX SDK 8.0.0",
                "C:\\ProgramData\\NVIDIA Corporation\\OptiX SDK 7.7.0",
            ];
            for c in &candidates {
                if PathBuf::from(c).join("include").join("optix.h").exists() {
                    return c.to_string();
                }
            }
            candidates[1].to_string()
        } else {
            "/usr/local/NVIDIA-OptiX-SDK-8.0.0".to_string()
        }
    });

    let cuda_include = PathBuf::from(cuda_path).join("include");
    let optix_include = PathBuf::from(&optix_path).join("include");

    if !optix_include.join("optix.h").exists() {
        panic!("OptiX SDK not found at {:?}; set OPTIX_PATH to an installation containing include/optix.h", optix_include);
    }

    // ---- Compile optix_host.c via cc crate ----
    // This produces a static library that Rust links against.
    // OptiX itself is loaded dynamically via optix_stubs.h (no static lib needed).
    let host_c = kernel_dir.join("optix_host.c");
    if host_c.exists() {
        cc::Build::new()
            .file(&host_c)
            .include(&cuda_include)
            .include(&optix_include)
            .warnings(false) // OptiX headers may produce warnings
            .compile("optix_host");
        if cfg!(windows) {
            println!("cargo:rustc-link-lib=advapi32");
        }

        println!("cargo:rerun-if-changed={}", host_c.display());
    } else {
        println!("cargo:warning=optix_host.c not found, skipping OptiX C shim compilation");
    }

    // ---- Compile OptiX device programs → PTX via nvcc ----
    // Uses sm_75 minimum (RTX 2000 series) since OptiX RT cores require Turing+.
    //
    let optix_kernels = ["optix_query_tri"];
    for kernel_name in &optix_kernels {
        let cu_path = kernel_dir.join(format!("{}.cu", kernel_name));
        if !cu_path.exists() {
            panic!("required OptiX kernel {}.cu not found", kernel_name);
        }
        let ptx_path = out_dir.join(format!("{}.ptx", kernel_name));

        let status = std::process::Command::new(nvcc)
            .args([
                "--ptx",
                "-O3",
                "-allow-unsupported-compiler",
                "-arch=sm_75", // Minimum: Turing (RTX 2000 series)
                "-maxrregcount=64",
                &format!("-DFLOATS_PER_PRIM={}", floats_per_prim),
                &format!("-I{}", kernel_dir.display()),
                &format!("-I{}", optix_include.display()),
                "-o",
                ptx_path.to_str().unwrap(),
                cu_path.to_str().unwrap(),
            ])
            .status()
            .unwrap_or_else(|e| panic!("Failed to launch nvcc for {}.cu: {}", kernel_name, e));

        if !status.success() {
            panic!(
                "nvcc failed to compile {}.cu (exit code: {:?})",
                kernel_name,
                status.code()
            );
        }

        println!("cargo:rerun-if-changed={}", cu_path.display());
        println!("cargo:warning=Compiled OptiX kernel {}.ptx", kernel_name);
    }
}
