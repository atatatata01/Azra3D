#!/usr/bin/env python3
"""Generate AZRA's bundled starter content: matcaps and default environment maps.

Everything here is computed from scratch (no photographs, no third-party images), so the
output carries no third-party license. The generated files are covered by the same
AGPL-3.0-or-later license as the rest of the repository.

Usage (from the repository root):
    python tools/gen_default_content.py

Requires: numpy, Pillow.
Writes:   assets/Matcaps/*.png               (512x512 matcaps)
          assets/HDR_Labs/HDR_Gradient/*.hdr (1024x512 equirectangular Radiance HDR)
"""
from pathlib import Path

import numpy as np
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
MATCAP_DIR = ROOT / "assets" / "Matcaps"
HDR_DIR = ROOT / "assets" / "HDR_Labs" / "HDR_Gradient"

# --------------------------------------------------------------------------------------
# Procedural studio environment, as a function of a unit direction (y is up).
# --------------------------------------------------------------------------------------


def _normalize(v):
    return v / np.maximum(np.linalg.norm(v, axis=-1, keepdims=True), 1e-9)


def _blob(d, centre, sharpness, power=1.0):
    """Smooth lobe around a direction: (max(0, d.c))^sharpness."""
    c = _normalize(np.asarray(centre, dtype=np.float64))
    return np.clip(np.sum(d * c, axis=-1), 0.0, 1.0) ** sharpness * power


def studio_env(d, key=6.0, fill=1.2, rim=2.5, ambient=1.0):
    """RGB radiance for directions d (..., 3). A soft three-light studio on a grey sweep."""
    y = d[..., 1]
    sky = np.array([0.80, 0.84, 0.92])
    floor = np.array([0.10, 0.10, 0.11])
    t = np.clip(y * 0.5 + 0.5, 0.0, 1.0)[..., None]
    base = (floor * (1 - t) + sky * t) * 0.55 * ambient
    # Soft horizon band to give metals a readable reflection line.
    band = np.exp(-((y - 0.02) / 0.12) ** 2)[..., None] * np.array([0.12, 0.12, 0.13]) * ambient
    k = _blob(d, (0.7, 0.8, 0.6), 18.0, key)[..., None] * np.array([1.00, 0.96, 0.90])
    f = _blob(d, (-0.9, 0.3, 0.4), 10.0, fill)[..., None] * np.array([0.80, 0.88, 1.00])
    r = _blob(d, (-0.2, 0.6, -0.9), 28.0, rim)[..., None] * np.array([1.00, 1.00, 1.00])
    return base + band + k + f + r


def blurred_env(n, **kw):
    """Cheap diffuse irradiance proxy: the environment smoothed over the hemisphere of n."""
    y = n[..., 1]
    t = np.clip(y * 0.5 + 0.5, 0.0, 1.0)[..., None]
    sky = np.array([0.78, 0.82, 0.90])
    floor = np.array([0.12, 0.11, 0.11])
    amb = (floor * (1 - t) + sky * t) * 0.9
    key = np.clip(np.sum(n * _normalize(np.array([0.7, 0.8, 0.6])), axis=-1), 0, 1)[..., None]
    fill = np.clip(np.sum(n * _normalize(np.array([-0.9, 0.3, 0.4])), axis=-1), 0, 1)[..., None]
    return amb * 0.55 + key * np.array([0.75, 0.72, 0.66]) + fill * np.array([0.18, 0.20, 0.24])


# --------------------------------------------------------------------------------------
# Matcaps
# --------------------------------------------------------------------------------------

N = 512


def sphere_normals():
    xs = (np.arange(N) + 0.5) / N * 2 - 1
    x, y = np.meshgrid(xs, -xs)
    # Outside the disc, extend the rim colour outwards: pull (x, y) back onto a circle just
    # inside the silhouette so the corners are the edge shading, not a bogus normal.
    r = np.sqrt(x * x + y * y)
    s = np.where(r > 0.985, 0.985 / np.maximum(r, 1e-9), 1.0)
    x, y = x * s, y * s
    z = np.sqrt(np.maximum(1.0 - (x * x + y * y), 0.0))
    return np.stack([x, y, z], axis=-1)


