# neural_beauty.py — Path-tracer / G-buffer guided AI beauty polish
#
# Modes:
#   1) SD img2img + structure lock  (when torch + diffusers pipe is loaded)
#   2) Classical structure polish   (always works — no torch required)
#
# Classical mode still uses depth/normal/edges/curvature for edge-aware sharpening
# and silhouette-locked contrast so the feature is useful without a GPU SD stack.

from __future__ import annotations

import base64
import io
import time
from typing import Optional, Tuple

import numpy as np


def _b64_to_rgb(b64: str, size: Optional[Tuple[int, int]] = None):
    from PIL import Image
    if not b64:
        return None
    try:
        raw = base64.b64decode(b64)
        img = Image.open(io.BytesIO(raw)).convert("RGB")
    except Exception:
        return None
    if size is not None:
        img = img.resize(size, Image.Resampling.LANCZOS)
    return img


def _rgb_to_b64_png(img) -> str:
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    return base64.b64encode(buf.getvalue()).decode("ascii")


def _rgb_to_b64_rgba(img) -> Tuple[str, int, int]:
    rgba = img.convert("RGBA")
    w, h = rgba.size
    return base64.b64encode(rgba.tobytes()).decode("ascii"), w, h


def sobel_edges(gray: np.ndarray) -> np.ndarray:
    g = gray.astype(np.float32)
    kx = np.array([[-1, 0, 1], [-2, 0, 2], [-1, 0, 1]], dtype=np.float32)
    ky = np.array([[-1, -2, -1], [0, 0, 0], [1, 2, 1]], dtype=np.float32)

    def conv(img, k):
        p = np.pad(img, 1, mode="edge")
        out = np.zeros_like(img)
        for y in range(3):
            for x in range(3):
                out += k[y, x] * p[y : y + img.shape[0], x : x + img.shape[1]]
        return out

    gx = conv(g, kx)
    gy = conv(g, ky)
    mag = np.sqrt(gx * gx + gy * gy)
    m = mag.max()
    if m > 1e-6:
        mag /= m
    return np.clip(mag, 0.0, 1.0)


def build_structure_mask(
    beauty_rgb: np.ndarray,
    depth_rgb: Optional[np.ndarray],
    normal_rgb: Optional[np.ndarray],
    edges_rgb: Optional[np.ndarray],
    curvature_rgb: Optional[np.ndarray],
    use_depth: bool,
    use_normal: bool,
    use_edges: bool,
    use_curvature: bool,
) -> np.ndarray:
    h, w = beauty_rgb.shape[:2]
    gray = (
        0.299 * beauty_rgb[:, :, 0]
        + 0.587 * beauty_rgb[:, :, 1]
        + 0.114 * beauty_rgb[:, :, 2]
    ) / 255.0

    acc = np.zeros((h, w), dtype=np.float32)
    weight = 0.0

    acc += sobel_edges(gray) * 0.35
    weight += 0.35

    if use_edges and edges_rgb is not None:
        eg = edges_rgb.mean(axis=2) / 255.0
        acc += eg * 1.0
        weight += 1.0

    if use_depth and depth_rgb is not None:
        dg = depth_rgb.mean(axis=2) / 255.0
        acc += sobel_edges(dg) * 1.2
        acc += (1.0 - dg) * 0.15
        weight += 1.35

    if use_normal and normal_rgb is not None:
        ng = normal_rgb.mean(axis=2) / 255.0
        acc += sobel_edges(ng) * 0.9
        nvar = normal_rgb.astype(np.float32).std(axis=2) / 255.0
        acc += np.clip(nvar * 2.0, 0, 1) * 0.4
        weight += 1.3

    if use_curvature and curvature_rgb is not None:
        cg = curvature_rgb.mean(axis=2) / 255.0
        acc += np.clip(cg, 0, 1) * 0.8
        acc += sobel_edges(cg) * 0.5
        weight += 1.3

    if weight > 1e-6:
        acc /= weight

    m = np.clip(acc, 0.0, 1.0)
    m = np.power(m, 0.75)
    p = np.pad(m, 1, mode="edge")
    dil = np.maximum.reduce(
        [
            p[0:-2, 0:-2], p[0:-2, 1:-1], p[0:-2, 2:],
            p[1:-1, 0:-2], p[1:-1, 1:-1], p[1:-1, 2:],
            p[2:, 0:-2], p[2:, 1:-1], p[2:, 2:],
        ]
    )
    return np.clip(dil, 0.0, 1.0)


