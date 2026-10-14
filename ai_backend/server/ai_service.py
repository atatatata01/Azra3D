# ai_backend/server/ai_service.py — FourthLayer AI Service
#
# Provides material analysis and generation endpoints:
#   POST /analyze_image    — CLIP-based material detection from image
#   POST /text_to_profile  — Text description → PBR material properties
#   GET  /health           — Service status
#
# Run:  python ai_service.py [--port 7861] [--device cpu]

import io, os, sys, json, base64, time, argparse, threading

# Add parent so we can import modules/
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

try:
    from fastapi import FastAPI, UploadFile, File, Form
    from fastapi.responses import JSONResponse
    import uvicorn
except ImportError:
    print("ERROR: pip install fastapi uvicorn"); sys.exit(1)

app = FastAPI(title="FourthLayer AI Service", version="1.0")
_clip_loaded = False
_clip_lock = threading.Lock()
_device = "cpu"


# ============================================================
# Health
# ============================================================
@app.get("/health")
def health():
    return {"status": "ok", "clip_loaded": _clip_loaded, "device": _device}


# ============================================================
# Analyze Image — CLIP material classification
# ============================================================
@app.post("/analyze_image")
async def analyze_image(
    image: UploadFile = File(None),
    image_b64: str = Form(None),
):
    """
    Classify a reference image into material categories and return PBR properties.
    Accepts either a file upload or base64-encoded image data.
    """
    global _clip_loaded
    from modules.clip_analysis import analyze_image as _analyze, load_clip

    # Lazy-load CLIP on first request
    with _clip_lock:
        if not _clip_loaded:
            try:
                load_clip(_device)
                _clip_loaded = True
            except ImportError as e:
                return JSONResponse(status_code=503, content={
                    "error": str(e),
                    "hint": "Install CLIP: pip install git+https://github.com/openai/CLIP.git"
                })

    # Get image bytes from upload or base64
    if image is not None:
        img_bytes = await image.read()
    elif image_b64 is not None:
        try:
            img_bytes = base64.b64decode(image_b64)
        except Exception:
            return JSONResponse(status_code=400, content={"error": "Invalid base64"})
    else:
        return JSONResponse(status_code=400, content={"error": "Provide 'image' file or 'image_b64' form field"})

    t0 = time.time()
    try:
        result = _analyze(img_bytes)
    except Exception as e:
        return JSONResponse(status_code=500, content={"error": str(e)})
    result["elapsed_ms"] = round((time.time() - t0) * 1000, 1)
    return result


# ============================================================
# Text to Profile — keyword-based material property estimation
# ============================================================
@app.post("/text_to_profile")
async def text_to_profile(description: str = Form(...)):
    """
    Convert a text description (e.g. "brushed copper") to PBR properties.
    Uses keyword matching against known material profiles.
    """
    from modules.clip_analysis import text_to_profile as _text_to_profile

    result = _text_to_profile(description)
    return result


# ============================================================
# Material Profiles — list all known profiles
# ============================================================
@app.get("/profiles")
def list_profiles():
    from modules.clip_analysis import MATERIAL_PROFILES
    return {"profiles": MATERIAL_PROFILES}


# ============================================================
# Main
# ============================================================
if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="FourthLayer AI Service")
    parser.add_argument("--port", type=int, default=7861)
    parser.add_argument("--device", type=str, default="cpu", choices=["cpu", "cuda"])
    parser.add_argument("--host", type=str, default="127.0.0.1")
    args = parser.parse_args()
    _device = args.device
    print(f"[FourthLayer AI] Starting on {args.host}:{args.port} (device={_device})")
    uvicorn.run(app, host=args.host, port=args.port, log_level="info")
