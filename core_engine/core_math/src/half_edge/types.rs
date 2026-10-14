// Core data types of the half-edge mesh (split from half_edge.rs — pure relocation).
// HVertex / HalfEdge / HFace / HalfEdgeMesh are BINCODE-SERIALIZED: field order, names,
// types, and serde attributes are VERBATIM from the monolith. Do not reorder or retype.

use glam::Vec3;
use serde::{Serialize, Deserialize};
use super::ids::*;

// ============================================================
// Core data types
// ============================================================

/// Output of [`HalfEdgeMesh::inset_region`]. RUNTIME-ONLY (never serialized — the `HalfEdgeMesh`
/// on-disk shape is unchanged).
#[derive(Clone, Debug, Default)]
pub struct InsetRegionResult {
    /// Faces created by the inset (rim quads + the re-created region faces).
    pub faces: Vec<FaceId>,
    /// ONLY the re-created region faces (the inset "island"), excluding the rim quads. This is what
    /// Blender leaves SELECTED after an inset, so the classic inset -> extrude combo just works.
    /// For **outset** (`thickness < 0`) the island is the original-footprint face; the rim is the
    /// outer segment ready to thicken — callers often select `rim` in that mode instead.
    pub inner: Vec<FaceId>,
    /// Rim strip faces only (annulus between original boundary and the new ring). Empty when the op
    /// refused. For outward inset these are the "segment extend" faces ready to thicken/extrude.
    pub rim: Vec<FaceId>,
    /// Each NEW ring vertex with its even-offset displacement PER UNIT thickness (dirs point
    /// **inward**). The interactive drag slides `pos = orig + dir * thickness` — negative thickness
    /// therefore places the new ring **outside** the original boundary without moving shared body
    /// verts (body corners stay fixed).
    pub boundary: Vec<(VertexId, Vec3)>,
    /// True when this result used outset topology (`thickness < 0` at build): island on original
    /// verts, new ring is the free outer edge of the rim.
    pub outward: bool,
}

/// One boundary loop of a face region, reshaped onto a circle by [`HalfEdgeMesh::circle_region`].
/// RUNTIME-ONLY (never serialized — the `HalfEdgeMesh` on-disk shape is unchanged).
#[derive(Clone, Debug)]
pub struct CircleLoopFit {
    /// Where the circle is actually centred = the loop's vertex CENTROID (see `circle_region` for why
    /// this is NOT the least-squares centre).
    pub center: Vec3,
    /// Best-fit plane normal of the loop (sign is arbitrary; the result does not depend on it).
    pub normal: Vec3,
    /// Circle radius: the least-squares best fit, or the caller's override.
    pub radius: f32,
    /// Each boundary vertex with its UNIT radial direction in the fit plane, so a future radius
    /// handle/drag is just `pos = center + dir * radius`. Paired by vertex IDENTITY — NEVER by ring
    /// order (POLY_POLISH_PLAN §8: `face_vertices` is destination-ordered = rotated by one).
    pub boundary: Vec<(VertexId, Vec3)>,
}

/// Output of [`HalfEdgeMesh::circle_region`]. RUNTIME-ONLY (never serialized).
#[derive(Clone, Debug, Default)]
pub struct CircleRegionResult {
    /// One entry per boundary loop of the region (>1 only for an annulus-like selection).
    /// EMPTY = the op refused the region (not a simple loop / degenerate plane) and changed nothing.
    pub loops: Vec<CircleLoopFit>,
    /// Interior region vertices moved by the relax pass, ascending. Empty when `relax_passes == 0`.
    pub relaxed: Vec<VertexId>,
}

/// Bitmask for poly Mirror modifier / mesh mirror planes.
///
/// **Encoding is bits, never an axis index:**
/// - X = `1`, Y = `2`, Z = `4` (same as the properties-panel checkboxes).
/// - Multi-axis = OR of bits (X|Y = `3` → 4-way mirror).
///
/// `PolyModifier.mirror_axis` stores this as a raw `u8` on disk (no format change). Convert at the
/// eval boundary via [`MirrorAxes::from_bits`]. Call sites that only have an index 0/1/2 use
/// [`MirrorAxes::from_axis_index`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct MirrorAxes(u8);

impl MirrorAxes {
    pub const X: Self = Self(1);
    pub const Y: Self = Self(2);
    pub const Z: Self = Self(4);

    /// Mask to the three axis bits (ignore garbage high bits from disk/UI).
    #[inline]
    pub fn from_bits(bits: u8) -> Self {
        Self(bits & 0b111)
    }

