//! Geometry Channel — Modo-style procedural filters for poly modifiers & select-by-channel.
//!
//! A channel describes WHICH faces/edges a modifier (or select command) targets:
//! tris / quads / n-gons / exact side count / material / edge length / edge angle.
//! Default is **All** (no filter) so existing stacks are bit-identical on load.

use glam::Vec3;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, NO_EDGE, NO_FACE, NO_TWIN};

/// Quantized position key — the SAME scheme and tolerance `CustomData::remap_by_position` uses
/// (`PRESERVE_Q` = 1e5), so authored data and the kernels agree by construction rather than by
/// coincidence.
pub type PosKey = (i64, i64, i64);

const POS_Q: f32 = 1e5;

#[inline]
fn qpos(p: Vec3) -> PosKey {
    (
        (p.x * POS_Q).round() as i64,
        (p.y * POS_Q).round() as i64,
        (p.z * POS_Q).round() as i64,
    )
}

/// AUTHORED per-element data, re-keyed by POSITION so it survives the modifier stack.
///
/// Everything the user marks by hand — a polygroup, an edge weight — lives in the body's
/// `CustomData` sidecar keyed by element id. Ids do not survive a stack evaluation (subdivide,
/// bevel, boolean and remesh all renumber, and most rebuild from scratch), so a channel that
/// looked up `groups[FaceId]` on the stack's intermediate mesh would read whatever face happened
/// to land at that index — arbitrary geometry, silently. Positions DO survive for geometry a
/// modifier did not rewrite, which is the same guarantee `remap_by_position` and the Smart Select
/// `op_created` tags already rely on.
///
/// Empty = nothing authored; every authored-data channel then matches nothing (never "everything",
/// which would silently smooth the whole mesh the moment a group was deleted).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AuthoredChannels {
    /// FACE domain — polygroup id by quantized face CENTROID (`0` is never stored).
    pub group_by_face: HashMap<PosKey, u16>,
    /// EDGE domain — authored weight in `0..=1` by UNDIRECTED quantized endpoint pair (the
    /// smaller key first, so a half-edge and its twin resolve to one entry).
    pub weight_by_edge: HashMap<(PosKey, PosKey), f32>,
}

impl AuthoredChannels {
    pub fn is_empty(&self) -> bool {
        self.group_by_face.is_empty() && self.weight_by_edge.is_empty()
    }

    /// Undirected edge key: orientation-independent, so `he` and its twin agree.
    #[inline]
    pub fn edge_key(a: Vec3, b: Vec3) -> (PosKey, PosKey) {
        let (ka, kb) = (qpos(a), qpos(b));
        if ka <= kb { (ka, kb) } else { (kb, ka) }
    }

    /// Centroid key for a face, matching `CustomData::remap_by_position`'s face identity.
    pub fn face_key(mesh: &HalfEdgeMesh, f: FaceId) -> PosKey {
        let vs = mesh.face_vertices(f);
        if vs.is_empty() {
            return qpos(Vec3::ZERO);
        }
        let c: Vec3 = vs.iter().map(|&v| mesh.vert(v).position).sum::<Vec3>() / vs.len() as f32;
        qpos(c)
    }

    /// The polygroup of `f` on `mesh`, or `0` when ungrouped / unknown.
    pub fn group_of_face(&self, mesh: &HalfEdgeMesh, f: FaceId) -> u16 {
        self.group_by_face
            .get(&Self::face_key(mesh, f))
            .copied()
            .unwrap_or(0)
    }

    /// The authored weight of the edge under `he`, or `0.0` when unweighted / unknown.
    ///
    /// For MASKING ("is this edge weighted enough to pass the filter"), where absent and zero mean
    /// the same thing. Callers that must tell "painted 0" apart from "never painted" — the bevel
    /// width percentage, where those two mean *no bevel* and *full bevel* — want
    /// [`Self::authored_weight_of_edge`] instead.
    pub fn weight_of_edge(&self, mesh: &HalfEdgeMesh, he: HalfEdgeId) -> f32 {
        self.authored_weight_of_edge(mesh, he).unwrap_or(0.0)
    }

