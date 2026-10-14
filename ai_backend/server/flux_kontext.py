# flux_kontext.py — Flux.1 Kontext pipeline for Azra (local ComfyUI weights)
#
# Fully offline when possible:
#   transformer  ← ComfyUI diffusion_models/flux1-kontext-dev.safetensors
#   vae          ← ComfyUI vae/ae.safetensors
#   clip_l       ← ComfyUI clip/clip_l.safetensors
#   t5           ← ComfyUI clip/t5xxl_*.safetensors
#   tokenizers   ← openai/clip-vit-large-patch14 + google/t5-v1_1-xxl  (not gated)
#   configs      ← ai_backend/server/flux_configs/  (bundled, no HF)
#
# Used for AI Texture + Neural Beauty. Azra owns lifecycle via spawn.

from __future__ import annotations

import gc
import json
import os
from pathlib import Path
from typing import Any, Optional, Tuple

_HERE = Path(__file__).resolve().parent
_CONFIGS = _HERE / "flux_configs"

# ── Local ComfyUI layout ─────────────────────────────────────────────────────
_COMFY = Path(os.environ.get("AZRA_COMFY_MODELS", r"C:\Users\Proprio\Documents\ComfyUI\models"))

KONTEXT_TRANSFORMERS = [
    _COMFY / "diffusion_models" / "flux1-kontext-dev.safetensors",
    _COMFY / "checkpoints" / "flux1-dev-fp8.safetensors",  # last-resort not kontext
]

VAE_CANDIDATES = [
    _COMFY / "vae" / "ae.safetensors",
]

CLIP_L_CANDIDATES = [
    _COMFY / "clip" / "clip_l.safetensors",
    _COMFY / "clip" / "clip_l_hidream.safetensors",
]

T5_CANDIDATES = [
    # Prefer fp16 for transformers; fp8-scaled needs extra cast
    _COMFY / "clip" / "t5xxl_fp16.safetensors",
    _COMFY / "text_encoders" / "t5xxl_fp16.safetensors",
    _COMFY / "clip" / "t5xxl_fp8_e4m3fn_scaled.safetensors",
    _COMFY / "text_encoders" / "t5xxl_fp8_e4m3fn_scaled.safetensors",
]


def _first_file(paths) -> Optional[Path]:
    for p in paths:
        if p.is_file() and p.suffix.lower() in (".safetensors", ".ckpt"):
            # skip gguf for diffusers path
            if p.suffix.lower() == ".gguf":
                continue
            return p
    return None


def discover_kontext_weights() -> dict:
    tr = _first_file(KONTEXT_TRANSFORMERS)
    return {
        "transformer": str(tr) if tr else "",
        "vae": str(_first_file(VAE_CANDIDATES) or ""),
        "clip_l": str(_first_file(CLIP_L_CANDIDATES) or ""),
        "t5": str(_first_file(T5_CANDIDATES) or ""),
        "ready": bool(tr),
        "comfy_root": str(_COMFY),
        "configs": str(_CONFIGS),
    }


def _dtype_device():
    import torch

    if torch.cuda.is_available():
        if torch.cuda.is_bf16_supported():
            return torch.bfloat16, "cuda"
        return torch.float16, "cuda"
    return torch.float32, "cpu"


def _empty_cuda():
    try:
        import torch

        if torch.cuda.is_available():
            torch.cuda.empty_cache()
        gc.collect()
    except Exception:
        pass


def _load_json(path: Path) -> dict:
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)


def _remap_clip_keys(sd: dict) -> dict:
    """Normalize Comfy / OpenCLIP / transformers CLIP key names → CLIPTextModel."""
    out = {}
    for k, v in sd.items():
        nk = k
        if nk.startswith("cond_stage_model.transformer."):
            nk = nk[len("cond_stage_model.transformer.") :]
        if nk.startswith("transformer."):
            nk = nk[len("transformer.") :]
        if nk.startswith("text_model."):
            pass
        elif nk.startswith("encoder.") or nk.startswith("embeddings.") or nk.startswith("final_layer_norm."):
            nk = "text_model." + nk
        # drop projection head if present (Flux only needs text tower)
        if "text_projection" in nk or nk.startswith("visual."):
            continue
        out[nk] = v
    return out


def _remap_t5_keys(sd: dict) -> dict:
    """Normalize Comfy T5 keys → T5EncoderModel (encoder only)."""
    out = {}
    for k, v in sd.items():
        nk = k
        if nk.startswith("encoder."):
            pass
        elif nk.startswith("shared."):
            pass
        elif nk.startswith("model.encoder."):
            nk = nk[len("model.") :]
        elif nk.startswith("t5xxl.transformer."):
            nk = nk[len("t5xxl.transformer.") :]
        # drop decoder weights if present
        if nk.startswith("decoder.") or ".decoder." in nk:
            continue
        out[nk] = v
    return out