    /// Convert a single axis **index** (0=X, 1=Y, 2=Z) into a one-bit mask.
    /// Used by symmetry pie / tools that speak index, not checkboxes.
    #[inline]
    pub fn from_axis_index(axis_index: u8) -> Self {
        match axis_index.min(2) {
            0 => Self::X,
            1 => Self::Y,
            _ => Self::Z,
        }
    }

    #[inline]
    pub fn bits(self) -> u8 {
        self.0
    }

    #[inline]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[inline]
    pub fn contains_x(self) -> bool {
        self.0 & Self::X.0 != 0
    }

    #[inline]
    pub fn contains_y(self) -> bool {
        self.0 & Self::Y.0 != 0
    }

    #[inline]
    pub fn contains_z(self) -> bool {
        self.0 & Self::Z.0 != 0
    }

    #[inline]
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HVertex {
    pub position: Vec3,
    pub normal: Vec3,
    pub half_edge: HalfEdgeId, // one outgoing half-edge
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HalfEdge {
    pub vertex: VertexId,      // vertex this half-edge points TO
    pub twin: HalfEdgeId,      // opposite half-edge (NO_TWIN for boundary)
    pub next: HalfEdgeId,      // next half-edge around the face (CCW)
    pub prev: HalfEdgeId,      // previous half-edge
    pub face: FaceId,          // face to the left (NO_FACE for boundary)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HFace {
    pub half_edge: HalfEdgeId, // one half-edge on this face's boundary
    pub normal: Vec3,
    #[serde(default)]
    pub material_id: u32,      // 0 = use body default material, >0 = scene material index
}

// ============================================================
// Free-list membership mirror — RUNTIME-ONLY, NEVER SERIALIZED
// ============================================================

/// O(1) membership mirror of ONE `Vec<u32>` free-list.
///
/// The `Vec` stays the SOURCE OF TRUTH (it is the bincode field, and `alloc_*`'s LIFO
/// `pop()` reuse — hence the whole kernel's id-assignment determinism — depends on its
/// exact order). This is a pure *index* over it: `bits[i/64] & (1 << i%64)` answers
/// "is slot i free?" in constant time instead of `Vec::contains`'s linear scan.
///
/// Why it exists: `is_edge_live` / `is_face_live` / `is_vert_live` are documented as the
/// predicate EVERY half-edge consumer must call, and every one of them was
/// `!free_*.contains(&i)` — O(free-list). A `for i in 0..half_edges.len()` sweep was
/// therefore O(HE x free_len), and `free_edge`'s own idempotency guard made deleting N
/// faces O(N^2) (measured: 4000 face deletes on a 9216-face grid = 75.8 s).
///
/// `count` is the number of live bits, mirroring `list.len()`. It is what makes the mirror
/// unable to lie: [`FreeBitset::contains`] compares it against the list length in O(1) and
/// falls back to the (correct, slow) linear scan if they ever disagree, so a mesh built by
/// a struct literal / `Default` that never went through [`HalfEdgeMesh::rebuild_free_index`]
/// still answers CORRECTLY — just not quickly — until the next `&mut` entry point heals it.
#[derive(Clone, Default)]
pub(super) struct FreeBitset {
    bits: Vec<u64>,
    count: usize,
}

impl std::fmt::Debug for FreeBitset {
    // Terse on purpose: `HalfEdgeMesh`'s derived Debug must not dump a bitset the size of
    // the mesh (and this field is an allocator detail, not mesh content).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FreeBitset({} free)", self.count)
    }
}

impl FreeBitset {
    /// O(1) staleness probe: the mirror tracks exactly one `Vec`, so equal cardinality is
    /// the invariant (`free_*` never holds a duplicate — the `free_*` chokepoints guarantee it).
    #[inline]
    pub(super) fn in_sync(&self, list: &[u32]) -> bool {
        self.count == list.len()
    }

    #[inline]
    fn get(&self, i: u32) -> bool {
        let w = (i >> 6) as usize;
        w < self.bits.len() && (self.bits[w] >> (i & 63)) & 1 != 0
    }

    #[inline]
    fn set_bit(&mut self, i: u32) {
        let w = (i >> 6) as usize;
        if w >= self.bits.len() {
            self.bits.resize(w + 1, 0);
        }
        self.bits[w] |= 1u64 << (i & 63);
    }

    #[inline]
    fn clear_bit(&mut self, i: u32) {
        let w = (i >> 6) as usize;
        if w < self.bits.len() {
            self.bits[w] &= !(1u64 << (i & 63));
        }
    }