    /// The authored weight of the edge under `he`, or `None` when the user never painted it.
    ///
    /// Distinguishing absent from `0.0` is load-bearing wherever the weight scales something: an
    /// edge painted to 0 asks for nothing, while an unpainted edge asks to be left alone. Collapsing
    /// the two makes enabling a weight-driven feature wipe out every edge not painted yet.
    pub fn authored_weight_of_edge(&self, mesh: &HalfEdgeMesh, he: HalfEdgeId) -> Option<f32> {
        let a = mesh.vert(mesh.he_source(he)).position;
        let b = mesh.vert(mesh.he(he).vertex).position;
        self.weight_by_edge.get(&Self::edge_key(a, b)).copied()
    }
}

/// What geometric property the channel matches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum GeoChannelKind {
    /// No filter — entire mesh (legacy default).
    #[default]
    All = 0,
    /// Faces with exactly 3 vertices.
    Tris = 1,
    /// Faces with exactly 4 vertices.
    Quads = 2,
    /// Faces with 5+ vertices (n-gons).
    Ngons = 3,
    /// Faces with exactly `n_sides` vertices.
    NSides = 4,
    /// Faces whose `material_id` matches `material_id` (0 = body default).
    Material = 5,
    /// Edges whose length is in `[edge_len_min, edge_len_max]`.
    EdgeLength = 6,
    /// Edges whose face-face dihedral angle (degrees) is in `[angle_min_deg, angle_max_deg]`.
    EdgeAngle = 7,
    // ── append-only (serde tags by variant INDEX — never insert above this line) ──
    /// AUTHORED. Faces the user assigned to polygroup `group_id` ("mark specific polygons").
    /// Needs [`AuthoredChannels`]; matches nothing without it.
    Polygroup = 8,
    /// AUTHORED. Edges the user gave a weight in `[weight_min, weight_max]` ("weighted edges").
    ///
    /// Unlike every other kind this one is **CONTINUOUS** where a modifier supports it: the
    /// authored 0..1 value becomes the mask strength directly, so a half-weighted edge is
    /// half-affected. Needs [`AuthoredChannels`]; matches nothing without it.
    EdgeWeight = 9,
}

impl GeoChannelKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All geometry",
            Self::Tris => "Triangles (3)",
            Self::Quads => "Quads (4)",
            Self::Ngons => "N-gons (5+)",
            Self::NSides => "Exact side count",
            Self::Material => "Material slot",
            Self::EdgeLength => "Edge length",
            Self::EdgeAngle => "Edge angle (dihedral)",
            Self::Polygroup => "Polygroup (marked faces)",
            Self::EdgeWeight => "Edge weight (authored)",
        }
    }

    pub fn all() -> &'static [GeoChannelKind] {
        &[
            Self::All,
            Self::Tris,
            Self::Quads,
            Self::Ngons,
            Self::NSides,
            Self::Material,
            Self::EdgeLength,
            Self::EdgeAngle,
            Self::Polygroup,
            Self::EdgeWeight,
        ]
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Tris,
            2 => Self::Quads,
            3 => Self::Ngons,
            4 => Self::NSides,
            5 => Self::Material,
            6 => Self::EdgeLength,
            7 => Self::EdgeAngle,
            8 => Self::Polygroup,
            9 => Self::EdgeWeight,
            _ => Self::All,
        }
    }

    /// True when the channel primarily filters edges (not faces).
    pub fn is_edge_filter(self) -> bool {
        matches!(self, Self::EdgeLength | Self::EdgeAngle | Self::EdgeWeight)
    }

    /// True when the channel reads AUTHORED data ([`AuthoredChannels`]) rather than deriving
    /// everything from the geometry in front of it. Callers use this to decide whether the
    /// authored tables must be threaded in — and to fold them into display-cache keys.
    pub fn needs_authored(self) -> bool {
        matches!(self, Self::Polygroup | Self::EdgeWeight)
    }

    /// True when the authored value is a CONTINUOUS strength rather than a yes/no match.
    pub fn is_continuous(self) -> bool {
        matches!(self, Self::EdgeWeight)
    }
}

