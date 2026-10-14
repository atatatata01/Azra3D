# ai_backend/server/texture_server.py — FourthLayer Texture Server v3
#
# Architecture:
#   Stage 1 — Albedo: SD generation with quality-tuned prompts
#   Stage 2 — Derived maps: physically-motivated derivation (preview/standard)
#              or independent SD generation per map (final preset)
#   Stage 3 — Tileability: cosine-blend seam removal
#
# Endpoints:
#   GET  /health           — status, loaded model, DB stats
#   GET  /models           — list available models
#   POST /switch_model     — hot-swap loaded model (blocking)
#   POST /generate         — full PBR set, returns thumbs + PNG + RGBA for GPU
#   POST /generate_map     — single map regeneration
#   POST /rate             — rate a generation (1-4 stars) → SQLite DB
#   GET  /db_stats         — generation history counts
#
# Run:  python texture_server.py [--model dreamshaper-8] [--port 7860]

import io, os, sys, json, base64, time, sqlite3, argparse, threading
from pathlib import Path

try:
    from fastapi import FastAPI
    from fastapi.responses import JSONResponse
    import uvicorn
except ImportError:
    print("ERROR: pip install fastapi uvicorn"); sys.exit(1)

app = FastAPI(title="FourthLayer Texture Server v4")
pipe = None
device = "cuda"
current_model_key = ""
# "sd15" | "sdxl" — drives img2img / generate pipeline choice
current_model_arch = "sd15"
_model_lock = threading.Lock()

# ============================================================
# Model Registry
# Prefer LOCAL ComfyUI single-file checkpoints when present (no re-download).
# arch: "sd15" | "sdxl"
# "path" = single .safetensors/.ckpt  |  "id" = HuggingFace repo (fallback)
# ============================================================
_COMFY_CKPT = Path(r"C:\Users\Proprio\Documents\ComfyUI\models\checkpoints")

def _local(*parts: str) -> str:
    p = _COMFY_CKPT.joinpath(*parts)
    return str(p) if p.is_file() else ""

MODELS = {
    # ── Flux.1 Kontext (PRIMARY — local ComfyUI diffusion_models) ──
    "flux-kontext": {
        "path": str(
            Path(r"C:\Users\Proprio\Documents\ComfyUI\models\diffusion_models\flux1-kontext-dev.safetensors")
        ),
        "id": "black-forest-labs/FLUX.1-Kontext-dev",
        "label": "Flux.1 Kontext ★ AZRA PRIMARY",
        "arch": "flux_kontext",
        "stars": 5, "speed": 1, "vram_gb": 16,
        "desc": "LOCAL transformer — context-aware polish + high-end textures (ComfyUI weights)",
    },
    # ── Local SDXL (you already have these — no download) ──
    "cyberrealistic-xl": {
        "path": _local("cyberrealisticXL_v60.safetensors"),
        "id": "",  # local only
        "label": "CyberRealistic XL v6 ★ LOCAL SDXL",
        "arch": "sdxl",
        "stars": 5, "speed": 2, "vram_gb": 8,
        "desc": "LOCAL — photoreal hard-surface / product (fallback if Flux VRAM tight)",
    },
    "juggernaut-xl": {
        "path": _local("juggernautXL_ragnarokBy.safetensors"),
        "id": "RunDiffusion/Juggernaut-XL-v9",
        "label": "Juggernaut XL Ragnarok ★ LOCAL",
        "arch": "sdxl",
        "stars": 5, "speed": 2, "vram_gb": 8,
        "desc": "LOCAL — top general SDXL (products, metals, leather)",
    },
    "fiamix-xl": {
        "path": _local("fiamixXL_v6000Overhaul.safetensors"),
        "id": "",
        "label": "FiaMix XL v6 LOCAL",
        "arch": "sdxl",
        "stars": 4, "speed": 2, "vram_gb": 8,
        "desc": "LOCAL — strong general XL mix",
    },
    "dreamshaper-xl": {
        "path": _local("dreamshaperXL_v21TurboDPMSDE.safetensors"),
        "id": "",
        "label": "DreamShaper XL Turbo LOCAL",
        "arch": "sdxl",
        "stars": 3, "speed": 4, "vram_gb": 8,
        "desc": "LOCAL — fast turbo XL (speed > max quality)",
    },
    "sdxl-base": {
        "path": _local("sd_xl_base_1.0.safetensors"),
        "id": "stabilityai/stable-diffusion-xl-base-1.0",
        "label": "SDXL Base 1.0 LOCAL",
        "arch": "sdxl",
        "stars": 3, "speed": 2, "vram_gb": 8,
        "desc": "LOCAL — stock SDXL base (fine-tunes above are better)",
    },
    # ── Local SD1.5 ──
    "dreamshaper-8": {
        "path": _local("dreamshaper_8.safetensors") or _local("DreamShaper_8_pruned.safetensors"),
        "id": "Lykon/dreamshaper-8",
        "label": "DreamShaper 8 LOCAL",
        "arch": "sd15",
        "stars": 3, "speed": 3, "vram_gb": 4,
        "desc": "LOCAL — old default, weaker than your XL checkpoints",
    },
    "sd15": {
        "path": _local("v1-5-pruned-emaonly-fp16.safetensors"),
        "id": "runwayml/stable-diffusion-v1-5",
        "label": "SD 1.5 base LOCAL",
        "arch": "sd15",
        "stars": 2, "speed": 3, "vram_gb": 4,
        "desc": "LOCAL stock 1.5 — only for debugging",
    },
    # ── HF download only if you want them (optional) ──
    "realvis-xl": {
        "path": "",
        "id": "SG161222/RealVisXL_V4.0",
        "label": "RealVisXL V4 (download)",
        "arch": "sdxl",
        "stars": 5, "speed": 2, "vram_gb": 8,
        "desc": "Optional HF download — similar class to CyberRealistic XL you already have",
    },
    "epicrealism": {
        "path": "",
        "id": "emilianJR/epiCRealism",
        "label": "epiCRealism (download)",
        "arch": "sd15",
        "stars": 4, "speed": 3, "vram_gb": 5,
        "desc": "Optional HF download — only if you need SD1.5 photoreal",
    },
    "realistic-vision": {
        "path": "",
        "id": "SG161222/Realistic_Vision_V5.1_noVAE",
        "label": "Realistic Vision 5.1 (download)",
        "arch": "sd15",
        "stars": 4, "speed": 3, "vram_gb": 4,
        "desc": "Optional HF download",
    },
    "small-sd": {
        "path": "",
        "id": "OFA-Sys/small-stable-diffusion-v0",
        "label": "Small SD (test)",
        "arch": "sd15",
        "stars": 1, "speed": 4, "vram_gb": 2,
        "desc": "Test only",
    },
}

# Drop entries that have neither a local file nor an HF id
# flux-kontext is always kept if discover says ready (path may be multi-file)
def _model_available(k: str, v: dict) -> bool:
    if k == "flux-kontext":
        try:
            from flux_kontext import discover_kontext_weights
            return discover_kontext_weights().get("ready", False) or bool(v.get("id"))
        except Exception:
            return bool(v.get("path") and Path(v["path"]).is_file()) or bool(v.get("id"))
    return bool(v.get("path") and Path(v["path"]).is_file()) or bool(v.get("id"))