# Quality presets for Neural Beauty (path-tracer polish)
BEAUTY_QUALITY = {
    "preview":  {"max_side": 768,  "steps": 16, "guidance": 6.5, "sd_strength_scale": 0.75},
    "standard": {"max_side": 1024, "steps": 28, "guidance": 7.0, "sd_strength_scale": 0.80},
    "final":    {"max_side": 1280, "steps": 36, "guidance": 7.5, "sd_strength_scale": 0.82},
    "ultra":    {"max_side": 1536, "steps": 48, "guidance": 8.0, "sd_strength_scale": 0.85},
}


def _build_prompt(user_prompt: str) -> Tuple[str, str]:
    # Strong geometry-preservation language — PT beauty is a LOOK pass, not a redesign.
    prefix = (
        "photorealistic CGI product beauty pass of the EXACT same 3D render, "
        "identical camera framing silhouette proportions and hard-surface edges, "
        "enhance material micro-detail PBR fidelity subsurface and specular response, "
        "clean studio lighting continuity, ray-traced look, sharp focus, "
    )
    suffix = (
        ", 8k uhd, physically based materials, contact shadows preserved, "
        "no redesign, no topology change, no extra objects, no text, no watermark"
    )
    negative = (
        "deformed, warped silhouette, melted edges, soft plastic edges, "
        "extra limbs, extra parts, floating geometry, different object, "
        "cartoon, anime, painterly, illustration, blurry, lowres, jpeg artifacts, "
        "text, watermark, logo, cropped, multiple objects, background clutter, "
        "face, person, hands, oversaturated, neon glow, bloom haze, "
        "changed shape, stretched proportions, missing parts"
    )
    return prefix + user_prompt.strip() + suffix, negative


def _prompt_grade(prompt: str) -> Tuple[np.ndarray, float, float]:
    """Return (rgb_gain, contrast, saturation) from simple keyword heuristics."""
    p = (prompt or "").lower()
    gain = np.array([1.0, 1.0, 1.0], dtype=np.float32)
    contrast = 1.08
    sat = 1.05
    if any(k in p for k in ("metal", "steel", "iron", "chrome", "gun", "rifle", "weapon")):
        gain *= np.array([0.96, 0.98, 1.02], dtype=np.float32)
        contrast = 1.14
        sat = 0.92
    if any(k in p for k in ("rust", "worn", "dirty", "weathered")):
        gain *= np.array([1.06, 0.97, 0.90], dtype=np.float32)
        contrast = 1.12
        sat = 1.08
    if any(k in p for k in ("gold", "brass", "copper", "bronze")):
        gain *= np.array([1.12, 1.02, 0.82], dtype=np.float32)
        sat = 1.15
    if any(k in p for k in ("plastic", "polymer", "matte")):
        contrast = 1.06
        sat = 0.95
    if any(k in p for k in ("studio", "product", "cinematic")):
        contrast = max(contrast, 1.12)
    return gain, contrast, sat


def classical_beauty_polish(
    beauty_np: np.ndarray,
    mask: np.ndarray,
    prompt: str,
    strength: float,
    structure: float,
) -> np.ndarray:
    """
    Edge-aware unsharp + contrast + mild grade, locked by structure mask.
    No ML dependencies.
    """
    base = beauty_np.astype(np.float32)
    # Blur via box filter (3 passes of 3x3 average ≈ soft blur)
    blur = base.copy()
    for _ in range(2):
        p = np.pad(blur, ((1, 1), (1, 1), (0, 0)), mode="edge")
        blur = (
            p[0:-2, 0:-2] + p[0:-2, 1:-1] + p[0:-2, 2:]
            + p[1:-1, 0:-2] + p[1:-1, 1:-1] + p[1:-1, 2:]
            + p[2:, 0:-2] + p[2:, 1:-1] + p[2:, 2:]
        ) / 9.0

    amount = 0.35 + 0.9 * float(np.clip(strength, 0.05, 0.95))
    sharp = base + (base - blur) * amount

    gain, contrast, sat = _prompt_grade(prompt)
    mid = 128.0
    graded = (sharp - mid) * contrast + mid
    graded *= gain.reshape(1, 1, 3)
    # saturation
    luma = (
        0.299 * graded[:, :, 0] + 0.587 * graded[:, :, 1] + 0.114 * graded[:, :, 2]
    )[..., None]
    graded = luma + (graded - luma) * sat

    # Extra edge pop on structure (micro-detail on silhouette / panels)
    edge = mask[..., None]
    edge_boost = sharp + (sharp - blur) * (0.5 * amount)
    graded = graded * (1.0 - 0.35 * edge) + edge_boost * (0.35 * edge)

    # Structure lock toward original form (never melt silhouette)
    lock = (mask * float(np.clip(structure, 0.0, 1.0)))[..., None]
    # Always keep some original; high structure keeps more original shape/color
    out = graded * (1.0 - 0.55 * lock) + base * (0.55 * lock)
    # Soft global mix with original controlled by strength (low strength = subtler)
    mix = 0.25 + 0.75 * float(np.clip(strength, 0.05, 0.95))
    out = base * (1.0 - mix) + out * mix
    return np.clip(out, 0, 255).astype(np.uint8)