/// Procedural geometry filter attached to a poly modifier or used for Select-by-Channel.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GeometryChannel {
    pub kind: GeoChannelKind,
    /// For [`GeoChannelKind::NSides`]: required vertex count (3..256).
    #[serde(default = "default_n_sides")]
    pub n_sides: u32,
    /// For [`GeoChannelKind::Material`]: face `material_id` to match.
    #[serde(default)]
    pub material_id: u32,
    /// Edge length range (local mesh units). Used by EdgeLength.
    #[serde(default)]
    pub edge_len_min: f32,
    #[serde(default = "default_edge_len_max")]
    pub edge_len_max: f32,
    /// Dihedral angle range in degrees (0 = coplanar, 90 = square). Used by EdgeAngle.
    #[serde(default)]
    pub angle_min_deg: f32,
    #[serde(default = "default_angle_max")]
    pub angle_max_deg: f32,
    /// Invert the match (select/modify everything the channel does NOT match).
    #[serde(default)]
    pub invert: bool,
    /// For [`GeoChannelKind::Polygroup`]: which authored group to match (`0` = ungrouped).
    ///
    /// **`#[serde(skip)]` — NOT `#[serde(default)]`.** `GeometryChannel` is serialized INSIDE the
    /// bincode blob (as `PolyModifier.geo_channel`), and bincode is positional: a new field here
    /// shifts every byte after it in `PolyModifier`, corrupting any `.fl4` that already stored a
    /// channel. These three ride the `FL4PSMTH` tail section alongside `smooth_falloff`.
    #[serde(skip)]
    pub group_id: u16,
    /// For [`GeoChannelKind::EdgeWeight`]: authored-weight range to match. See `group_id` re skip.
    #[serde(skip, default = "default_weight_min")]
    pub weight_min: f32,
    #[serde(skip, default = "default_weight_max")]
    pub weight_max: f32,
}

fn default_n_sides() -> u32 {
    4
}
fn default_edge_len_max() -> f32 {
    1.0e6
}
fn default_angle_max() -> f32 {
    180.0
}
/// Any weight above zero counts by default — an authored edge is authored.
fn default_weight_min() -> f32 {
    0.001
}
fn default_weight_max() -> f32 {
    1.0
}

impl Default for GeometryChannel {
    fn default() -> Self {
        Self {
            kind: GeoChannelKind::All,
            n_sides: 4,
            material_id: 0,
            edge_len_min: 0.0,
            edge_len_max: 1.0e6,
            angle_min_deg: 0.0,
            angle_max_deg: 180.0,
            invert: false,
            group_id: 0,
            weight_min: 0.001,
            weight_max: 1.0,
        }
    }
}

impl GeometryChannel {
    pub fn all() -> Self {
        Self::default()
    }

    pub fn is_all(&self) -> bool {
        self.kind == GeoChannelKind::All && !self.invert
    }

    /// Hash payload for display-mesh cache keys.
    pub fn hash_into(&self, h: &mut impl std::hash::Hasher) {
        use std::hash::Hasher;
        h.write_u8(self.kind as u8);
        h.write_u32(self.n_sides);
        h.write_u32(self.material_id);
        h.write_u32(self.edge_len_min.to_bits());
        h.write_u32(self.edge_len_max.to_bits());
        h.write_u32(self.angle_min_deg.to_bits());
        h.write_u32(self.angle_max_deg.to_bits());
        h.write_u8(self.invert as u8);
        h.write_u16(self.group_id);
        h.write_u32(self.weight_min.to_bits());
        h.write_u32(self.weight_max.to_bits());
    }

    fn face_matches_raw(&self, mesh: &HalfEdgeMesh, f: FaceId) -> bool {
        self.face_matches_auth(mesh, f, None)
    }

    fn face_matches_auth(
        &self,
        mesh: &HalfEdgeMesh,
        f: FaceId,
        auth: Option<&AuthoredChannels>,
    ) -> bool {
        if (f.0 as usize) >= mesh.faces.len() || mesh.face(f).half_edge == NO_EDGE {
            return false;
        }
        let n = mesh.face_vertices(f).len() as u32;
        match self.kind {
            GeoChannelKind::All => true,
            GeoChannelKind::Tris => n == 3,
            GeoChannelKind::Quads => n == 4,
            GeoChannelKind::Ngons => n >= 5,
            GeoChannelKind::NSides => n == self.n_sides.clamp(3, 256),
            GeoChannelKind::Material => mesh.face(f).material_id == self.material_id,
            // AUTHORED: no tables threaded in → matches NOTHING. Never "everything": a stack that
            // silently smoothed the whole body the moment a group was deleted would be worse than
            // one that visibly does nothing.
            GeoChannelKind::Polygroup => match auth {
                Some(a) => a.group_of_face(mesh, f) == self.group_id,
                None => false,
            },
            // Edge filters: face matches if ANY boundary edge matches.
            GeoChannelKind::EdgeLength | GeoChannelKind::EdgeAngle | GeoChannelKind::EdgeWeight => {
                for he in mesh.face_half_edges(f) {
                    if self.edge_matches_auth(mesh, he, auth) {
                        return true;
                    }
                }
                false
            }
        }
    }