MODELS = {k: v for k, v in MODELS.items() if _model_available(k, v)}

# ============================================================
# Quality Presets
# ============================================================
# IMPORTANT: gen_all=True runs SD once PER map (5× gens) → minutes on XL.
# Default is always albedo once + derive PBR maps (what people expect ~10s on 4090).
QUALITY_PRESETS = {
    "preview":  {"size": 768,  "steps": 12, "guidance": 6.5, "gen_all": False},
    "standard": {"size": 1024, "steps": 18, "guidance": 7.0, "gen_all": False},
    "final":    {"size": 1024, "steps": 24, "guidance": 7.5, "gen_all": False},
    "ultra":    {"size": 1280, "steps": 28, "guidance": 7.5, "gen_all": False},
}
MAX_TEX_SIZE = 1536

# ============================================================
# Physically-motivated PBR prompts
# ============================================================
MAP_PROMPTS = {

    "albedo": {
        "prefix": (
            "seamless tileable pbr albedo diffuse color texture, perfectly flat 2D material, "
            "perfectly flat even lighting, absolutely no shadows no ambient occlusion "
            "no specular highlights no reflections, pure surface color information only, "
        ),
        "suffix": (
            ", top-down orthographic, game asset texture, 8k photorealistic, "
            "material surface detail, no baked lighting whatsoever"
        ),
        "negative": (
            "person, woman, girl, man, character, portrait, face, body, human, anatomy, "
            "shadows, ambient occlusion, baked shading, specular highlights, reflections, "
            "glare, normal map effect, depth of field, perspective distortion, "
            "text, watermark, blurry, gradient, vignette, dark borders, 3d render, caustics"
        ),
    },
    "normal": {
        "prefix": (
            "tangent-space normal map texture, flat surface base color rgb(128,128,255) "
            "blue-purple, raised bumps lighter blue, recessed cavities darker, "
            "surface micro-detail relief, "
        ),
        "suffix": ", seamless tileable pbr normal map, blue-purple dominant, surface bump detail",
        "negative": (
            "text, watermark, faces, saturated red tones, yellow, green, "
            "shadows, specular, gloss, albedo color"
        ),
    },
    "roughness": {
        "prefix": (
            "pbr roughness map grayscale texture, "
            "pure white 255=fully rough matte unpolished surface, "
            "pure black 0=perfectly smooth mirror-polished surface, "
            "surface micro-roughness variation, "
        ),
        "suffix": ", seamless tileable, grayscale only, roughness map, no color",
        "negative": "text, watermark, faces, color, saturated, red, blue, green",
    },
    "height": {
        "prefix": (
            "pbr heightmap displacement grayscale texture, "
            "white=raised elevated geometry, black=recessed cavity, "
            "surface micro relief, subtle elevation variation, "
        ),
        "suffix": ", seamless tileable, grayscale heightmap, displacement map, no color",
        "negative": "text, watermark, faces, color, saturated, completely flat uniform",
    },
    "metalness": {
        "prefix": (
            "pbr metalness mask grayscale texture, "
            "pure white 255=metallic conductor (steel, iron, gold), "
            "pure black 0=non-metallic dielectric (paint, plastic, stone, wood), "
            "sharp binary material boundaries, "
        ),
        "suffix": ", seamless tileable, metallic mask, binary or near-binary grayscale",
        "negative": "text, watermark, faces, uniform mid-grey, gradual gradient everywhere, color",
    },
}


def build_prompt(user_prompt: str, map_type: str):
    info = MAP_PROMPTS.get(map_type, MAP_PROMPTS["albedo"])
    return info["prefix"] + user_prompt + info["suffix"], info["negative"]


# ============================================================
# SQLite generation history + rating
# ============================================================
DB_PATH = Path("fourthLayer_generations.db")


def init_db():
    with sqlite3.connect(DB_PATH) as conn:
        conn.execute("""
            CREATE TABLE IF NOT EXISTS generations (
                id        INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT    DEFAULT (datetime('now')),
                prompt    TEXT,
                model     TEXT,
                seed      INTEGER,
                steps     INTEGER,
                guidance  REAL,
                size      INTEGER,
                preset    TEXT,
                elapsed   REAL,
                rating    INTEGER DEFAULT NULL
            )
        """)
        conn.commit()


def db_save_gen(prompt, model, seed, steps, guidance, size, preset, elapsed) -> int:
    with sqlite3.connect(DB_PATH) as conn:
        cur = conn.execute(
            "INSERT INTO generations "
            "(prompt,model,seed,steps,guidance,size,preset,elapsed) "
            "VALUES (?,?,?,?,?,?,?,?)",
            (prompt, model, seed, steps, guidance, size, preset, round(elapsed, 2)),
        )
        conn.commit()
        return cur.lastrowid


def db_rate(gen_id: int, rating: int):
    with sqlite3.connect(DB_PATH) as conn:
        conn.execute("UPDATE generations SET rating=? WHERE id=?", (rating, gen_id))
        conn.commit()


def db_stats():
    try:
        with sqlite3.connect(DB_PATH) as conn:
            total  = conn.execute("SELECT COUNT(*) FROM generations").fetchone()[0]
            liked  = conn.execute("SELECT COUNT(*) FROM generations WHERE rating >= 3").fetchone()[0]
            return {"total": total, "liked": liked}
    except Exception:
        return {"total": 0, "liked": 0}