def _composite_structure_lock(
    gen_np: np.ndarray,
    base_np: np.ndarray,
    mask: np.ndarray,
    structure: float,
    strength: float,
) -> np.ndarray:
    """
    Blend SD result with original PT beauty.
    High structure mask (silhouettes / hard edges) keeps the render;
    flat interiors get more AI micro-detail. Stronger than a flat lerp.
    """
    structure = float(np.clip(structure, 0.0, 1.0))
    strength = float(np.clip(strength, 0.05, 0.95))
    # Edge lock weight: near 1 on structure, lower on flats
    edge = np.clip(mask, 0.0, 1.0)
    # Base lock from structure slider + edge emphasis
    lock = np.clip(edge * (0.35 + 0.65 * structure) + (1.0 - edge) * (0.08 * structure), 0.0, 0.95)
    lock = lock[..., None]
    # How much of the creative pass to admit overall
    admit = 0.15 + 0.85 * strength
    blended = gen_np * (1.0 - lock) + base_np * lock
    out = base_np * (1.0 - admit) + blended * admit
    # Micro-sharpen flat regions only (avoid ringing on silhouettes)
    blur = out.copy()
    p = np.pad(blur, ((1, 1), (1, 1), (0, 0)), mode="edge")
    blur = (
        p[0:-2, 0:-2] + p[0:-2, 1:-1] + p[0:-2, 2:]
        + p[1:-1, 0:-2] + p[1:-1, 1:-1] + p[1:-1, 2:]
        + p[2:, 0:-2] + p[2:, 1:-1] + p[2:, 2:]
    ) / 9.0
    flat = (1.0 - edge)[..., None]
    out = out + (out - blur) * (0.22 * strength) * flat
    return np.clip(out, 0, 255).astype(np.uint8)