def shade(n, albedo, metal=0.0, rough=0.35, spec=0.5, rim=0.0, tint=None, env_gain=1.0):
    v = np.array([0.0, 0.0, 1.0])
    albedo = np.asarray(albedo, dtype=np.float64)
    ndv = np.clip(n[..., 2], 0.0, 1.0)[..., None]
    refl = _normalize(2.0 * ndv * n - v)
    sharp = studio_env(refl) * env_gain
    soft = blurred_env(refl)
    glossy = sharp * (1.0 - rough) + soft * rough
    diffuse = albedo * blurred_env(n)
    fres = (0.04 + (1 - 0.04) * (1 - ndv) ** 5) * spec * 2.0
    dielectric = diffuse * (1 - fres) + glossy * fres
    tint_c = albedo if tint is None else np.asarray(tint, dtype=np.float64)
    metallic = glossy * tint_c
    col = dielectric * (1 - metal) + metallic * metal
    if rim > 0:
        col = col + (1 - ndv) ** 3 * rim * np.array([0.9, 0.95, 1.0])
    return col


def tonemap(c):
    c = np.maximum(c, 0.0)
    c = c / (1.0 + c * 0.25)  # soft shoulder, keeps highlights from clipping flat
    return np.clip(c, 0, 1) ** (1 / 2.2)


def save_png(path, rgb, seed):
    rng = np.random.default_rng(seed)
    img = rgb + (rng.random(rgb.shape) - 0.5) / 255.0  # dither against banding
    Image.fromarray((np.clip(img, 0, 1) * 255 + 0.5).astype(np.uint8), "RGB").save(path, optimize=True)


def matcap_presets(n):
    h = np.clip(np.sum(n * _normalize(np.array([0.5, 0.7, 0.6])), axis=-1), 0, 1)[..., None]
    steps = np.floor(h * 3.999) / 3.0
    return {
        "Clay_Gray": shade(n, (0.55, 0.55, 0.56), rough=0.9, spec=0.15),
        "Clay_Warm": shade(n, (0.66, 0.40, 0.30), rough=0.9, spec=0.15),
        "Clay_Cool": shade(n, (0.42, 0.50, 0.60), rough=0.9, spec=0.15),
        "Ceramic_White": shade(n, (0.88, 0.88, 0.86), rough=0.15, spec=0.9),
        "Black_Glossy": shade(n, (0.02, 0.02, 0.025), rough=0.05, spec=1.0, rim=0.04),
        "Chrome": shade(n, (0.95, 0.95, 0.96), metal=1.0, rough=0.02, spec=1.0),
        "Steel_Satin": shade(n, (0.62, 0.64, 0.67), metal=1.0, rough=0.45, spec=1.0),
        "Gold": shade(n, (1.0, 0.78, 0.34), metal=1.0, rough=0.12, spec=1.0),
        "Copper": shade(n, (0.95, 0.52, 0.38), metal=1.0, rough=0.18, spec=1.0),
        "Brass": shade(n, (0.85, 0.68, 0.30), metal=1.0, rough=0.30, spec=1.0),
        "Plastic_Red": shade(n, (0.70, 0.06, 0.05), rough=0.12, spec=0.8),
        "Plastic_Green": shade(n, (0.10, 0.55, 0.15), rough=0.12, spec=0.8),
        "Plastic_Blue": shade(n, (0.08, 0.20, 0.70), rough=0.12, spec=0.8),
        "Soft_Skin": shade(n, (0.80, 0.55, 0.45), rough=0.6, spec=0.3, rim=0.10),
        "Sculpt_Dark": shade(n, (0.22, 0.22, 0.24), rough=0.7, spec=0.35),
        "Pearl_Blue": shade(n, (0.12, 0.22, 0.60), rough=0.2, spec=0.9, rim=0.30),
        "Pearl_Lime": shade(n, (0.45, 0.70, 0.12), rough=0.2, spec=0.9, rim=0.25),
        "Wax_Red": shade(n, (0.70, 0.14, 0.10), rough=0.5, spec=0.3, rim=0.15),
        "Glass_Blue": shade(n, (0.10, 0.28, 0.55), rough=0.02, spec=1.0, rim=0.45),
        "Steel_Warm": shade(n, (0.76, 0.69, 0.62), metal=1.0, rough=0.35, spec=1.0),
        "Steel_Cold": shade(n, (0.58, 0.64, 0.73), metal=1.0, rough=0.25, spec=1.0),
        "Bronze_Antique": shade(n, (0.55, 0.38, 0.22), metal=1.0, rough=0.45, spec=1.0),
        "Outline_White": (0.92 * (1.0 - np.clip((1.0 - n[..., 2:3]) ** 3 * 2.4, 0.0, 1.0)) + 0.04) * np.ones(3),
        "Toon_Steps": np.clip(0.18 + steps * 0.9, 0, 1) * np.array([0.95, 0.90, 0.85]),
        # Debug matcap: view-space normal as colour.
        "Normal": (n * 0.5 + 0.5),
    }