# ============================================================
# Model loading / hot-swap
# ============================================================
def load_model(model_key: str) -> bool:
    """Load Flux Kontext / SD1.5 / SDXL from local ComfyUI weights or HuggingFace."""
    global pipe, device, current_model_key, current_model_arch
    info = MODELS.get(model_key)
    if not info:
        print(f"Unknown model key: {model_key}")
        print(f"Available: {', '.join(MODELS.keys())}")
        return False
    local_path = (info.get("path") or "").strip()
    model_id = (info.get("id") or "").strip()
    arch = info.get("arch", "sd15")
    use_local = bool(local_path) and Path(local_path).is_file()
    if arch != "flux_kontext" and not use_local and not model_id:
        print(f"No local file and no HF id for {model_key}")
        return False
    try:
        import torch
        from diffusers import StableDiffusionPipeline, StableDiffusionXLPipeline

        if torch.cuda.is_available():
            device = "cuda"
            dtype = torch.float16
        else:
            device = "cpu"
            dtype = torch.float32
            print("WARNING: No CUDA -- using CPU (slow)")

        os.environ.pop("HF_TOKEN", None)
        os.environ.pop("HUGGING_FACE_HUB_TOKEN", None)

        if pipe is not None:
            try:
                # Move off GPU before delete so VRAM actually frees (esp. after Flux).
                if hasattr(pipe, "to"):
                    pipe.to("cpu")
            except Exception:
                pass
            del pipe
            pipe = None
            if torch.cuda.is_available():
                import gc
                torch.cuda.empty_cache()
                torch.cuda.ipc_collect()
                gc.collect()
                free = torch.cuda.mem_get_info()[0] / (1024 ** 3)
                print(f"[load_model] freed GPU — ~{free:.1f} GB free before load")

        # ── Flux.1 Kontext (heavy; not the 10s path) ─────────────────
        if arch == "flux_kontext":
            from flux_kontext import load_flux_kontext_pipeline

            print(f"Loading {info['label']} (Flux.1 Kontext pipeline) …")
            pipe, device, note = load_flux_kontext_pipeline(prefer_offload=True)
            current_model_key = model_key
            current_model_arch = "flux_kontext"
            print(f"[OK] {info['label']} loaded on {device} ({note})")
            return True

        src = local_path if use_local else model_id
        print(f"Loading {info['label']} from {'LOCAL' if use_local else 'HF'}: {src}")
        print(f"  arch={arch} device={device} dtype={dtype}")

        # SDXL / SD1.5 — full CUDA (4090 can hold XL easily; NO cpu_offload).
        if use_local:
            if arch == "sdxl":
                pipe = StableDiffusionXLPipeline.from_single_file(
                    local_path,
                    torch_dtype=dtype,
                    use_safetensors=True,
                    variant=None,
                )
            else:
                pipe = StableDiffusionPipeline.from_single_file(
                    local_path,
                    torch_dtype=dtype,
                    use_safetensors=True,
                    safety_checker=None,
                    requires_safety_checker=False,
                )
        else:
            common = dict(torch_dtype=dtype, use_safetensors=True, token=False)
            if arch == "sdxl":
                pipe = StableDiffusionXLPipeline.from_pretrained(model_id, **common)
            else:
                pipe = StableDiffusionPipeline.from_pretrained(
                    model_id,
                    safety_checker=None,
                    requires_safety_checker=False,
                    **common,
                )

        # Fast scheduler: DPM++ 2M Karras — good quality at 15–20 steps (web ~10s path).
        try:
            from diffusers import DPMSolverMultistepScheduler
            pipe.scheduler = DPMSolverMultistepScheduler.from_config(
                pipe.scheduler.config,
                use_karras_sigmas=True,
                algorithm_type="dpmsolver++",
            )
            print("[load_model] scheduler = DPMSolverMultistep++ Karras")
        except Exception as e:
            print(f"[load_model] scheduler keep default ({e})")

        if device == "cuda":
            torch.backends.cuda.matmul.allow_tf32 = True
            torch.backends.cudnn.allow_tf32 = True
            try:
                torch.set_float32_matmul_precision("high")
            except Exception:
                pass
            pipe = pipe.to("cuda")
            # Prefer fused SDPA (PyTorch 2.x) over xformers when available
            try:
                pipe.enable_xformers_memory_efficient_attention()
                print("[load_model] xformers attention ON")
            except Exception:
                try:
                    pipe.enable_attention_slicing(1)
                except Exception:
                    pass
            # VAE tiling only helps huge res; hurts 1024 speed — leave off by default
            try:
                pipe.vae.enable_slicing()
            except Exception:
                pass
            # Warmup one tiny step so first real gen isn't cold-compile slow
            try:
                with torch.inference_mode():
                    _ = pipe(
                        prompt="warmup",
                        num_inference_steps=1,
                        width=512,
                        height=512,
                        guidance_scale=1.0,
                    )
                torch.cuda.empty_cache()
                print("[load_model] CUDA warmup done")
            except Exception as e:
                print(f"[load_model] warmup skip: {e}")

            free = torch.cuda.mem_get_info()[0] / (1024 ** 3)
            print(f"[load_model] VRAM free after load: {free:.1f} GB")
        else:
            pipe = pipe.to("cpu")

        current_model_key = model_key
        current_model_arch = arch
        print(f"[OK] {info['label']} loaded on {device} (arch={arch}, local={use_local}) — expect ~10s @ 1024/18 on 4090")
        return True
    except Exception as e:
        print(f"Load failed: {e}")
        import traceback
        traceback.print_exc()
        return False


# ============================================================
# Tileability
# ============================================================
def make_tileable(img):
    import numpy as np
    from PIL import Image
    arr = np.array(img, dtype=np.float32)
    h, w = arr.shape[:2]
    blend = w // 4
    result = arr.copy()
    for x in range(blend):
        t = x / blend
        t = t * t * (3 - 2 * t)
        result[:, x]         = arr[:, x] * t         + arr[:, w - blend + x] * (1 - t)
        result[:, w - 1 - x] = arr[:, w - 1 - x] * t + arr[:, blend - 1 - x] * (1 - t)
    for y in range(blend):
        t = y / blend
        t = t * t * (3 - 2 * t)
        result[y]         = result[y] * t         + result[h - blend + y] * (1 - t)
        result[h - 1 - y] = result[h - 1 - y] * t + result[blend - 1 - y] * (1 - t)
    return Image.fromarray(result.clip(0, 255).astype(np.uint8))


# ============================================================
# Physically-motivated map derivation
#   Used for preview preset (fast) and standard (normal+height derived,
#   roughness/metalness derived).  Final preset generates all with SD.
# ============================================================
def derive_normal(albedo_img, strength: float = 3.0):
    """Tangent-space normal from luminance gradient (Gaussian-smoothed Sobel)."""
    import numpy as np
    from PIL import Image
    from scipy.ndimage import gaussian_filter, sobel

    arr  = np.array(albedo_img.convert("RGB"), dtype=np.float32) / 255.0
    lum  = 0.299 * arr[:, :, 0] + 0.587 * arr[:, :, 1] + 0.114 * arr[:, :, 2]
    lum  = gaussian_filter(lum, sigma=1.0)          # reduce noise before gradient
    dx   = -sobel(lum, axis=1) * strength
    dy   = -sobel(lum, axis=0) * strength
    h, w = lum.shape
    n    = np.stack([-dx, -dy, np.ones((h, w), dtype=np.float32)], axis=2)
    n   /= (np.linalg.norm(n, axis=2, keepdims=True) + 1e-8)
    return Image.fromarray(((n * 0.5 + 0.5) * 255).clip(0, 255).astype(np.uint8))


def derive_roughness(albedo_img):
    """
    Roughness from local luminance variance + inverse luminance.
    High-variance areas (scratches, wear) → rough.
    Bright smooth areas → smoother.
    """
    import numpy as np
    from PIL import Image
    from scipy.ndimage import gaussian_filter

    arr  = np.array(albedo_img.convert("RGB"), dtype=np.float32) / 255.0
    lum  = 0.299 * arr[:, :, 0] + 0.587 * arr[:, :, 1] + 0.114 * arr[:, :, 2]
    mean = gaussian_filter(lum, sigma=3.0)
    var  = gaussian_filter((lum - mean) ** 2, sigma=3.0)
    rough = (0.5 + (0.5 - lum * 0.35) + np.sqrt(var.clip(0, 1)) * 2.5).clip(0.05, 1.0)
    return Image.fromarray((rough * 255).clip(0, 255).astype(np.uint8), mode="L")