    fn edge_matches_raw(&self, mesh: &HalfEdgeMesh, he: HalfEdgeId) -> bool {
        self.edge_matches_auth(mesh, he, None)
    }

    fn edge_matches_auth(
        &self,
        mesh: &HalfEdgeMesh,
        he: HalfEdgeId,
        auth: Option<&AuthoredChannels>,
    ) -> bool {
        if (he.0 as usize) >= mesh.half_edges.len() || !mesh.is_edge_live(he) {
            return false;
        }
        match self.kind {
            GeoChannelKind::All => true,
            GeoChannelKind::EdgeLength => {
                let a = mesh.vert(mesh.he_source(he)).position;
                let b = mesh.vert(mesh.he(he).vertex).position;
                let len = (b - a).length();
                len >= self.edge_len_min && len <= self.edge_len_max
            }
            GeoChannelKind::EdgeAngle => {
                let deg = match mesh.edge_dihedral_cos(he) {
                    Some(c) => c.clamp(-1.0, 1.0).acos().to_degrees(),
                    None => return false, // boundary — no dihedral
                };
                deg >= self.angle_min_deg && deg <= self.angle_max_deg
            }
            GeoChannelKind::EdgeWeight => match auth {
                Some(a) => {
                    let w = a.weight_of_edge(mesh, he);
                    w >= self.weight_min && w <= self.weight_max
                }
                None => false,
            },
            // Face channels: edge matches if either incident face matches.
            GeoChannelKind::Tris
            | GeoChannelKind::Quads
            | GeoChannelKind::Ngons
            | GeoChannelKind::NSides
            | GeoChannelKind::Material
            | GeoChannelKind::Polygroup => {
                let f0 = mesh.he(he).face;
                let twin = mesh.he(he).twin;
                let f1 = if twin != NO_TWIN {
                    mesh.he(twin).face
                } else {
                    NO_FACE
                };
                let mut ok = false;
                if f0 != NO_FACE {
                    ok |= self.face_matches_auth(mesh, f0, auth);
                }
                if f1 != NO_FACE {
                    ok |= self.face_matches_auth(mesh, f1, auth);
                }
                ok
            }
        }
    }

    /// Live faces that match this channel (after invert). Authored kinds match nothing.
    pub fn matching_faces(&self, mesh: &HalfEdgeMesh) -> HashSet<u32> {
        self.matching_faces_auth(mesh, None)
    }

    /// [`Self::matching_faces`] with authored data available.
    pub fn matching_faces_auth(
        &self,
        mesh: &HalfEdgeMesh,
        auth: Option<&AuthoredChannels>,
    ) -> HashSet<u32> {
        let mut out = HashSet::new();
        for i in 0..mesh.faces.len() as u32 {
            let f = FaceId(i);
            if mesh.face(f).half_edge == NO_EDGE {
                continue;
            }
            let m = self.face_matches_auth(mesh, f, auth);
            if m ^ self.invert {
                out.insert(i);
            }
        }
        out
    }

    /// Live half-edges that match this channel (after invert). Twin pairs both included.
    pub fn matching_edges(&self, mesh: &HalfEdgeMesh) -> HashSet<u32> {
        self.matching_edges_auth(mesh, None)
    }

