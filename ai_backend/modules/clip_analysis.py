# ai_backend/modules/clip_analysis.py — CLIP-based material detection
#
# Uses OpenAI CLIP (ViT-B/32) to classify reference images into material
# categories and estimate PBR properties from the classification scores.

import os
import numpy as np
from typing import Optional

_model = None
_preprocess = None
_device = "cpu"

# Material categories with associated PBR defaults
MATERIAL_PROFILES = {
    "polished metal":     {"roughness": 0.1, "metallic": 1.0, "albedo": [0.8, 0.8, 0.85]},
    "brushed metal":      {"roughness": 0.35, "metallic": 1.0, "albedo": [0.7, 0.7, 0.72]},
    "rusted metal":       {"roughness": 0.75, "metallic": 0.6, "albedo": [0.45, 0.25, 0.15]},
    "gold":               {"roughness": 0.2, "metallic": 1.0, "albedo": [1.0, 0.76, 0.33]},
    "copper":             {"roughness": 0.25, "metallic": 1.0, "albedo": [0.95, 0.64, 0.54]},
    "smooth plastic":     {"roughness": 0.15, "metallic": 0.0, "albedo": [0.7, 0.7, 0.7]},
    "rough plastic":      {"roughness": 0.6, "metallic": 0.0, "albedo": [0.5, 0.5, 0.5]},
    "rubber":             {"roughness": 0.85, "metallic": 0.0, "albedo": [0.15, 0.15, 0.15]},
    "polished wood":      {"roughness": 0.3, "metallic": 0.0, "albedo": [0.55, 0.35, 0.2]},
    "rough wood":         {"roughness": 0.75, "metallic": 0.0, "albedo": [0.45, 0.3, 0.18]},
    "smooth stone":       {"roughness": 0.4, "metallic": 0.0, "albedo": [0.55, 0.55, 0.5]},
    "rough stone":        {"roughness": 0.85, "metallic": 0.0, "albedo": [0.4, 0.38, 0.35]},
    "marble":             {"roughness": 0.2, "metallic": 0.0, "albedo": [0.9, 0.88, 0.85]},
    "concrete":           {"roughness": 0.9, "metallic": 0.0, "albedo": [0.5, 0.48, 0.45]},
    "brick":              {"roughness": 0.8, "metallic": 0.0, "albedo": [0.6, 0.3, 0.2]},
    "ceramic":            {"roughness": 0.15, "metallic": 0.0, "albedo": [0.85, 0.85, 0.82]},
    "glass":              {"roughness": 0.05, "metallic": 0.0, "albedo": [0.95, 0.95, 0.95]},
    "leather":            {"roughness": 0.65, "metallic": 0.0, "albedo": [0.35, 0.2, 0.12]},
    "fabric":             {"roughness": 0.9, "metallic": 0.0, "albedo": [0.5, 0.45, 0.4]},
    "sand":               {"roughness": 0.95, "metallic": 0.0, "albedo": [0.76, 0.7, 0.5]},
    "ice":                {"roughness": 0.1, "metallic": 0.0, "albedo": [0.8, 0.9, 0.95]},
    "clay":               {"roughness": 0.7, "metallic": 0.0, "albedo": [0.65, 0.45, 0.3]},
}


def load_clip(device: str = "cpu"):
    """Load CLIP model (ViT-B/32). Downloads on first use (~340MB)."""
    global _model, _preprocess, _device
    if _model is not None:
        return

    try:
        import clip
        import torch
    except ImportError:
        raise ImportError("pip install git+https://github.com/openai/CLIP.git torch")

    _device = device
    cache_dir = os.path.join(os.path.dirname(__file__), "..", "models_cache")
    os.makedirs(cache_dir, exist_ok=True)

    _model, _preprocess = clip.load("ViT-B/32", device=_device, download_root=cache_dir)
    _model.eval()


