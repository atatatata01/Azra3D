# occt_fillet_ref.py — FreeCAD/OCCT reference for AZRA NURBS fillets
# Run:  freecadcmd occt_fillet_ref.py
#
# Writes refs/<case>.json. Commit JSON. NEVER regen only to silence AZRA.
# Case matrix: FILLET_EDGE_CASES.md + OCCT_ORACLE_HARNESS.md
#
# Solids:
#   unit box          — AllSame corners only
#   L-bracket         — mixed concavity (OnSame / OnDiff candidates)
#   boss-on-plate     — junction + mixed concavity

import json
import os
import sys

import FreeCAD  # noqa: F401
import Part

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "refs")
os.makedirs(OUT, exist_ok=True)

FREECAD_VERSION = getattr(FreeCAD, "Version", lambda: ["?"])()
if callable(FREECAD_VERSION):
    FREECAD_VERSION = FREECAD_VERSION()
VERSION_STR = ".".join(str(x) for x in FREECAD_VERSION[:3]) if FREECAD_VERSION else "unknown"


def surface_type(face):
    t = face.Surface.TypeId
    if "Geom" in t:
        return t.rsplit("Geom", 1)[-1]
    return t


def edge_report(shape, label="shape"):
    rows = []
    print("--- edges:", label, "---")
    for i, e in enumerate(shape.Edges):
        vs = e.Vertexes
        p0, p1 = vs[0].Point, vs[1].Point
        rows.append(
            {
                "i": i,
                "a": [round(p0.x, 6), round(p0.y, 6), round(p0.z, 6)],
                "b": [round(p1.x, 6), round(p1.y, 6), round(p1.z, 6)],
            }
        )
        print(
            "edge",
            i,
            "(%g,%g,%g)" % (p0.x, p0.y, p0.z),
            "->",
            "(%g,%g,%g)" % (p1.x, p1.y, p1.z),
        )
    return rows


def emit(name, shape, dev=1e-3, meta=None):
    counts = {}
    for f in shape.Faces:
        k = surface_type(f)
        counts[k] = counts.get(k, 0) + 1
    verts, tris = shape.tessellate(dev)
    payload = {
        "case": name,
        "freecad_version": VERSION_STR,
        "faces": counts,
        "total": len(shape.Faces),
        "vertices": [[round(c, 6) for c in (v.x, v.y, v.z)] for v in verts],
        "triangles": [list(t) for t in tris],
    }
    if meta:
        payload["meta"] = meta
    path = os.path.join(OUT, name + ".json")
    with open(path, "w", encoding="utf-8") as fp:
        json.dump(payload, fp, indent=1)
    print(name, counts, len(verts), "verts ->", path)
    return counts


def try_fillet(name, shape, r, edge_idx, meta=None):
    edges = shape.Edges if edge_idx is None else [shape.Edges[i] for i in edge_idx]
    try:
        shaped = shape.makeFillet(r, edges)
    except Exception as e:
        print("FAIL", name, e)
        # Still write a stub so the batch list is complete and failures are visible
        path = os.path.join(OUT, name + ".json")
        with open(path, "w", encoding="utf-8") as fp:
            json.dump(
                {
                    "case": name,
                    "error": str(e),
                    "freecad_version": VERSION_STR,
                    "meta": meta or {},
                },
                fp,
                indent=1,
            )
        return None
    return emit(name, shaped, meta=meta)


# ── Solids ─────────────────────────────────────────────────────────────


def solid_unit_box():
    """Unit box [0,1]^3 — AllSame corners only."""
    return Part.makeBox(1, 1, 1)


def solid_l_bracket():
    """
    L-bracket: base plate [0,2]x[0,1]x[0,0.3] + upright [0,0.3]x[0,1]x[0,2].
    Re-entrant edges along the inner corner produce mixed concavity (OnSame/OnDiff candidates).
    """
    base = Part.makeBox(2.0, 1.0, 0.3)
    upright = Part.makeBox(0.3, 1.0, 2.0)
    # Fuse — OCCT boolean; fillet after
    return base.fuse(upright).removeSplitter()


def solid_boss_on_plate():
    """
    Boss on plate: plate [0,2]x[0,2]x[0,0.2] + boss [0.5,1.5]x[0.5,1.5]x[0.2,0.7].
    Junction ring and vertical boss edges mix convex/concave at top vertices of the plate hole-ring.
    """
    plate = Part.makeBox(2.0, 2.0, 0.2)
    boss = Part.makeBox(1.0, 1.0, 0.5)
    boss.translate(FreeCAD.Vector(0.5, 0.5, 0.2))
    return plate.fuse(boss).removeSplitter()


def edges_near_point(shape, point, tol=1e-4):
    """Return edge indices with an endpoint near `point` (FreeCAD.Vector or (x,y,z))."""
    if not isinstance(point, FreeCAD.Vector):
        point = FreeCAD.Vector(*point)
    out = []
    for i, e in enumerate(shape.Edges):
        for v in e.Vertexes:
            if v.Point.distanceToPoint(point) < tol:
                out.append(i)
                break
    return out