    /// [`Self::matching_edges`] with authored data available.
    pub fn matching_edges_auth(
        &self,
        mesh: &HalfEdgeMesh,
        auth: Option<&AuthoredChannels>,
    ) -> HashSet<u32> {
        let mut out = HashSet::new();
        for i in 0..mesh.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !mesh.is_edge_live(he) {
                continue;
            }
            let m = self.edge_matches_auth(mesh, he, auth);
            if m ^ self.invert {
                out.insert(i);
            }
        }
        out
    }

    /// Restrict `edges` to those that also match the channel. If channel is All, returns input.
    pub fn filter_edge_ids(&self, mesh: &HalfEdgeMesh, edges: &[u32]) -> Vec<u32> {
        if self.is_all() {
            return edges.to_vec();
        }
        let allowed = self.matching_edges(mesh);
        edges
            .iter()
            .copied()
            .filter(|e| allowed.contains(e))
            .collect()
    }

    /// Per-vertex mask in `0..=1`: `1` where this channel matches, `0` where it does not, with a
    /// linear ramp across `feather_rings` of topological 1-ring dilation in between.
    ///
    /// This is how a FACE/EDGE channel becomes something a per-vertex deformer (Smooth) can use.
    /// The feather is not cosmetic: a binary mask leaves a visible ridge exactly on the channel
    /// boundary, because one row of vertices is fully relaxed and its neighbour is fully pinned.
    /// Ramping over N rings spreads that step out so the modifier's effect fades into the
    /// untouched geometry.
    ///
    /// Returns `None` when the channel is `All` (no filter) — callers should then take their
    /// unmasked path rather than allocate and multiply by an all-ones buffer.
    ///
    /// Ring 0 = the matched vertices themselves (weight 1). Ring `k` (1..=feather_rings) gets
    /// `1 - k / (feather_rings + 1)`, so the ramp reaches the last dilated ring still positive and
    /// hits 0 only outside the feathered band. `feather_rings = 0` gives a hard binary mask.
    pub fn vertex_weights(&self, mesh: &HalfEdgeMesh, feather_rings: u32) -> Option<Vec<f32>> {
        self.vertex_weights_auth(mesh, feather_rings, None)
    }

    /// [`Self::vertex_weights`] with authored data available.
    ///
    /// For a CONTINUOUS kind ([`GeoChannelKind::is_continuous`], i.e. `EdgeWeight`) ring 0 is
    /// seeded with the AUTHORED VALUE rather than a flat `1.0`: a 0.4-weighted edge yields 0.4
    /// strength. That is the whole point of a weight map — the user painted the ramp, so the
    /// feather must not overwrite it. Feather rings still extend BELOW the authored value, never
    /// above it, so dilation can only ever soften.
    pub fn vertex_weights_auth(
        &self,
        mesh: &HalfEdgeMesh,
        feather_rings: u32,
        auth: Option<&AuthoredChannels>,
    ) -> Option<Vec<f32>> {
        if self.is_all() {
            return None;
        }
        let nv = mesh.vertices.len();
        let mut w = vec![0.0f32; nv];
        // Seed ring 0 from whichever domain this channel speaks natively, so an EdgeAngle channel
        // masks the vertices ON those edges rather than every vertex of any touching face.
        let mut frontier: Vec<u32> = Vec::new();
        let continuous = self.kind.is_continuous();
        if self.kind.is_edge_filter() {
            for e in self.matching_edges_auth(mesh, auth) {
                let he = HalfEdgeId(e);
                // A vertex touched by several weighted edges takes the STRONGEST of them —
                // averaging would let an unweighted neighbour edge dilute a weight the user
                // explicitly painted.
                let s = if continuous {
                    match auth {
                        Some(a) => a.weight_of_edge(mesh, he).clamp(0.0, 1.0),
                        None => 0.0,
                    }
                } else {
                    1.0
                };
                if s <= 0.0 {
                    continue;
                }
                for v in [mesh.he_source(he), mesh.he(he).vertex] {
                    if w[v.0 as usize] < s {
                        if w[v.0 as usize] == 0.0 {
                            frontier.push(v.0);
                        }
                        w[v.0 as usize] = s;
                    }
                }
            }
        } else {
            let mut ring = Vec::new();
            for f in self.matching_faces_auth(mesh, auth) {
                mesh.face_vertices_into(FaceId(f), &mut ring);
                for v in ring.iter() {
                    if w[v.0 as usize] == 0.0 {
                        w[v.0 as usize] = 1.0;
                        frontier.push(v.0);
                    }
                }
            }
        }
        if frontier.is_empty() || feather_rings == 0 {
            return Some(w);
        }

        // BFS dilation over face rings — the SAME adjacency the Laplacian itself uses (consecutive
        // vertices of every live face), so the mask can never feather along a connection the
        // smoothing does not actually average over.
        let mut adj_ring = Vec::new();
        let mut nbrs: Vec<Vec<u32>> = vec![Vec::new(); nv];
        for i in 0..mesh.faces.len() as u32 {
            let fid = FaceId(i);
            if mesh.face(fid).half_edge == NO_EDGE {
                continue;
            }
            mesh.face_vertices_into(fid, &mut adj_ring);
            let n = adj_ring.len();
            for k in 0..n {
                let (a, b) = (adj_ring[k].0, adj_ring[(k + 1) % n].0);
                nbrs[a as usize].push(b);
                nbrs[b as usize].push(a);
            }
        }
        // `seed[v]` = the ring-0 strength this vertex's ramp descends from. For a binary channel
        // that is always 1.0, so the ramp is the plain linear `1 - k/steps`. For a CONTINUOUS
        // channel it is the authored value, so a 0.4-weighted edge feathers 0.4 → 0, never
        // 1.0 → 0: dilation may only soften an authored weight, never amplify it.
        let mut seed: Vec<f32> = w.clone();
        let steps = feather_rings as f32 + 1.0;
        for k in 1..=feather_rings {
            let ramp = 1.0 - (k as f32) / steps;
            let mut next = Vec::new();
            for &v in &frontier {
                let s = seed[v as usize];
                let val = s * ramp;
                for &nb in &nbrs[v as usize] {
                    // Only unclaimed vertices, and only if this source actually brightens them —
                    // a strong seed two rings away must win over a weak seed one ring away.
                    if w[nb as usize] < val && seed[nb as usize] == 0.0 {
                        if w[nb as usize] == 0.0 {
                            next.push(nb);
                        }
                        w[nb as usize] = val;
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            // Ring k's vertices inherit their source's seed so the next ring keeps descending from
            // the same authored value rather than restarting at their own (already-decayed) weight.
            for &v in &next {
                seed[v as usize] = w[v as usize] / ramp.max(1e-6);
            }
            frontier = next;
        }
        Some(w)
    }

    /// Restrict face ids to those matching the channel.
    pub fn filter_face_ids(&self, mesh: &HalfEdgeMesh, faces: &[u32]) -> Vec<u32> {
        if self.is_all() {
            return faces.to_vec();
        }
        let allowed = self.matching_faces(mesh);
        faces
            .iter()
            .copied()
            .filter(|f| allowed.contains(f))
            .collect()
    }
}

/// Short summary for UI badges.
pub fn channel_summary(ch: &GeometryChannel) -> String {
    if ch.is_all() {
        return "All".into();
    }
    let base = match ch.kind {
        GeoChannelKind::Tris => "Tris".into(),
        GeoChannelKind::Quads => "Quads".into(),
        GeoChannelKind::Ngons => "N-gons".into(),
        GeoChannelKind::NSides => format!("{}-sides", ch.n_sides),
        GeoChannelKind::Material => format!("Mat {}", ch.material_id),
        GeoChannelKind::EdgeLength => format!("Len {:.3}–{:.3}", ch.edge_len_min, ch.edge_len_max),
        GeoChannelKind::EdgeAngle => format!("∠ {:.0}–{:.0}°", ch.angle_min_deg, ch.angle_max_deg),
        GeoChannelKind::EdgeWeight => format!(
            "W {:.2}–{:.2}",
            ch.weight_min, ch.weight_max
        ),
        GeoChannelKind::Polygroup => format!("PG {}", ch.group_id),
        GeoChannelKind::All => "All".into(),
    };
    if ch.invert {
        format!("NOT {base}")
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    fn box_mesh() -> HalfEdgeMesh {
        HalfEdgeMesh::new_box(Vec3::ONE)
    }

    #[test]
    fn quads_match_all_box_faces() {
        let m = box_mesh();
        let ch = GeometryChannel {
            kind: GeoChannelKind::Quads,
            ..Default::default()
        };
        let faces = ch.matching_faces(&m);
        assert_eq!(faces.len(), 6, "unit box has 6 quads");
        let tris = GeometryChannel {
            kind: GeoChannelKind::Tris,
            ..Default::default()
        };
        assert!(tris.matching_faces(&m).is_empty());
    }

    #[test]
    fn invert_selects_complement() {
        let m = box_mesh();
        let ch = GeometryChannel {
            kind: GeoChannelKind::Quads,
            invert: true,
            ..Default::default()
        };
        assert!(ch.matching_faces(&m).is_empty());
    }

    #[test]
    fn n_sides_exact() {
        let m = box_mesh();
        let ch = GeometryChannel {
            kind: GeoChannelKind::NSides,
            n_sides: 4,
            ..Default::default()
        };
        assert_eq!(ch.matching_faces(&m).len(), 6);
        let ch3 = GeometryChannel {
            kind: GeoChannelKind::NSides,
            n_sides: 3,
            ..Default::default()
        };
        assert!(ch3.matching_faces(&m).is_empty());
    }

    #[test]
    fn edge_angle_square_corners() {
        let m = box_mesh();
        // Cube edges are ~90° dihedral.
        let ch = GeometryChannel {
            kind: GeoChannelKind::EdgeAngle,
            angle_min_deg: 80.0,
            angle_max_deg: 100.0,
            ..Default::default()
        };
        let edges = ch.matching_edges(&m);
        assert!(!edges.is_empty(), "cube should have ~90° edges");
    }

    #[test]
    fn vertex_weights_none_for_unfiltered_channel() {
        // `All` must return None, not an all-ones buffer: callers use None to take the
        // untouched fast path, which is what keeps an unmasked Smooth bit-identical.
        assert!(GeometryChannel::all().vertex_weights(&box_mesh(), 2).is_none());
    }

    #[test]
    fn vertex_weights_are_binary_without_feather() {
        let m = box_mesh();
        let ch = GeometryChannel {
            kind: GeoChannelKind::Quads,
            ..Default::default()
        };
        let w = ch.vertex_weights(&m, 0).expect("filtered channel");
        assert_eq!(w.len(), m.vertices.len());
        // Every face of a box is a quad, so every vertex is in ring 0.
        assert!(w.iter().all(|&x| x == 1.0), "got {w:?}");
    }

    #[test]
    fn vertex_weights_ramp_down_across_feather_rings() {
        // A grid is wide enough that a single-face seed does not immediately reach every vertex,
        // so the ramp is observable. Seed = one material slot, feathered 2 rings.
        let mut m = HalfEdgeMesh::new_plane(6.0, 6.0, 6, 6);
        let f0 = FaceId(0);
        m.faces[f0.0 as usize].material_id = 7;
        let ch = GeometryChannel {
            kind: GeoChannelKind::Material,
            material_id: 7,
            ..Default::default()
        };
        let w = ch.vertex_weights(&m, 2).expect("filtered channel");

        let ones = w.iter().filter(|&&x| x == 1.0).count();
        assert_eq!(ones, 4, "one quad seeds exactly its 4 corners at full weight");
        // feather_rings = 2 → ring1 = 1 - 1/3, ring2 = 1 - 2/3. Nothing else may be positive.
        let r1 = w.iter().filter(|&&x| (x - 2.0 / 3.0).abs() < 1e-6).count();
        let r2 = w.iter().filter(|&&x| (x - 1.0 / 3.0).abs() < 1e-6).count();
        assert!(r1 > 0, "ring 1 must be populated");
        assert!(r2 > 0, "ring 2 must be populated");
        assert!(
            w.iter().any(|&x| x == 0.0),
            "a 6x6 grid must still have vertices outside a 2-ring feather"
        );
        for &x in &w {
            assert!(
                x == 0.0 || x == 1.0 || (x - 2.0 / 3.0).abs() < 1e-6 || (x - 1.0 / 3.0).abs() < 1e-6,
                "unexpected ramp value {x}"
            );
        }
    }

    #[test]
    fn vertex_weights_invert_flips_the_masked_region() {
        let mut m = HalfEdgeMesh::new_plane(6.0, 6.0, 6, 6);
        m.faces[0].material_id = 7;
        let base = GeometryChannel {
            kind: GeoChannelKind::Material,
            material_id: 7,
            ..Default::default()
        };
        let inv = GeometryChannel {
            invert: true,
            ..base.clone()
        };
        let a = base.vertex_weights(&m, 0).expect("filtered");
        let b = inv.vertex_weights(&m, 0).expect("filtered");
        // Hard masks of a channel and its inverse cover every vertex between them, and the only
        // overlap is the seam ring (vertices shared by a matched and an unmatched face).
        for i in 0..a.len() {
            assert!(a[i] == 1.0 || b[i] == 1.0, "vertex {i} masked by neither");
        }
        assert!(b.iter().filter(|&&x| x == 1.0).count() > a.iter().filter(|&&x| x == 1.0).count());
    }

    /// Build authored tables for a plane: face 0 in group 3, one edge weighted 0.5.
    fn authored_plane() -> (HalfEdgeMesh, AuthoredChannels, HalfEdgeId) {
        let m = HalfEdgeMesh::new_plane(6.0, 6.0, 6, 6);
        let mut a = AuthoredChannels::default();
        a.group_by_face
            .insert(AuthoredChannels::face_key(&m, FaceId(0)), 3);
        // First live half-edge of face 0.
        let he = m.face_half_edges(FaceId(0))[0];
        let pa = m.vert(m.he_source(he)).position;
        let pb = m.vert(m.he(he).vertex).position;
        a.weight_by_edge
            .insert(AuthoredChannels::edge_key(pa, pb), 0.5);
        (m, a, he)
    }

    #[test]
    fn authored_channels_match_nothing_without_the_tables() {
        // The fail-closed rule: no sidecar threaded in must mean "nothing", never "everything".
        // The opposite default would silently smooth a whole body the moment a group was deleted.
        let (m, _a, _he) = authored_plane();
        for kind in [GeoChannelKind::Polygroup, GeoChannelKind::EdgeWeight] {
            let ch = GeometryChannel {
                kind,
                group_id: 3,
                ..Default::default()
            };
            assert!(
                ch.matching_faces(&m).is_empty(),
                "{kind:?}: no authored data must match no faces"
            );
            assert!(
                ch.matching_edges(&m).is_empty(),
                "{kind:?}: no authored data must match no edges"
            );
        }
    }

    #[test]
    fn polygroup_channel_matches_only_the_marked_faces() {
        let (m, a, _he) = authored_plane();
        let ch = GeometryChannel {
            kind: GeoChannelKind::Polygroup,
            group_id: 3,
            ..Default::default()
        };
        let faces = ch.matching_faces_auth(&m, Some(&a));
        assert_eq!(faces.len(), 1, "exactly the one marked face");
        assert!(faces.contains(&0));
        // A different group id matches nothing — ids are not interchangeable.
        let other = GeometryChannel {
            group_id: 4,
            ..ch.clone()
        };
        assert!(other.matching_faces_auth(&m, Some(&a)).is_empty());
    }

    #[test]
    fn edge_weight_channel_is_continuous_and_survives_orientation() {
        let (m, a, he) = authored_plane();
        let ch = GeometryChannel {
            kind: GeoChannelKind::EdgeWeight,
            ..Default::default()
        };
        // Twin symmetry: `edge_key` is undirected, so both half-edges resolve to one weight.
        assert_eq!(a.weight_of_edge(&m, he), 0.5);
        let tw = m.he(he).twin;
        if tw != core_math_no_twin() {
            assert_eq!(
                a.weight_of_edge(&m, tw),
                0.5,
                "an edge and its twin must read the SAME authored weight"
            );
        }
        // CONTINUOUS: the mask carries the authored 0.5, not a flat 1.0.
        let w = ch
            .vertex_weights_auth(&m, 0, Some(&a))
            .expect("filtered channel");
        let strong: Vec<f32> = w.iter().copied().filter(|&x| x > 0.0).collect();
        assert_eq!(strong.len(), 2, "one weighted edge → its two endpoints");
        for x in strong {
            assert!(
                (x - 0.5).abs() < 1e-6,
                "authored weight 0.5 must reach the mask as 0.5, got {x}"
            );
        }
    }

    #[test]
    fn edge_weight_feather_descends_from_the_authored_value() {
        // Dilation may only SOFTEN an authored weight — a 0.5 edge must never feather out of a
        // 1.0 seed, or the ramp would be stronger than the value the user painted.
        let (m, a, _he) = authored_plane();
        let ch = GeometryChannel {
            kind: GeoChannelKind::EdgeWeight,
            ..Default::default()
        };
        let w = ch
            .vertex_weights_auth(&m, 2, Some(&a))
            .expect("filtered channel");
        let max = w.iter().copied().fold(0.0f32, f32::max);
        assert!(
            (max - 0.5).abs() < 1e-6,
            "feathered mask peaked at {max}, above the authored 0.5"
        );
        assert!(
            w.iter().any(|&x| x > 0.0 && x < 0.5 - 1e-6),
            "feather must produce values BELOW the authored weight"
        );
    }

    #[test]
    fn weight_range_excludes_out_of_band_edges() {
        let (m, a, _he) = authored_plane();
        let ch = GeometryChannel {
            kind: GeoChannelKind::EdgeWeight,
            weight_min: 0.8,
            weight_max: 1.0,
            ..Default::default()
        };
        assert!(
            ch.matching_edges_auth(&m, Some(&a)).is_empty(),
            "a 0.5 edge is outside [0.8, 1.0]"
        );
    }

    /// `NO_TWIN` without importing it into this scope's namespace collisions.
    fn core_math_no_twin() -> HalfEdgeId {
        crate::half_edge::NO_TWIN
    }

    #[test]
    fn all_is_identity_filter() {
        let m = box_mesh();
        let ch = GeometryChannel::all();
        let faces: Vec<u32> = (0..m.faces.len() as u32).collect();
        assert_eq!(ch.filter_face_ids(&m, &faces), faces);
    }
}