def analyze_image(image_bytes: bytes) -> dict:
    """
    Analyze an image and return material classification + PBR properties.

    Returns:
        {
            "top_material": str,
            "confidence": float,
            "scores": {material_name: float, ...},  (top 5)
            "pbr": {"roughness": float, "metallic": float, "albedo": [r,g,b]}
        }
    """
    import torch
    import clip
    from PIL import Image
    import io

    load_clip()

    img = Image.open(io.BytesIO(image_bytes)).convert("RGB")
    img_tensor = _preprocess(img).unsqueeze(0).to(_device)

    # Build text prompts: "a photo of {material}"
    material_names = list(MATERIAL_PROFILES.keys())
    text_prompts = [f"a photo of {m}" for m in material_names]
    text_tokens = clip.tokenize(text_prompts).to(_device)

    with torch.no_grad():
        img_features = _model.encode_image(img_tensor)
        txt_features = _model.encode_text(text_tokens)
        img_features = img_features / img_features.norm(dim=-1, keepdim=True)
        txt_features = txt_features / txt_features.norm(dim=-1, keepdim=True)
        similarity = (img_features @ txt_features.T).squeeze(0)
        probs = similarity.softmax(dim=0).cpu().numpy()

    # Sort by score
    ranked = sorted(zip(material_names, probs), key=lambda x: -x[1])

    # Weighted PBR blend from top-3 materials
    top3 = ranked[:3]
    total_w = sum(s for _, s in top3)
    pbr = {"roughness": 0.0, "metallic": 0.0, "albedo": [0.0, 0.0, 0.0]}
    for name, score in top3:
        w = score / total_w
        p = MATERIAL_PROFILES[name]
        pbr["roughness"] += p["roughness"] * w
        pbr["metallic"] += p["metallic"] * w
        for i in range(3):
            pbr["albedo"][i] += p["albedo"][i] * w

    # Round for clean output
    pbr["roughness"] = round(float(pbr["roughness"]), 3)
    pbr["metallic"] = round(float(pbr["metallic"]), 3)
    pbr["albedo"] = [round(float(v), 3) for v in pbr["albedo"]]

    return {
        "top_material": ranked[0][0],
        "confidence": round(float(ranked[0][1]), 3),
        "scores": {n: round(float(s), 3) for n, s in ranked[:5]},
        "pbr": pbr,
    }


def text_to_profile(description: str) -> dict:
    """
    Convert a text description to PBR material properties using keyword matching
    against known material profiles. Falls back to a neutral default.

    Returns:
        {"roughness": float, "metallic": float, "albedo": [r,g,b], "matched": str}
    """
    desc = description.lower().strip()

    # Score each profile by keyword overlap
    best_name = None
    best_score = 0
    for name in MATERIAL_PROFILES:
        words = name.split()
        score = sum(1 for w in words if w in desc)
        # Bonus for exact substring match
        if name in desc:
            score += 2
        if score > best_score:
            best_score = score
            best_name = name

    if best_name and best_score > 0:
        p = MATERIAL_PROFILES[best_name]
        return {
            "roughness": p["roughness"],
            "metallic": p["metallic"],
            "albedo": p["albedo"],
            "matched": best_name,
        }

    # Keyword heuristics for properties not in profiles
    roughness = 0.5
    metallic = 0.0
    albedo = [0.5, 0.5, 0.5]

    if any(w in desc for w in ["shiny", "polished", "glossy", "mirror", "smooth"]):
        roughness = 0.15
    elif any(w in desc for w in ["rough", "matte", "coarse", "grainy"]):
        roughness = 0.8
    if any(w in desc for w in ["metal", "steel", "iron", "aluminum", "chrome"]):
        metallic = 1.0
        albedo = [0.7, 0.7, 0.72]
    if any(w in desc for w in ["red"]):
        albedo = [0.7, 0.15, 0.1]
    elif any(w in desc for w in ["blue"]):
        albedo = [0.1, 0.2, 0.7]
    elif any(w in desc for w in ["green"]):
        albedo = [0.15, 0.55, 0.15]
    elif any(w in desc for w in ["white"]):
        albedo = [0.9, 0.9, 0.9]
    elif any(w in desc for w in ["black", "dark"]):
        albedo = [0.05, 0.05, 0.05]

    return {
        "roughness": roughness,
        "metallic": metallic,
        "albedo": albedo,
        "matched": None,
    }