def main():
    print("FreeCAD", VERSION_STR)

    # ── Edge maps (commit for AZRA position mapping) ──
    box = solid_unit_box()
    lb = solid_l_bracket()
    boss = solid_boss_on_plate()

    with open(os.path.join(OUT, "_edge_map_unit_box.json"), "w", encoding="utf-8") as fp:
        json.dump({"edges": edge_report(box, "unit_box"), "solid": "box"}, fp, indent=1)
    with open(os.path.join(OUT, "_edge_map_l_bracket.json"), "w", encoding="utf-8") as fp:
        json.dump({"edges": edge_report(lb, "l_bracket"), "solid": "l_bracket"}, fp, indent=1)
    with open(os.path.join(OUT, "_edge_map_boss_on_plate.json"), "w", encoding="utf-8") as fp:
        json.dump({"edges": edge_report(boss, "boss_on_plate"), "solid": "boss"}, fp, indent=1)

    summary = {}

    # ── Tier 0 + box Tier 1 (AllSame) —  FILLET_EDGE_CASES 0.x / box rows ──
    # Edge indices: verify against _edge_map_unit_box.json after first run.
    cases_box = [
        ("single_edge_r02", 0.2, [0], {"tier": "0.1", "concavity": "n/a"}),
        ("two_edges_at_vertex", 0.2, [0, 4], {"tier": "0.5", "corner": "Two", "concavity": "AllSame"}),
        ("three_edges_at_vertex", 0.2, [0, 4, 8], {"tier": "0.3", "corner": "Three", "concavity": "AllSame"}),
        ("all_edges_r005", 0.05, None, {"tier": "box_loop"}),
        ("all_edges_r02", 0.2, None, {"tier": "box_loop"}),
        ("all_edges_r045", 0.45, None, {"tier": "1.8", "note": "near-max"}),
    ]
    for name, r, idx, meta in cases_box:
        c = try_fillet(name, solid_unit_box(), r, idx, meta=meta)
        if c:
            summary[name] = c

    # ── L-bracket: pick edges at the re-entrant inner corner ──
    # Inner corner approx (0.3, 0.5, 0.3) — refine after edge map print.
    inner = FreeCAD.Vector(0.3, 0.5, 0.3)
    lb_edges = edges_near_point(lb, inner, tol=0.05)
    print("L-bracket edges near inner", list(inner), "->", lb_edges)
    if len(lb_edges) >= 2:
        try_fillet(
            "lbracket_two_edges_reentrant",
            solid_l_bracket(),
            0.12,
            lb_edges[:2],
            meta={"tier": "1.2/1.3", "solid": "l_bracket", "concavity": "OnSame|OnDiff?"},
        )
    if len(lb_edges) >= 3:
        try_fillet(
            "lbracket_three_edges_reentrant",
            solid_l_bracket(),
            0.12,
            lb_edges[:3],
            meta={"tier": "1.2/1.3", "solid": "l_bracket", "corner": "Three"},
        )
    # Single concave-ish edge on L (first edge near inner)
    if lb_edges:
        try_fillet(
            "lbracket_one_edge",
            solid_l_bracket(),
            0.12,
            [lb_edges[0]],
            meta={"tier": "1.1/1.4", "solid": "l_bracket"},
        )

    # ── Boss-on-plate: junction vertex on plate top at boss corner ──
    junc = FreeCAD.Vector(0.5, 0.5, 0.2)
    boss_edges = edges_near_point(boss, junc, tol=0.05)
    print("Boss edges near junction", list(junc), "->", boss_edges)
    if len(boss_edges) >= 2:
        try_fillet(
            "boss_two_edges_junction",
            solid_boss_on_plate(),
            0.08,
            boss_edges[:2],
            meta={"tier": "1.2", "solid": "boss_on_plate", "concavity": "OnSame|OnDiff?"},
        )
    if len(boss_edges) >= 3:
        try_fillet(
            "boss_three_edges_junction",
            solid_boss_on_plate(),
            0.08,
            boss_edges[:3],
            meta={"tier": "1.2/1.3", "solid": "boss_on_plate"},
        )

    # Summary face counts for expected_tier1 merge (human review before commit)
    with open(os.path.join(OUT, "_batch_summary.json"), "w", encoding="utf-8") as fp:
        json.dump(
            {
                "freecad_version": VERSION_STR,
                "note": "Review OnSame/OnDiff cases by hand; cube rows may feed expected_tier1.json",
                "box_counts": summary,
            },
            fp,
            indent=1,
        )

    print("done. Commit refs/*.json after review. Do not regen only to silence AZRA.")
    print("Cube cannot produce OnSame/OnDiff — L-bracket + boss rows are required for 1.2/1.3.")


if __name__ == "__main__":
    main()
    sys.exit(0)