def derive_height(albedo_img):
    """Height from perceptual luminance with contrast stretch and mild gamma."""
    import numpy as np
    from PIL import Image

    arr  = np.array(albedo_img.convert("RGB"), dtype=np.float32) / 255.0
    lum  = 0.299 * arr[:, :, 0] + 0.587 * arr[:, :, 1] + 0.114 * arr[:, :, 2]
    mn, mx = lum.min(), lum.max()
    h    = (lum - mn) / (mx - mn + 1e-8)
    h    = h ** 0.7   # mild gamma to push midtones up
    return Image.fromarray((h * 255).clip(0, 255).astype(np.uint8), mode="L")


def derive_metalness(albedo_img):
    """
    Metalness from saturation + luminance.
    Low-saturation, mid-luminance areas are likely metallic.
    Threshold + soften for a cleaner binary-ish mask.
    """
    import numpy as np
    from PIL import Image
    from scipy.ndimage import gaussian_filter

    arr = np.array(albedo_img.convert("RGB"), dtype=np.float32) / 255.0
    r, g, b = arr[:, :, 0], arr[:, :, 1], arr[:, :, 2]
    mx  = np.maximum(np.maximum(r, g), b)
    mn  = np.minimum(np.minimum(r, g), b)
    sat = (mx - mn) / (mx + 1e-8)
    lum = 0.299 * r + 0.587 * g + 0.114 * b
    # Metal signal: low saturation AND medium luminance (not pure black/white)
    metal = ((1.0 - sat) * (1.0 - (lum - 0.4).clip(0, 1) * 2.0)).clip(0, 1)
    metal = (metal > 0.45).astype(np.float32)
    metal = gaussian_filter(metal, sigma=1.5)  # soften sharp mask edges
    return Image.fromarray((metal * 255).clip(0, 255).astype(np.uint8), mode="L")


# ============================================================
# Base64 helpers
# ============================================================
# NOUVEAU CODE :
def img_to_b64_rgba(img):
    """Full-resolution RGBA packed (for GPU buffer)."""
    img_rgba = img.convert("RGBA")
    h, w = img_rgba.height, img_rgba.width
    return base64.b64encode(img_rgba.tobytes()).decode("ascii"), w, h

def img_to_thumb_b64_rgba(img, thumb_size: int = 128):
    """128×128 RGBA packed for UI preview thumbnail."""
    from PIL import Image
    small = img.resize((thumb_size, thumb_size), Image.LANCZOS)
    b64, _, _ = img_to_b64_rgba(small)
    return b64


def img_to_b64_png(img):
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    return base64.b64encode(buf.getvalue()).decode("ascii")


