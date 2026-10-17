#!/usr/bin/env python3
"""Download free (CC0) PBR materials from ambientCG into assets/PBR/.

The materials are NOT stored in this repository: a single 4K material weighs 30-120 MB. This
script fetches the ones listed in tools/content_manifest.json, at the resolution you choose,
checks each download against the size announced by ambientCG, and records its provenance in
assets/PBR/SOURCES.md. The assets/PBR/ folder is git-ignored.

Usage (from the repository root):
    python tools/fetch_content.py --dry-run          # show sizes only, download nothing
    python tools/fetch_content.py                    # download at 2K (default)
    python tools/fetch_content.py --res 4K           # download at 4K
    python tools/fetch_content.py --only Metal009 Wood096 --res 4K

Each ambientCG material contains colour, normal, roughness, displacement and (often) ambient
occlusion/metalness maps, i.e. the layout AZRA's material import and library understand.

Needs only the Python standard library.
"""
import argparse
import datetime
import io
import json
import sys
import urllib.parse
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "tools" / "content_manifest.json"
OUT_DIR = ROOT / "assets" / "PBR"
API = "https://ambientcg.com/api/v2/full_json"
USER_AGENT = "AZRA-content-fetch (https://github.com/atatatata01/Azra3D)"


def http_get(url, timeout=120):
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    return urllib.request.urlopen(req, timeout=timeout)


def find_downloads(node, out):
    """Collect every dict that looks like a download entry (has 'attribute' and 'downloadLink')."""
    if isinstance(node, dict):
        if "attribute" in node and "downloadLink" in node:
            out.append(node)
        for v in node.values():
            find_downloads(v, out)
    elif isinstance(node, list):
        for v in node:
            find_downloads(v, out)


def lookup(asset_id, res):
    url = f"{API}?{urllib.parse.urlencode({'id': asset_id, 'type': 'Material', 'include': 'downloadData'})}"
    with http_get(url) as r:
        data = json.load(r)
    assets = data.get("foundAssets") or []
    if not assets:
        raise RuntimeError(f"{asset_id}: not found on ambientCG")
    downloads = []
    find_downloads(assets[0], downloads)
    wanted = f"{res}-JPG"
    for d in downloads:
        if d.get("attribute") == wanted:
            return d
    available = sorted({d.get("attribute") for d in downloads})
    raise RuntimeError(f"{asset_id}: no {wanted} download (available: {', '.join(map(str, available))})")


def human(n):
    return f"{n / 1e6:.1f} MB"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--res", default="2K", choices=["1K", "2K", "4K", "8K"], help="resolution (default 2K)")
    ap.add_argument("--only", nargs="*", help="only these asset ids (default: the whole manifest)")
    ap.add_argument("--dry-run", action="store_true", help="list sizes, download nothing")
    args = ap.parse_args()

    ids = args.only or json.loads(MANIFEST.read_text(encoding="utf-8"))["ambientcg_materials"]
    plan, total = [], 0
    for asset_id in ids:
        try:
            d = lookup(asset_id, args.res)
        except Exception as e:  # keep going: report every problem at the end
            print(f"  SKIP {e}")
            continue
        size = int(d.get("size") or 0)
        total += size
        plan.append((asset_id, d, size))
        print(f"  {asset_id:18s} {args.res}  {human(size):>10s}")
    print(f"Total: {human(total)} for {len(plan)} material(s) at {args.res}")
    if args.dry_run or not plan:
        return 0

    OUT_DIR.mkdir(parents=True, exist_ok=True)
    log = OUT_DIR / "SOURCES.md"
    if not log.exists():
        log.write_text(
            "# Downloaded content (CC0)\n\nFetched by `tools/fetch_content.py` from "
            "[ambientCG](https://ambientcg.com) (CC0 1.0, https://docs.ambientcg.com/license/).\n\n"
            "| Asset | Resolution | Bytes | Date | Source |\n|---|---|---|---|---|\n",
            encoding="utf-8",
        )
    for asset_id, d, size in plan:
        dest = OUT_DIR / asset_id
        if dest.exists() and any(dest.iterdir()):
            print(f"  {asset_id}: already present, skipped")
            continue
        link = d["downloadLink"]
        print(f"  {asset_id}: downloading {human(size)} ...", flush=True)
        buf = io.BytesIO()
        with http_get(link, timeout=600) as r:
            while True:
                chunk = r.read(1 << 20)
                if not chunk:
                    break
                buf.write(chunk)
        got = buf.getbuffer().nbytes
        if size and got != size:
            print(f"  {asset_id}: ERROR size mismatch (got {got}, expected {size}), not extracted")
            continue
        dest.mkdir(parents=True, exist_ok=True)
        with zipfile.ZipFile(buf) as z:
            z.extractall(dest)
        with log.open("a", encoding="utf-8") as f:
            f.write(f"| {asset_id} | {args.res} | {got} | {datetime.date.today()} | {link} |\n")
        print(f"  {asset_id}: ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