def write_matcaps():
    MATCAP_DIR.mkdir(parents=True, exist_ok=True)
    n = sphere_normals()
    for i, (name, col) in enumerate(matcap_presets(n).items()):
        out = col if name == "Normal" else tonemap(col)
        save_png(MATCAP_DIR / f"{name}.png", out, seed=100 + i)
        print("matcap", name)


# --------------------------------------------------------------------------------------
# Radiance HDR writer (new-style RLE, as understood by every common loader)
# --------------------------------------------------------------------------------------


def to_rgbe(rgb):
    mx = np.max(rgb, axis=-1)
    out = np.zeros(rgb.shape[:-1] + (4,), dtype=np.uint8)
    ok = mx > 1e-32
    m, e = np.frexp(np.where(ok, mx, 1.0))
    scale = np.where(ok, m * 256.0 / np.where(ok, mx, 1.0), 0.0)
    out[..., :3] = np.clip(rgb * scale[..., None], 0, 255).astype(np.uint8)
    out[..., 3] = np.where(ok, e + 128, 0).astype(np.uint8)
    return out


def rle_channel(ch):
    out, i, n = bytearray(), 0, len(ch)
    while i < n:
        run = 1
        while i + run < n and ch[i + run] == ch[i] and run < 127:
            run += 1
        if run >= 4:
            out += bytes([128 + run, int(ch[i])])
            i += run
        else:
            start, lit = i, 0
            while i < n and lit < 128:
                run = 1
                while i + run < n and ch[i + run] == ch[i] and run < 127:
                    run += 1
                if run >= 4:
                    break
                i += 1
                lit += 1
            out += bytes([lit]) + bytes(int(v) for v in ch[start : start + lit])
    return bytes(out)


def write_hdr(path, rgb):
    h, w, _ = rgb.shape
    rgbe = to_rgbe(rgb.astype(np.float64))
    with open(path, "wb") as f:
        f.write(b"#?RADIANCE\n# Generated by tools/gen_default_content.py (AZRA, AGPL-3.0-or-later)\n")
        f.write(b"FORMAT=32-bit_rle_rgbe\n\n")
        f.write(f"-Y {h} +X {w}\n".encode())
        for row in rgbe:
            f.write(bytes([2, 2, w >> 8, w & 255]))
            for c in range(4):
                f.write(rle_channel(row[:, c]))


def equirect_dirs(w, h):
    u = (np.arange(w) + 0.5) / w
    v = (np.arange(h) + 0.5) / h
    lon = (u * 2 - 1) * np.pi
    lat = (0.5 - v) * np.pi
    lon, lat = np.meshgrid(lon, lat)
    return np.stack([np.cos(lat) * np.sin(lon), np.sin(lat), np.cos(lat) * np.cos(lon)], axis=-1)


def write_hdrs():
    HDR_DIR.mkdir(parents=True, exist_ok=True)
    d = equirect_dirs(1024, 512)
    # The app loads HDR_Labs/HDR_Gradient/neutral_dark_0.003.hdr as its default environment
    # (app_update.rs, content_library.rs, library_scan.rs), with environment_dark_gradient.hdr
    # as a fallback: those two file names are kept so no code has to change. Despite the
    # historical names, "neutral_dark_0.003" is the balanced studio default.
    variants = {
        "neutral_dark_0.003": dict(key=6.0, fill=1.2, rim=2.5, ambient=1.0),
        "environment_dark_gradient": dict(key=1.5, fill=0.3, rim=0.8, ambient=0.25),
        "studio_bright_softbox": dict(key=14.0, fill=2.5, rim=4.0, ambient=1.3),
    }
    for name, kw in variants.items():
        write_hdr(HDR_DIR / f"{name}.hdr", studio_env(d, **kw))
        print("hdr", name)


if __name__ == "__main__":
    write_matcaps()
    write_hdrs()