def run_neural_beauty(
    pipe,
    beauty_b64: str,
    prompt: str,
    *,
    depth_b64: str = "",
    normal_b64: str = "",
    edges_b64: str = "",
    curvature_b64: str = "",
    strength: float = 0.40,
    structure: float = 0.80,
    seed: int = 42,
    steps: int = 28,
    guidance: float = 7.0,
    max_side: int = 1024,
    quality_preset: str = "",
    use_depth: bool = True,
    use_normal: bool = True,
    use_edges: bool = True,
    use_curvature: bool = True,
    device: str = "cuda",
    model_label: str = "",
    model_arch: str = "sd15",
) -> dict:
    from PIL import Image

    t0 = time.time()
    if not beauty_b64:
        return {"status": "error", "error": "beauty_b64 required"}

    beauty = _b64_to_rgb(beauty_b64)
    if beauty is None:
        return {"status": "error", "error": "invalid beauty image"}

    # Quality preset can raise max_side / steps / guidance when client omits overrides
    q = BEAUTY_QUALITY.get((quality_preset or "").lower(), None)
    sd_strength_scale = 0.82
    if q is not None:
        max_side = max(int(max_side), int(q["max_side"]))
        steps = max(int(steps), int(q["steps"]))
        guidance = max(float(guidance), float(q["guidance"]))
        sd_strength_scale = float(q["sd_strength_scale"])

    # SDXL / Flux want ≥1024 for best quality; cap still 1536
    max_side = int(np.clip(max_side, 256, 1536))
    arch_l = (model_arch or "sd15").lower()
    is_flux = arch_l in ("flux_kontext", "flux", "flux-kontext")
    if arch_l == "sdxl" or is_flux:
        max_side = max(max_side, 1024)
    w0, h0 = beauty.size
    scale = min(1.0, float(max_side) / float(max(w0, h0)))
    # Flux prefers multiples of 16; SD of 8
    snap = 16 if is_flux else 8
    tw = max(64, int(round(w0 * scale / snap) * snap))
    th = max(64, int(round(h0 * scale / snap) * snap))
    if (arch_l == "sdxl" or is_flux) and max(tw, th) < 768:
        s2 = 1024.0 / float(max(tw, th))
        tw = max(256, (int(round(tw * s2)) // snap) * snap)
        th = max(256, (int(round(th * s2)) // snap) * snap)
    size = (tw, th)
    beauty = beauty.resize(size, Image.Resampling.LANCZOS)

    depth = _b64_to_rgb(depth_b64, size) if depth_b64 else None
    normal = _b64_to_rgb(normal_b64, size) if normal_b64 else None
    edges = _b64_to_rgb(edges_b64, size) if edges_b64 else None
    curvature = _b64_to_rgb(curvature_b64, size) if curvature_b64 else None

    beauty_np = np.array(beauty, dtype=np.uint8)
    depth_np = np.array(depth, dtype=np.uint8) if depth is not None else None
    normal_np = np.array(normal, dtype=np.uint8) if normal is not None else None
    edges_np = np.array(edges, dtype=np.uint8) if edges is not None else None
    curv_np = np.array(curvature, dtype=np.uint8) if curvature is not None else None

    mask = build_structure_mask(
        beauty_np,
        depth_np,
        normal_np,
        edges_np,
        curv_np,
        use_depth=use_depth and depth_np is not None,
        use_normal=use_normal and normal_np is not None,
        use_edges=use_edges and edges_np is not None,
        use_curvature=use_curvature and curv_np is not None,
    )

    full_prompt, negative = _build_prompt(prompt or "detailed hard-surface prop, realistic materials")
    strength = float(np.clip(strength, 0.05, 0.95))
    structure = float(np.clip(structure, 0.0, 1.0))
    steps = int(np.clip(steps, 4, 60))
    guidance = float(np.clip(guidance, 1.0, 15.0))
    # Map UI strength → safer img2img denoise (high denoise melts PT geometry)
    sd_strength = float(np.clip(strength * sd_strength_scale, 0.12, 0.72))

    gen_img = None
    mode = "classical+structure_lock"
    note = ""

    # ---- Optional ML path (Flux Kontext / SD1.5 / SDXL img2img) ----
    arch = (model_arch or "sd15").lower()
    is_flux = arch in ("flux_kontext", "flux", "flux-kontext")
    if not is_flux and pipe is not None:
        is_flux = "FluxKontext" in type(pipe).__name__
    if pipe is not None:
        try:
            import torch

            # ── Flux.1 Kontext: image-conditioned edit (ideal for PT beauty) ──
            if is_flux:
                from flux_kontext import run_flux_kontext, flux_guidance

                # Lower guidance than SD; strength maps to how far we allow change
                # via true structure composite after (not denoise strength like SD)
                flux_steps = int(np.clip(steps, 8, 40))
                flux_g = flux_guidance(guidance)
                # Mild prompt bias for subtle polish when strength is low
                if strength < 0.35:
                    edit_prompt = (
                        "subtle material fidelity and micro-detail enhancement of the exact same render, "
                        + full_prompt
                    )
                else:
                    edit_prompt = full_prompt
                gen_img = run_flux_kontext(
                    pipe,
                    prompt=edit_prompt,
                    image=beauty,
                    width=tw,
                    height=th,
                    seed=seed,
                    steps=flux_steps,
                    guidance=flux_g,
                    negative_prompt=negative,
                    device=device,
                )
                if gen_img.size != size:
                    gen_img = gen_img.resize(size, Image.Resampling.LANCZOS)
                mode = "flux_kontext+structure_lock"
                tag = model_label or "Flux.1 Kontext"
                note = f"{tag} @ {tw}x{th} steps={flux_steps} guidance={flux_g:.2f}"
            else:
                from diffusers import (
                    StableDiffusionImg2ImgPipeline,
                    StableDiffusionXLImg2ImgPipeline,
                )

                gen_device = device if device == "cuda" and torch.cuda.is_available() else "cpu"
                generator = torch.Generator(device=gen_device).manual_seed(int(seed) & 0xFFFFFFFF)

                img2img_pipe = None
                is_xl = arch == "sdxl" or "XL" in type(pipe).__name__ or "xl" in type(pipe).__name__.lower()

                if is_xl:
                    if isinstance(pipe, StableDiffusionXLImg2ImgPipeline):
                        img2img_pipe = pipe
                    elif hasattr(pipe, "components"):
                        img2img_pipe = StableDiffusionXLImg2ImgPipeline(**pipe.components)
                        if gen_device == "cuda":
                            img2img_pipe = img2img_pipe.to("cuda")
                else:
                    if isinstance(pipe, StableDiffusionImg2ImgPipeline):
                        img2img_pipe = pipe
                    elif hasattr(pipe, "components"):
                        img2img_pipe = StableDiffusionImg2ImgPipeline(**pipe.components)
                        if gen_device == "cuda":
                            img2img_pipe = img2img_pipe.to("cuda")

                if img2img_pipe is not None:
                    call = dict(
                        prompt=full_prompt,
                        negative_prompt=negative,
                        image=beauty,
                        strength=sd_strength,
                        num_inference_steps=steps,
                        guidance_scale=guidance,
                        generator=generator,
                    )
                    if is_xl:
                        call["prompt_2"] = full_prompt
                        call["negative_prompt_2"] = negative
                    result = img2img_pipe(**call)
                    gen_img = result.images[0].convert("RGB")
                    if gen_img.size != size:
                        gen_img = gen_img.resize(size, Image.Resampling.LANCZOS)
                    mode = "img2img+structure_lock"
                    tag = "SDXL" if is_xl else "SD1.5"
                    if model_label:
                        note = f"{tag} img2img ({model_label}) @ {tw}x{th} steps={steps}"
                    else:
                        note = f"{tag} img2img @ {tw}x{th} steps={steps}"
                else:
                    result = pipe(
                        prompt=full_prompt,
                        negative_prompt=negative,
                        width=tw,
                        height=th,
                        num_inference_steps=steps,
                        guidance_scale=guidance,
                        generator=generator,
                    )
                    gen_img = result.images[0].convert("RGB")
                    mode = "txt2img+structure_lock"
                    note = "txt2img fallback (no img2img components)"
        except Exception as e:
            note = f"ML unavailable ({e}); classical polish used"
            print(f"[neural_beauty] {note}")
            import traceback
            traceback.print_exc()
            gen_img = None
    else:
        note = "No model loaded (torch/diffusers); classical polish used"
        print(f"[neural_beauty] {note}")

    if gen_img is not None:
        gen_np = np.array(gen_img, dtype=np.float32)
        base_np = beauty_np.astype(np.float32)
        out = _composite_structure_lock(gen_np, base_np, mask, structure, strength)
    else:
        out = classical_beauty_polish(beauty_np, mask, prompt, strength, structure)
        mode = "classical+structure_lock"

    out_img = Image.fromarray(out, mode="RGB")
    if (w0, h0) != size:
        # Upscale with LANCZOS then re-lock high-frequency edges from original full-res beauty
        out_img = out_img.resize((w0, h0), Image.Resampling.LANCZOS)
        base_full = _b64_to_rgb(beauty_b64)
        if base_full is not None and base_full.size == (w0, h0):
            base_f = np.array(base_full, dtype=np.float32)
            out_f = np.array(out_img, dtype=np.float32)
            mask_full = Image.fromarray((mask * 255).astype(np.uint8), mode="L").resize(
                (w0, h0), Image.Resampling.BILINEAR
            )
            m = np.array(mask_full, dtype=np.float32) / 255.0
            # On silhouettes, prefer original full-res PT pixels
            lock = (m * (0.45 + 0.45 * structure))[..., None]
            out_f = out_f * (1.0 - lock) + base_f * lock
            out_img = Image.fromarray(np.clip(out_f, 0, 255).astype(np.uint8), mode="RGB")
        mask_img = Image.fromarray((mask * 255).astype(np.uint8), mode="L").resize(
            (w0, h0), Image.Resampling.BILINEAR
        )
    else:
        mask_img = Image.fromarray((mask * 255).astype(np.uint8), mode="L")

    rgba_b64, rw, rh = _rgb_to_b64_rgba(out_img)
    elapsed = time.time() - t0

    return {
        "status": "ok",
        "mode": mode,
        "note": note,
        "elapsed": round(elapsed, 2),
        "width": rw,
        "height": rh,
        "seed": int(seed),
        "strength": strength,
        "structure": structure,
        "max_side": max_side,
        "steps": steps,
        "sd_strength": sd_strength if gen_img is not None else 0.0,
        "quality_preset": (quality_preset or "custom"),
        "result_png_b64": _rgb_to_b64_png(out_img),
        "result_rgba_b64": rgba_b64,
        "mask_png_b64": _rgb_to_b64_png(mask_img.convert("RGB")),
        "prompt_used": full_prompt,
    }