def _load_clip_l(path: Path, dtype):
    import torch
    from transformers import CLIPTextConfig, CLIPTextModel
    from safetensors.torch import load_file

    config = CLIPTextConfig(
        vocab_size=49408,
        hidden_size=768,
        intermediate_size=3072,
        num_hidden_layers=12,
        num_attention_heads=12,
        max_position_embeddings=77,
        hidden_act="quick_gelu",
        layer_norm_eps=1e-5,
        dropout=0.0,
        attention_dropout=0.0,
        initializer_range=0.02,
        initializer_factor=1.0,
        pad_token_id=1,
        bos_token_id=49406,
        eos_token_id=49407,
        projection_dim=768,
    )
    model = CLIPTextModel(config)
    raw = load_file(str(path))
    sd = _remap_clip_keys(raw)
    missing, unexpected = model.load_state_dict(sd, strict=False)
    if len(missing) > 50:
        print(f"[flux_kontext] CLIP missing {len(missing)} keys (first 5: {missing[:5]})")
    if unexpected:
        print(f"[flux_kontext] CLIP unexpected {len(unexpected)} keys")
    model = model.to(dtype=dtype)
    model.eval()
    return model


def _load_t5(path: Path, dtype):
    import torch
    from transformers import T5Config, T5EncoderModel
    from safetensors.torch import load_file

    # google/t5-v1_1-xxl encoder config
    config = T5Config(
        vocab_size=32128,
        d_model=4096,
        d_kv=64,
        d_ff=10240,
        num_layers=24,
        num_heads=64,
        relative_attention_num_buckets=32,
        relative_attention_max_distance=128,
        dropout_rate=0.1,
        layer_norm_epsilon=1e-6,
        initializer_factor=1.0,
        feed_forward_proj="gated-gelu",
        is_encoder_decoder=True,
        use_cache=True,
        pad_token_id=0,
        eos_token_id=1,
        dense_act_fn="gelu_new",
        is_gated_act=True,
    )
    model = T5EncoderModel(config)
    raw = load_file(str(path))
    # fp8 scaled tensors may arrive as float8 — cast
    casted = {}
    f8 = getattr(torch, "float8_e4m3fn", None)
    for k, v in raw.items():
        if f8 is not None and v.dtype == f8:
            casted[k] = v.to(torch.float16)
        else:
            casted[k] = v
    sd = _remap_t5_keys(casted)
    missing, unexpected = model.load_state_dict(sd, strict=False)
    if len(missing) > 50:
        print(f"[flux_kontext] T5 missing {len(missing)} keys (first 5: {missing[:5]})")
    if unexpected:
        print(f"[flux_kontext] T5 unexpected {len(unexpected)} (sample {unexpected[:5]})")
    model = model.to(dtype=dtype)
    model.eval()
    return model


def _load_tokenizers():
    """Tokenizers are small and not gated — openai CLIP + google T5."""
    from transformers import CLIPTokenizer, T5TokenizerFast

    # Prefer offline if already cached; otherwise download (public models)
    try:
        tok = CLIPTokenizer.from_pretrained("openai/clip-vit-large-patch14", local_files_only=True)
    except Exception:
        tok = CLIPTokenizer.from_pretrained("openai/clip-vit-large-patch14")
    try:
        tok2 = T5TokenizerFast.from_pretrained("google/t5-v1_1-xxl", local_files_only=True)
    except Exception:
        try:
            tok2 = T5TokenizerFast.from_pretrained("google/flan-t5-xxl", local_files_only=True)
        except Exception:
            try:
                tok2 = T5TokenizerFast.from_pretrained("google/t5-v1_1-xxl")
            except Exception:
                tok2 = T5TokenizerFast.from_pretrained("google/flan-t5-xxl")
    # Hard-cap — some cached tokenizers ship with huge model_max_length
    # which crashes tokenizers' enable_truncation (OverflowError).
    tok.model_max_length = 77
    if hasattr(tok, "init_kwargs"):
        tok.init_kwargs["model_max_length"] = 77
    tok2.model_max_length = 512
    if hasattr(tok2, "init_kwargs"):
        tok2.init_kwargs["model_max_length"] = 512
    return tok, tok2


def _load_transformer_local(path: Path, dtype):
    from diffusers import FluxTransformer2DModel

    cfg_dir = _CONFIGS / "transformer"
    if not (cfg_dir / "config.json").is_file():
        raise FileNotFoundError(f"Missing bundled config: {cfg_dir / 'config.json'}")

    # Pass local config directory so diffusers never hits gated HF
    print(f"[flux_kontext] loading transformer from {path.name} with local config …")
    model = FluxTransformer2DModel.from_single_file(
        str(path),
        config=str(cfg_dir),
        torch_dtype=dtype,
        local_files_only=True,
    )
    return model