# ============================================================
# Core generation
# ============================================================
def _generate_raw(prompt, negative, width, height, seed, steps, guidance):
    """Run the loaded pipe (Flux Kontext / SD1.5 / SDXL)."""
    import torch

    # ── Flux.1 Kontext (context canvas → material) ───────────────────
    if current_model_arch == "flux_kontext":
        from flux_kontext import run_flux_kontext, _empty_cuda

        # Cap res/steps for 24GB + offload — push quality via steps only up to 28.
        w = max(256, (int(width) // 16) * 16)
        h = max(256, (int(height) // 16) * 16)
        max_side = 1280
        if max(w, h) > max_side:
            scale = max_side / float(max(w, h))
            w = max(256, (int(round(w * scale)) // 16) * 16)
            h = max(256, (int(round(h * scale)) // 16) * 16)
        if max(w, h) < 768:
            scale = 1024.0 / float(max(w, h))
            w = max(256, (int(round(w * scale)) // 16) * 16)
            h = max(256, (int(round(h * scale)) // 16) * 16)
        steps = int(min(int(steps), 28))
        _empty_cuda()
        img = run_flux_kontext(
            pipe,
            prompt=prompt,
            image=None,
            width=w,
            height=h,
            seed=seed,
            steps=steps,
            guidance=guidance,
            negative_prompt=negative or "",
            device=device,
        )
        if img.size != (width, height) and (int(width), int(height)) != (w, h):
            from PIL import Image
            img = img.resize((int(width), int(height)), Image.Resampling.LANCZOS)
        return make_tileable(img)

    t0 = time.time()
    # Always generate on CUDA when available — never silent CPU fallback.
    use_cuda = device == "cuda" and torch.cuda.is_available()
    if not use_cuda:
        print("[WARN] generating on CPU — will be minutes, not ~10s")
    gen_dev = "cuda" if use_cuda else "cpu"
    gen = torch.Generator(device=gen_dev).manual_seed(int(seed) & 0xFFFFFFFF)
    w = max(256, (int(width) // 8) * 8)
    h = max(256, (int(height) // 8) * 8)
    # Cap for speed: 4090 SDXL sweet spot is 1024; >1280 gets expensive
    if current_model_arch == "sdxl":
        if max(w, h) > 1280:
            scale = 1280.0 / float(max(w, h))
            w = max(256, (int(round(w * scale)) // 8) * 8)
            h = max(256, (int(round(h * scale)) // 8) * 8)
        if max(w, h) < 768:
            scale = 1024.0 / float(max(w, h))
            w = max(256, (int(round(w * scale)) // 8) * 8)
            h = max(256, (int(round(h * scale)) // 8) * 8)
        # 15–24 steps is enough with DPM++ Karras; clamp runaway client values
        steps = int(min(max(int(steps), 8), 30))
        kwargs = dict(
            prompt=prompt,
            negative_prompt=negative,
            width=w,
            height=h,
            num_inference_steps=steps,
            guidance_scale=guidance,
            generator=gen,
        )
        print(f"[sdxl] denoise {w}x{h} steps={steps} guidance={guidance} …")
        with torch.inference_mode():
            try:
                result = pipe(**kwargs)
            except TypeError:
                result = pipe(
                    prompt=prompt,
                    prompt_2=prompt,
                    negative_prompt=negative,
                    negative_prompt_2=negative,
                    width=w, height=h,
                    num_inference_steps=steps,
                    guidance_scale=guidance,
                    generator=gen,
                )
    else:
        steps = int(min(max(int(steps), 8), 40))
        print(f"[sd15] denoise {w}x{h} steps={steps} …")
        with torch.inference_mode():
            result = pipe(
                prompt=prompt, negative_prompt=negative,
                width=w, height=h,
                num_inference_steps=steps, guidance_scale=guidance,
                generator=gen,
            )
    img = result.images[0]
    if img.size != (width, height) and (width, height) != (w, h):
        from PIL import Image
        img = img.resize((int(width), int(height)), Image.Resampling.LANCZOS)
    img = make_tileable(img)
    print(f"[gen] single map {w}x{h} steps={steps} in {time.time()-t0:.2f}s on {gen_dev}")
    return img


def generate_all_maps(prompt, width, height, seed, steps, guidance, gen_all: bool) -> dict:
    """
    Returns dict {albedo, normal, roughness, height, metalness} of PIL images.
    gen_all=False → derive all non-albedo maps (fast, preview/standard presets).
    gen_all=True  → independent SD generation for every map (slow, final preset).
    """
    full_p, neg = build_prompt(prompt, "albedo")
    albedo = _generate_raw(full_p, neg, width, height, seed, steps, guidance)

    if gen_all:
        maps = {"albedo": albedo}
        for mtype in ("normal", "roughness", "height", "metalness"):
            mp, mn = build_prompt(prompt, mtype)
            # Offset seed per map so maps don't correlate
            mseed = (seed + hash(mtype) % 9999) & 0x7FFFFFFF
            img = _generate_raw(mp, mn, width, height, mseed, steps, guidance)
            if mtype in ("roughness", "height", "metalness"):
                import numpy as np
                from PIL import Image
                arr = np.array(img)
                gray = (0.299 * arr[:, :, 0] + 0.587 * arr[:, :, 1] + 0.114 * arr[:, :, 2]).astype(np.uint8)
                img = Image.fromarray(gray, mode="L")
            maps[mtype] = img
    else:
        maps = {
            "albedo":    albedo,
            "normal":    derive_normal(albedo),
            "roughness": derive_roughness(albedo),
            "height":    derive_height(albedo),
            "metalness": derive_metalness(albedo),
        }
    return maps


# ============================================================
# Endpoints
# ============================================================
@app.get("/health")
def health():
    info = MODELS.get(current_model_key, {})
    flux_weights = {}
    try:
        from flux_kontext import discover_kontext_weights
        flux_weights = discover_kontext_weights()
    except Exception:
        pass
    return {
        "status":       "ok" if pipe is not None else "no_model",
        "model_loaded": pipe is not None,
        "model_key":    current_model_key,
        "model_label":  info.get("label", ""),
        "model_stars":  info.get("stars", 0),
        "model_speed":  info.get("speed", 0),
        "arch":         current_model_arch,
        "device":       device,
        "pipeline":     "azra_flux_kontext" if current_model_arch == "flux_kontext" else "azra_sd",
        "flux_kontext": flux_weights,
        "db":           db_stats(),
    }


@app.get("/models")
def list_models():
    return {
        "current": current_model_key,
        "arch": current_model_arch,
        "models": [
            {
                "key":     k,
                "label":   v["label"],
                "desc":    v["desc"],
                "stars":   v["stars"],
                "speed":   v["speed"],
                "vram_gb": v["vram_gb"],
                "arch":    v.get("arch", "sd15"),
                "loaded":  (k == current_model_key and pipe is not None),
            }
            for k, v in MODELS.items()
        ],
    }


@app.post("/switch_model")
async def switch_model(request: dict):
    model_key = request.get("model_key", "dreamshaper-8")
    if model_key == current_model_key and pipe is not None:
        return {"status": "ok", "message": "Already loaded", "model_key": model_key,
                "label": MODELS.get(model_key, {}).get("label", "")}
    with _model_lock:
        ok = load_model(model_key)
    if ok:
        return {"status": "ok", "model_key": current_model_key,
                "label": MODELS[current_model_key]["label"]}
    return JSONResponse(status_code=500, content={"error": f"Failed to load {model_key}"})


@app.post("/generate")
async def generate(request: dict):
    if pipe is None:
        return JSONResponse(status_code=503, content={"error":
            "Model not loaded — install torch+diffusers: "
            "pip install torch --index-url https://download.pytorch.org/whl/cu118 "
            "&& pip install diffusers transformers accelerate"})

    prompt      = request.get("prompt", "rusty metal surface")
    preset_name = request.get("quality_preset", "standard")
    preset      = QUALITY_PRESETS.get(preset_name, QUALITY_PRESETS["standard"])

    width    = min(int(request.get("width",            preset["size"])),  MAX_TEX_SIZE)
    height   = min(int(request.get("height",           preset["size"])),  MAX_TEX_SIZE)
    # Snap to multiples of 8 (SD latent requirement)
    width  = max(256, (width  // 8) * 8)
    height = max(256, (height // 8) * 8)
    seed     = request.get("seed",                 42)
    steps    = request.get("steps",                preset["steps"])
    guidance = request.get("guidance_scale",       preset["guidance"])
    # Only explicit true runs 5× full SD (slow). Default = one albedo + derive maps (~10s).
    gen_all = request.get("generate_all_maps") is True

    # Optional: auto-enhance the prompt via Ollama before SD generation
    if request.get("auto_enhance", False):
        try:
            prompt = ollama_enhance_prompt(prompt)
        except Exception:
            pass  # Silently fall back to original prompt if Ollama unavailable

    print(
        f"[/generate] model={current_model_key} arch={current_model_arch} "
        f"{width}x{height} steps={steps} gen_all={gen_all} preset={preset_name}"
    )
    t0   = time.time()
    maps = generate_all_maps(prompt, width, height, seed, steps, guidance, gen_all)
    elapsed = time.time() - t0
    print(f"[/generate] DONE in {elapsed:.2f}s (albedo + derived maps)")

    # Persist to disk
    out_dir = Path("generated_textures")
    out_dir.mkdir(exist_ok=True)
    prefix = f"tex_{seed}"
    for name, img in maps.items():
        img.save(out_dir / f"{prefix}_{name}.png")

    # Log to DB
    gen_id = db_save_gen(prompt, current_model_key, seed, steps, guidance, width, preset_name, elapsed)

    return _pack_maps_response(maps, seed, preset_name, gen_id, elapsed)


def _pack_maps_response(maps: dict, seed, preset_name, gen_id, elapsed) -> dict:
    albedo_rgba, w, h = img_to_b64_rgba(maps["albedo"])
    return {
        "status": "ok", "elapsed": round(elapsed, 2),
        "width": w, "height": h, "seed": seed,
        "gen_id": gen_id, "preset": preset_name,
        "albedo_rgba_b64":      albedo_rgba,
        "albedo_thumb_b64":     img_to_thumb_b64_rgba(maps["albedo"]),
        "normal_thumb_b64":     img_to_thumb_b64_rgba(maps["normal"]),
        "roughness_thumb_b64":  img_to_thumb_b64_rgba(maps["roughness"]),
        "metalness_thumb_b64":  img_to_thumb_b64_rgba(maps["metalness"]),
        "height_thumb_b64":     img_to_thumb_b64_rgba(maps["height"]),
        "albedo_png_b64":       img_to_b64_png(maps["albedo"]),
        "normal_png_b64":       img_to_b64_png(maps["normal"]),
        "roughness_png_b64":    img_to_b64_png(maps["roughness"]),
        "metalness_png_b64":    img_to_b64_png(maps["metalness"]),
        "height_png_b64":       img_to_b64_png(maps["height"]),
        "db": db_stats(),
    }


def _decode_mask_b64(mask_b64: str, size: tuple) -> "Image.Image":
    """Decode a PNG/RGBA mask; white = regenerate, black = keep."""
    from PIL import Image
    raw = base64.b64decode(mask_b64)
    m = Image.open(io.BytesIO(raw)).convert("L").resize(size, Image.LANCZOS)
    return m


def _composite_with_mask(base_img, new_img, mask_l):
    """Soft-composite new_img over base_img where mask is white."""
    import numpy as np
    from PIL import Image
    base = np.array(base_img.convert("RGB"), dtype=np.float32)
    new  = np.array(new_img.convert("RGB"), dtype=np.float32)
    m    = np.array(mask_l, dtype=np.float32) / 255.0
    if m.ndim == 2:
        m = m[:, :, None]
    out = base * (1.0 - m) + new * m
    return Image.fromarray(out.clip(0, 255).astype(np.uint8))


@app.post("/generate_map")
async def generate_map(request: dict):
    if pipe is None:
        return JSONResponse(status_code=503, content={"error": "Model not loaded"})

    prompt      = request.get("prompt", "rusty metal surface")
    map_type    = request.get("map_type", "albedo")
    preset_name = request.get("quality_preset", "standard")
    preset      = QUALITY_PRESETS.get(preset_name, QUALITY_PRESETS["standard"])

    width    = min(int(request.get("width",       preset["size"])), MAX_TEX_SIZE)
    height   = min(int(request.get("height",      preset["size"])), MAX_TEX_SIZE)
    width  = max(256, (width  // 8) * 8)
    height = max(256, (height // 8) * 8)
    seed     = request.get("seed",            42)
    steps    = request.get("steps",           preset["steps"])
    guidance = request.get("guidance_scale",  preset["guidance"])
    # Material-mask regen: white regions of mask_b64 get a new generation blended in.
    # base_*_b64 = previous map to keep under black mask regions.
    mask_b64 = request.get("mask_b64")
    base_b64 = request.get("base_png_b64") or request.get("base_b64")

    if map_type not in MAP_PROMPTS:
        return JSONResponse(status_code=400, content={"error": f"Unknown map type: {map_type}"})

    t0 = time.time()
    full_p, neg = build_prompt(prompt, map_type)
    img = _generate_raw(full_p, neg, width, height, seed, steps, guidance)
    elapsed = time.time() - t0

    if map_type in ("roughness", "height", "metalness"):
        import numpy as np
        from PIL import Image
        arr  = np.array(img)
        gray = (0.299 * arr[:, :, 0] + 0.587 * arr[:, :, 1] + 0.114 * arr[:, :, 2]).astype(np.uint8)
        img  = Image.fromarray(gray, mode="L")

    # Soft mask composite: regenerate only white regions of the material mask
    if mask_b64 and base_b64:
        try:
            from PIL import Image
            base_raw = base64.b64decode(base_b64)
            base_img = Image.open(io.BytesIO(base_raw)).convert("RGB").resize((width, height), Image.LANCZOS)
            mask_l = _decode_mask_b64(mask_b64, (width, height))
            new_rgb = img.convert("RGB") if img.mode != "RGB" else img
            composed = _composite_with_mask(base_img, new_rgb, mask_l)
            if map_type in ("roughness", "height", "metalness"):
                import numpy as np
                arr = np.array(composed)
                gray = (0.299 * arr[:, :, 0] + 0.587 * arr[:, :, 1] + 0.114 * arr[:, :, 2]).astype(np.uint8)
                img = Image.fromarray(gray, mode="L")
            else:
                img = composed
        except Exception as e:
            print(f"[generate_map] mask composite failed: {e}")

    out_dir = Path("generated_textures")
    out_dir.mkdir(exist_ok=True)
    img.save(out_dir / f"tex_{seed}_{map_type}.png")

    resp: dict = {
        "status": "ok", "elapsed": round(elapsed, 2),
        "width": width, "height": height, "seed": seed, "map_type": map_type,
        f"{map_type}_thumb_b64": img_to_thumb_b64_rgba(img),
        f"{map_type}_png_b64":   img_to_b64_png(img),
    }
    if map_type == "albedo":
        rgba_b64, w, h = img_to_b64_rgba(img)
        resp["albedo_rgba_b64"] = rgba_b64
        resp["width"] = w
        resp["height"] = h
    return resp


@app.post("/generate_multi")
async def generate_multi(request: dict):
    """
    Per-object / multi-material generation for node workflows.
    Body:
      {
        "targets": [
          {"id": "mesh-01", "name": "Leather Strap", "prompt": "worn brown leather"},
          {"id": "mesh-02", "name": "Steel Shaft",   "prompt": "brushed stainless steel"}
        ],
        "quality_preset": "final",
        "seed": 42,
        "auto_enhance": false
      }
    Returns one full PBR set per target (same fields as /generate, nested under results[]).
    """
    if pipe is None:
        return JSONResponse(status_code=503, content={"error": "Model not loaded"})

    targets = request.get("targets") or []
    if not targets:
        return JSONResponse(status_code=400, content={"error": "targets[] required"})

    preset_name = request.get("quality_preset", "final")
    preset      = QUALITY_PRESETS.get(preset_name, QUALITY_PRESETS["final"])
    width    = min(int(request.get("width",  preset["size"])), MAX_TEX_SIZE)
    height   = min(int(request.get("height", preset["size"])), MAX_TEX_SIZE)
    width  = max(256, (width  // 8) * 8)
    height = max(256, (height // 8) * 8)
    base_seed = int(request.get("seed", 42))
    steps     = request.get("steps", preset["steps"])
    guidance  = request.get("guidance_scale", preset["guidance"])
    gen_all   = request.get("generate_all_maps", preset["gen_all"])
    auto_enh  = bool(request.get("auto_enhance", False))

    results = []
    t0 = time.time()
    for i, t in enumerate(targets):
        tid   = str(t.get("id", f"target_{i}"))
        name  = str(t.get("name", tid))
        prompt = str(t.get("prompt", "material surface")).strip() or "material surface"
        if auto_enh:
            try:
                prompt = ollama_enhance_prompt(prompt)
            except Exception:
                pass
        seed = int(t.get("seed", base_seed + i * 9973))
        maps = generate_all_maps(prompt, width, height, seed, steps, guidance, gen_all)
        gen_id = db_save_gen(prompt, current_model_key, seed, steps, guidance, width, preset_name, 0.0)
        packed = _pack_maps_response(maps, seed, preset_name, gen_id, 0.0)
        packed["target_id"] = tid
        packed["target_name"] = name
        packed["prompt"] = prompt
        results.append(packed)
        # Persist per-object
        out_dir = Path("generated_textures") / "by_object"
        out_dir.mkdir(parents=True, exist_ok=True)
        safe = "".join(c if c.isalnum() or c in "-_" else "_" for c in tid)[:48]
        for mname, img in maps.items():
            img.save(out_dir / f"{safe}_{seed}_{mname}.png")

    elapsed = time.time() - t0
    return {
        "status": "ok",
        "elapsed": round(elapsed, 2),
        "count": len(results),
        "preset": preset_name,
        "width": width,
        "height": height,
        "results": results,
        "db": db_stats(),
    }


@app.post("/rate")
async def rate(request: dict):
    gen_id = request.get("gen_id")
    rating = request.get("rating")
    if gen_id is None or rating is None:
        return JSONResponse(status_code=400, content={"error": "gen_id and rating required"})
    db_rate(int(gen_id), int(rating))
    return {"status": "ok", "gen_id": gen_id, "rating": rating, "db": db_stats()}


@app.get("/db_stats")
def get_db_stats():
    return db_stats()


# ============================================================
# Ollama helpers  (ollama must be running: ollama serve)
# Text model:   ollama pull llama3.2:3b   (fast, ~2 GB)
# Vision model: ollama pull llava:7b      (image-to-text, ~4 GB)
# ============================================================
OLLAMA_HOST = "localhost"
OLLAMA_PORT = 11434


def _ollama_post(path: str, payload: dict, timeout: int = 30) -> dict:
    """Raw POST to Ollama REST API. Returns parsed JSON or raises."""
    import urllib.request
    body = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        f"http://{OLLAMA_HOST}:{OLLAMA_PORT}{path}",
        data=body,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read())


def ollama_enhance_prompt(user_prompt: str, model: str = "llama3.2:3b") -> str:
    """
    Rewrite a short material description into a Stable Diffusion-optimised prompt.
    Returns the enhanced string, or the original on failure.
    """
    system = (
        "You are an expert Stable Diffusion prompt engineer specialising in "
        "seamless PBR texture generation. Rewrite the user's short material description "
        "into a single rich SD prompt (max 60 words). "
        "Include: texture type, surface qualities (roughness, reflectivity), colours, "
        "wear state, PBR context words (tileable, seamless, photorealistic, 8k). "
        "Output ONLY the prompt — no preamble, no quotes, no bullet points."
    )
    result = _ollama_post("/api/generate", {
        "model": model,
        "system": system,
        "prompt": user_prompt,
        "stream": False,
        "options": {"temperature": 0.4, "num_predict": 120},
    }, timeout=30)
    return result.get("response", user_prompt).strip()


def ollama_describe_image(image_b64: str, model: str = "llava:7b") -> str:
    """
    Use a local vision model (LLaVA) to describe a reference material photo.
    image_b64: standard base64 string (no data URI prefix).
    Returns a short material description suitable for further SD prompt enhancement.
    """
    prompt = (
        "Describe the material or surface texture in this image in 1-2 sentences. "
        "Focus on: colour, finish (matte/glossy/rough), material type, wear or aging. "
        "Be specific and technical. Output only the description, no preamble."
    )
    result = _ollama_post("/api/generate", {
        "model": model,
        "prompt": prompt,
        "images": [image_b64],
        "stream": False,
        "options": {"temperature": 0.2, "num_predict": 80},
    }, timeout=60)
    return result.get("response", "").strip()


# ============================================================
# Prompt endpoints
# ============================================================

# Flat material → SD prompt dictionary for the app's local autocomplete
# Mirrored here so the app can optionally refresh from the server.
MATERIAL_DICT_PY = {
    "bare steel":           "bare steel surface, cold blue-grey metallic, machined finish",
    "brushed steel":        "brushed stainless steel, linear grain pattern, chrome highlights",
    "polished steel":       "mirror-polished steel, highly reflective, silver metallic",
    "cast iron":            "cast iron surface, dark grey, porous rough finish, matte",
    "galvanized steel":     "hot-dip galvanized steel, crystalline spangle pattern, silvery zinc",
    "aluminum raw":         "raw aluminum surface, light grey, machined marks, low reflectivity",
    "brushed aluminum":     "anodized brushed aluminum, satin finish, fine linear grain",
    "copper":               "copper metal surface, warm orange-brown, high specular",
    "oxidized copper":      "heavily oxidized copper, blue-green patina, verdigris texture",
    "brass":                "polished brass, warm golden yellow, high specular, subtle grain",
    "aged brass":           "aged brass, dark golden brown, tarnished, patina spots",
    "bronze":               "dark bronze alloy, brownish-grey, antique finish",
    "gold":                 "gold metal surface, vivid yellow, mirror-like reflective finish",
    "silver":               "pure silver surface, bright white-grey, high reflectivity",
    "titanium":             "titanium metal surface, dark silver-blue grey, anodized",
    "chrome":               "chrome plating, mirror finish, highly reflective silver surface",
    "gunmetal":             "gunmetal grey alloy, dark blue-grey, matte-satin military finish",
    "nickel":               "nickel plated surface, bright silver, slightly warm tint",
    "rust":                 "heavily corroded steel, thick flaking orange-brown rust, dark pitting",
    "surface rust":         "light surface rust, orange-red streaks on steel, early corrosion",
    "pitted corrosion":     "severe pitting corrosion, deep craters, dark brown corroded metal",
    "powder coat black":    "gloss black powder coat finish, smooth even coating, slight sheen",
    "powder coat white":    "white powder coat, smooth matte-satin finish, uniform coverage",
    "powder coat grey":     "medium grey powder coat, industrial matte finish",
    "matte black paint":    "flat matte black paint coating, zero specular, uniform",
    "gloss paint":          "high gloss automotive paint, deep wet reflective finish",
    "primer grey":          "grey etching primer, rough matte surface, light grey",
    "hammertone paint":     "hammertone textured paint, dimpled metallic finish",
    "anodized black":       "black anodized aluminum, dark matte satin",
    "blued steel":          "hot-blued steel, deep blue-black oxidized finish",
    "phosphate coat":       "phosphate parkerized finish, dark grey matte, corrosion resistant",
    "carbon fiber":         "woven carbon fiber composite, black diagonal weave, glossy clear coat",
    "carbon fiber matte":   "matte carbon fiber weave, black, non-glossy surface",
    "ceramic":              "glazed ceramic surface, smooth white, slight gloss",
    "rubber":               "black rubber surface, matte, soft, slightly bumpy",
    "concrete raw":         "raw poured concrete, rough grey, porous, construction texture",
    "polished concrete":    "polished concrete floor, smooth grey, subtle aggregate visible",
    "sandstone":            "buff sandstone, warm tan, fine-grained, slightly layered",
    "granite":              "dark grey granite, coarse speckled pattern, polished",
    "marble":               "white marble, grey veins, polished smooth, cool tone",
    "slate":                "dark grey slate stone, layered flaky surface, rough",
    "brick":                "red fired brick, rough rectangular, mortar joints visible",
    "oak wood":             "oak wood grain, warm brown, clear visible grain lines",
    "dark walnut":          "dark walnut wood, rich brown, fine grain, polished",
    "weathered wood":       "weathered grey wood, cracked, faded paint, splinters",
    "leather":              "dark brown leather, soft surface texture, natural grain",
    "worn leather":         "worn aged leather, cracked, dry, patina, tan-brown",
    "pcb green":            "FR4 PCB circuit board, green soldermask, gold traces",
    "diamond plate":        "diamond plate aluminum, raised diamond pattern, metallic",
    "perforated metal":     "perforated steel sheet, regular hole pattern, matte grey",
    "weld bead":            "TIG weld bead on steel, ripple pattern, heat-discolored zone",
    "heat treated steel":   "heat treated steel, rainbow oxide colors, blue-purple-gold zones",
    "hdpe plastic":         "white HDPE plastic, slightly waxy, matte, low gloss",
    "abs plastic":          "grey ABS plastic, injection moulded, smooth matte",
    "asphalt":              "dark asphalt road, coarse aggregate, black, rough",
    "sand":                 "fine sand surface, warm tan, granular, flat",
}


@app.get("/material_dict")
def material_dict():
    """Returns the full material name → SD prompt mapping for client autocomplete refresh."""
    return {"materials": MATERIAL_DICT_PY}


@app.post("/enhance_prompt")
async def enhance_prompt(request: dict):
    """
    Rewrite a short material description into a rich SD prompt using a local Ollama LLM.
    Body: {"prompt": "black powder coat", "model": "llama3.2:3b"}
    Response: {"enhanced": "...", "original": "..."}
    Requires: ollama pull llama3.2:3b (or any small text model)
    """
    prompt = request.get("prompt", "").strip()
    if not prompt:
        return JSONResponse(status_code=400, content={"error": "prompt is required"})
    model = request.get("model", "llama3.2:3b")
    try:
        enhanced = ollama_enhance_prompt(prompt, model=model)
        return {"status": "ok", "original": prompt, "enhanced": enhanced}
    except Exception as e:
        # Graceful degradation: return original if Ollama isn't running
        return {"status": "ollama_unavailable", "original": prompt, "enhanced": prompt,
                "error": str(e)}


@app.post("/describe_image")
async def describe_image(request: dict):
    """
    Describe a reference material photo using a local Ollama vision model (LLaVA).
    Body: {"image_b64": "<base64 string>", "model": "llava:7b", "auto_enhance": true}
    Response: {"description": "...", "enhanced": "..."}
    Requires: ollama pull llava:7b
    The 'enhanced' field passes the raw description through /enhance_prompt automatically
    if auto_enhance is true (default true).
    """
    image_b64 = request.get("image_b64", "")
    if not image_b64:
        return JSONResponse(status_code=400, content={"error": "image_b64 is required"})
    vision_model = request.get("model", "llava:7b")
    text_model   = request.get("text_model", "llama3.2:3b")
    auto_enhance = request.get("auto_enhance", True)
    try:
        description = ollama_describe_image(image_b64, model=vision_model)
        enhanced = description
        if auto_enhance and description:
            try:
                enhanced = ollama_enhance_prompt(description, model=text_model)
            except Exception:
                pass  # Enhanced stays as raw description
        return {"status": "ok", "description": description, "enhanced": enhanced}
    except Exception as e:
        return {"status": "ollama_unavailable", "description": "", "enhanced": "",
                "error": str(e)}


# Also allow /generate to auto-enhance the prompt via Ollama before sending to SD.
# Patch is applied by checking 'auto_enhance' in the request body above `generate_all_maps`.
_original_generate = None  # placeholder — enhancement is inline in the route below


# ============================================================
# Neural Beauty — path-tracer / G-buffer guided AI polish
# ============================================================
@app.post("/neural_beauty")
async def neural_beauty(request: dict):
    """
    Structure-guided AI beauty pass for Azra path-tracer / viewport frames.

    Works WITH or WITHOUT a loaded SD model:
      - pipe loaded  → SD img2img + structure lock
      - no pipe      → classical edge-aware polish (still uses depth/normal/edges)

    Required:
      beauty_b64 — base64 PNG/JPEG of the rendered frame
      prompt     — look / material / lighting description
    """
    try:
        from neural_beauty import run_neural_beauty
    except ImportError:
        import importlib.util
        here = Path(__file__).resolve().parent
        spec = importlib.util.spec_from_file_location(
            "neural_beauty", here / "neural_beauty.py"
        )
        if spec is None or spec.loader is None:
            return JSONResponse(status_code=500, content={"error": "neural_beauty.py missing"})
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        run_neural_beauty = mod.run_neural_beauty

    # Accept missing keys safely (old clients / partial AOV sets)
    if not isinstance(request, dict):
        return JSONResponse(status_code=400, content={"error": "JSON body required"})

    with _model_lock:
        try:
            model_label = MODELS.get(current_model_key, {}).get("label", current_model_key or "")
            result = run_neural_beauty(
                pipe,  # may be None → classical path
                beauty_b64=request.get("beauty_b64", "") or "",
                prompt=request.get("prompt", "detailed hard-surface prop, realistic materials") or "",
                depth_b64=request.get("depth_b64", "") or "",
                normal_b64=request.get("normal_b64", "") or "",
                edges_b64=request.get("edges_b64", "") or "",
                curvature_b64=request.get("curvature_b64", "") or "",
                strength=float(request.get("strength", 0.40)),
                structure=float(request.get("structure", 0.80)),
                seed=int(request.get("seed", 42)),
                steps=int(request.get("steps", 28)),
                guidance=float(request.get("guidance", 7.0)),
                max_side=int(request.get("max_side", 1024)),
                quality_preset=str(request.get("quality_preset", "") or ""),
                use_depth=bool(request.get("use_depth", True)),
                use_normal=bool(request.get("use_normal", True)),
                use_edges=bool(request.get("use_edges", True)),
                use_curvature=bool(request.get("use_curvature", True)),
                device=device,
                model_label=model_label,
                model_arch=current_model_arch,
            )
        except Exception as e:
            print(f"[neural_beauty] endpoint error: {e}")
            return JSONResponse(status_code=500, content={"error": str(e)})

    if result.get("status") != "ok":
        return JSONResponse(status_code=500, content=result)
    return result


# ============================================================
# Entry point
# ============================================================
if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="FourthLayer Texture Server v4")
    _default_model = (
        "flux-kontext" if "flux-kontext" in MODELS
        else (
            "cyberrealistic-xl" if "cyberrealistic-xl" in MODELS
            else ("juggernaut-xl" if "juggernaut-xl" in MODELS else next(iter(MODELS)))
        )
    )
    parser.add_argument(
        "--model",
        default=_default_model,
        choices=list(MODELS.keys()),
        help=f"Model key at startup (default: {_default_model} — Flux.1 Kontext first)",
    )
    parser.add_argument("--port", type=int, default=7860)
    parser.add_argument("--host", default="127.0.0.1")
    args = parser.parse_args()

    print("=" * 62)
    print("  Azra AI Pipeline  (Flux.1 Kontext first)")
    print("=" * 62)
    print(f"  Model   : {MODELS[args.model]['label']}  [{MODELS[args.model].get('arch','sd15')}]")
    print(f"  Bind    : http://{args.host}:{args.port}  (Azra-owned worker)")
    print(f"  Beauty  : POST /neural_beauty  (PT + Kontext structure lock)")
    print(f"  Models  : {', '.join(MODELS.keys())}")
    print("=" * 62)

    init_db()

    # Always start the HTTP server so the UI can connect and show proper errors.
    # Model load failure → /health returns no_model, /generate returns 503 with install hint.
    if not load_model(args.model):
        print("\n[!] torch/diffusers not found -- server running in NO-MODEL mode.")
        print("    Install AI packages:")
        print("    pip install torch --index-url https://download.pytorch.org/whl/cu118")
        print("    pip install diffusers transformers accelerate")
        print("    Then restart the server.\n")

    uvicorn.run(app, host=args.host, port=args.port)
