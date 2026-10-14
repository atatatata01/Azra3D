fn main() {
    let path = std::env::args_os().nth(1).expect("expected WGSL output path");
    std::fs::write(path, graphics::build_pathtrace_tri_wgsl()).unwrap();
}