def _load_vae_local(path: Path, dtype):
    import torch
    from diffusers import AutoencoderKL
    from safetensors.torch import load_file

    cfg_dir = _CONFIGS / "vae"
    print(f"[flux_kontext] loading VAE from {path.name} …")
    cfg = _load_json(cfg_dir / "config.json")
    cfg = {k: v for k, v in cfg.items() if not k.startswith("_")}

    # Avoid meta-device path in from_single_file (breaks .to(dtype) on some versions)
    vae = AutoencoderKL.from_config(cfg)
    raw = load_file(str(path))
    # Flux ae.safetensors is usually already in diffusers key layout
    missing, unexpected = vae.load_state_dict(raw, strict=False)
    if len(missing) > 10:
        try:
            from diffusers.loaders.single_file_utils import convert_ldm_vae_checkpoint

            converted = convert_ldm_vae_checkpoint(raw, vae.config)
            missing, unexpected = vae.load_state_dict(converted, strict=False)
            print(f"[flux_kontext] VAE after LDM convert missing={len(missing)} unexpected={len(unexpected)}")
        except Exception as e:
            print(f"[flux_kontext] VAE convert failed: {e} (missing={len(missing)})")
    else:
        print(f"[flux_kontext] VAE keys ok missing={len(missing)} unexpected={len(unexpected)}")

    # Materialize weights off meta and cast
    vae = vae.to(dtype=dtype)
    vae.eval()
    # Sanity: at least one parameter must be non-zero
    with torch.no_grad():
        n = 0
        s = 0.0
        for p in vae.parameters():
            n += p.numel()
            s += float(p.detach().float().abs().mean().cpu())
            break
        print(f"[flux_kontext] VAE first-param mean-abs={s:.6f} (n0={n})")
    return vae


def _load_scheduler():
    from diffusers import FlowMatchEulerDiscreteScheduler

    cfg_path = _CONFIGS / "scheduler" / "scheduler_config.json"
    cfg = _load_json(cfg_path)
    cfg = {k: v for k, v in cfg.items() if not k.startswith("_")}
    return FlowMatchEulerDiscreteScheduler.from_config(cfg)


def load_flux_kontext_pipeline(prefer_offload: Optional[bool] = None) -> Tuple[Any, str, str]:
    """
    Load FluxKontextPipeline from local ComfyUI weights + bundled configs.
    Returns (pipe, device, note).

    prefer_offload:
      None  → auto (full CUDA if ≥28GB VRAM, else model CPU offload)
      True  → always cpu_offload (slower steps, fits 12–24GB cards)
      False → full GPU (fast, needs ~28GB+ for bf16 stack)
    """
    import torch
    from diffusers import FluxKontextPipeline

    weights = discover_kontext_weights()
    if not weights["ready"]:
        raise RuntimeError(
            "Flux.1 Kontext transformer not found under ComfyUI models.\n"
            f"Expected e.g. {_COMFY / 'diffusion_models' / 'flux1-kontext-dev.safetensors'}\n"
            "Set AZRA_COMFY_MODELS if your ComfyUI path differs."
        )
    if not weights["vae"] or not weights["clip_l"] or not weights["t5"]:
        raise RuntimeError(
            "Missing Flux components (need VAE + CLIP-L + T5):\n"
            f"  vae={weights['vae']}\n  clip_l={weights['clip_l']}\n  t5={weights['t5']}\n"
            "Install ae.safetensors, clip_l.safetensors, t5xxl_fp16.safetensors into ComfyUI/models."
        )

    dtype, device = _dtype_device()
    notes = []
    os.environ.pop("HF_TOKEN", None)
    os.environ.pop("HUGGING_FACE_HUB_TOKEN", None)

    if prefer_offload is None and device == "cuda":
        try:
            total = torch.cuda.get_device_properties(0).total_memory
            # Full stack (transformer~24GB bf16 + T5 + VAE) needs ~28GB+ comfortably
            prefer_offload = total < 28 * (1024 ** 3)
            notes.append(f"vram={total/1024**3:.1f}GB")
        except Exception:
            prefer_offload = True
    if prefer_offload is None:
        prefer_offload = True

    transformer_path = Path(weights["transformer"])
    vae_path = Path(weights["vae"])
    clip_path = Path(weights["clip_l"])
    t5_path = Path(weights["t5"])

    # 1) Transformer (local weights + local config — no gated HF)
    transformer = _load_transformer_local(transformer_path, dtype)
    notes.append(f"transformer={transformer_path.name}")

    # 2) VAE
    vae = _load_vae_local(vae_path, dtype)
    notes.append(f"vae={vae_path.name}")

    # 3) Text encoders
    print(f"[flux_kontext] loading CLIP-L from {clip_path.name} …")
    text_encoder = _load_clip_l(clip_path, dtype)
    notes.append(f"clip={clip_path.name}")

    print(f"[flux_kontext] loading T5 from {t5_path.name} …")
    text_encoder_2 = _load_t5(t5_path, dtype)
    notes.append(f"t5={t5_path.name}")

    # 4) Tokenizers (public, small)
    print("[flux_kontext] loading tokenizers …")
    tokenizer, tokenizer_2 = _load_tokenizers()
    notes.append("tokenizers=openai-clip+google-t5")

    # 5) Scheduler
    scheduler = _load_scheduler()

    pipe = FluxKontextPipeline(
        scheduler=scheduler,
        vae=vae,
        text_encoder=text_encoder,
        tokenizer=tokenizer,
        text_encoder_2=text_encoder_2,
        tokenizer_2=tokenizer_2,
        transformer=transformer,
    )

    if device == "cuda":
        try:
            if prefer_offload:
                pipe.enable_model_cpu_offload()
                notes.append("cpu_offload")
            else:
                pipe.to("cuda")
                notes.append("full_cuda")
        except Exception as e:
            print(f"[flux_kontext] placement: {e}")
            try:
                pipe.enable_model_cpu_offload()
                notes.append("cpu_offload_fallback")
            except Exception:
                pipe.to("cuda")
                notes.append("cuda_fallback")
        try:
            pipe.vae.enable_slicing()
            pipe.vae.enable_tiling()
        except Exception:
            pass
    else:
        pipe.to("cpu")
        notes.append("cpu")

    note = " | ".join(notes)
    print(f"[flux_kontext] LOADED on {device}: {note}")
    return pipe, device, note