    /// Is slot `i` on `list`? O(1) when the mirror is in sync, and STILL CORRECT (linear)
    /// when it is not — the mirror is never permitted to answer differently from the `Vec`.
    #[inline]
    pub(super) fn contains(&self, list: &[u32], i: u32) -> bool {
        if self.in_sync(list) { self.get(i) } else { list.contains(&i) }
    }

    /// Discard and re-derive from the authoritative `Vec`. O(free_len), and the ONLY way the
    /// mirror is ever populated from scratch (deserialize / literal-built mesh).
    pub(super) fn rebuild(&mut self, list: &[u32]) {
        self.bits.clear();
        for &i in list {
            self.set_bit(i);
        }
        self.count = list.len();
    }

    /// O(1) in the steady state; re-derives only when a mesh arrived without a mirror.
    #[inline]
    pub(super) fn ensure(&mut self, list: &[u32]) {
        if !self.in_sync(list) {
            self.rebuild(list);
        }
    }

    /// Mirror a `list.push(i)`. Caller MUST have called [`FreeBitset::ensure`] first.
    #[inline]
    pub(super) fn insert(&mut self, i: u32) {
        self.set_bit(i);
        self.count += 1;
    }

    /// Mirror a `list.pop()` that yielded `i`. Caller MUST have called [`FreeBitset::ensure`] first.
    #[inline]
    pub(super) fn remove(&mut self, i: u32) {
        self.clear_bit(i);
        self.count -= 1;
    }
}

/// The three mirrors, one per free-list. RUNTIME-ONLY — `#[serde(skip)]` on the field means
/// it emits and consumes ZERO bytes, so `HalfEdgeMesh`'s on-disk shape is byte-identical.
#[derive(Clone, Debug, Default)]
pub(super) struct FreeSlotIndex {
    pub(super) verts: FreeBitset,
    pub(super) edges: FreeBitset,
    pub(super) faces: FreeBitset,
}

// ============================================================
// HalfEdgeMesh
// ============================================================

#[derive(Clone, Debug, Serialize)]
pub struct HalfEdgeMesh {
    pub vertices: Vec<HVertex>,
    pub half_edges: Vec<HalfEdge>,
    pub faces: Vec<HFace>,
    // Free-list indices for O(1) slot reuse after deletion
    pub(super) free_verts: Vec<u32>,
    pub(super) free_edges: Vec<u32>,
    pub(super) free_faces: Vec<u32>,
    /// O(1) membership mirror of the three lists above. `#[serde(skip)]` — emits/consumes
    /// ZERO bytes, so the six serialized fields above remain the WHOLE on-disk image and
    /// every existing `.fl4` still decodes byte-for-byte (gated by `tests/serde_golden.rs`).
    #[serde(skip)]
    pub(super) free_index: FreeSlotIndex,
}

/// Wire image of [`HalfEdgeMesh`] — EXACTLY the six serialized fields, in order, with the
/// same names and types, so it decodes byte-for-byte identically to the old
/// `#[derive(Deserialize)]` (bincode is positional; JSON/self-describing formats key on the
/// same names). It exists so `Deserialize` can populate the `#[serde(skip)]` mirror.
///
/// Without this, a loaded mesh would arrive with populated `free_*` Vecs and an EMPTY
/// mirror — the classic `serde(skip)` hazard. `FreeBitset::contains` already refuses to lie
/// in that state (it falls back to the linear scan), but this makes the state UNREACHABLE
/// rather than merely survivable: after `deserialize` the mirror is always correct AND fast.
#[derive(Deserialize)]
#[serde(rename = "HalfEdgeMesh")]
struct HalfEdgeMeshWire {
    vertices: Vec<HVertex>,
    half_edges: Vec<HalfEdge>,
    faces: Vec<HFace>,
    free_verts: Vec<u32>,
    free_edges: Vec<u32>,
    free_faces: Vec<u32>,
}

impl<'de> Deserialize<'de> for HalfEdgeMesh {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let w = HalfEdgeMeshWire::deserialize(deserializer)?;
        let mut mesh = HalfEdgeMesh {
            vertices: w.vertices,
            half_edges: w.half_edges,
            faces: w.faces,
            free_verts: w.free_verts,
            free_edges: w.free_edges,
            free_faces: w.free_faces,
            free_index: FreeSlotIndex::default(),
        };
        mesh.rebuild_free_index();
        Ok(mesh)
    }
}