def flux_guidance(ui_guidance: float) -> float:
    """Map SD-style CFG (7–8) down to Flux-friendly ~2.0–4.0."""
    g = float(ui_guidance)
    if g > 5.0:
        t = min(1.0, (g - 5.0) / 10.0)
        return 2.0 + t * 2.0
    return max(1.0, min(g, 5.0))


def _make_texture_canvas(width: int, height: int, seed: int):
    import numpy as np
    from PIL import Image

    rng = np.random.RandomState(int(seed) & 0xFFFFFFFF)
    noise = rng.randn(height, width, 3).astype(np.float32) * 12.0 + 128.0
    arr = np.clip(noise, 0, 255).astype(np.uint8)
    return Image.fromarray(arr, mode="RGB")


def run_flux_kontext(
    pipe,
    *,
    prompt: str,
    image=None,
    width: int = 1024,
    height: int = 1024,
    seed: int = 42,
    steps: int = 28,
    guidance: float = 2.5,
    negative_prompt: str = "",
    device: str = "cuda",
):
    """Run FluxKontextPipeline. image=None → soft-noise canvas (texture mode)."""
    import torch
    from PIL import Image

    w = max(256, (int(width) // 16) * 16)
    h = max(256, (int(height) // 16) * 16)
    steps = int(max(4, min(steps, 50)))
    g = flux_guidance(guidance)

    if image is None:
        image = _make_texture_canvas(w, h, seed)
    else:
        if not isinstance(image, Image.Image):
            image = Image.fromarray(image).convert("RGB")
        else:
            image = image.convert("RGB")
        if image.size != (w, h):
            image = image.resize((w, h), Image.Resampling.LANCZOS)

    generator = torch.Generator(device="cpu").manual_seed(int(seed) & 0xFFFFFFFF)

    # Free any leftover VRAM before denoise (PT may have just released the GPU).
    _empty_cuda()

    kwargs = dict(
        image=image,
        prompt=prompt,
        num_inference_steps=steps,
        guidance_scale=g,
        generator=generator,
        height=h,
        width=w,
        max_sequence_length=512,
    )
    if negative_prompt:
        kwargs["negative_prompt"] = negative_prompt
        kwargs["true_cfg_scale"] = 1.0 + min(2.0, max(0.0, (g - 1.0) * 0.35))

    print(f"[flux_kontext] denoise {w}x{h} steps={steps} guidance={g:.2f} …")
    result = pipe(**kwargs)
    out = result.images[0].convert("RGB")
    if out.size != (width, height) and (int(width), int(height)) != (w, h):
        out = out.resize((int(width), int(height)), Image.Resampling.LANCZOS)
    _empty_cuda()
    return out


def unload_pipe(pipe) -> None:
    try:
        del pipe
    except Exception:
        pass
    _empty_cuda()
