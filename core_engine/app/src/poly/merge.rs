use core_math::mesh_ops::PolyEdit;
use core_math::half_edge::VertexId;
use core_math::scene_graph::NodePayload;
use crate::AzraApp;

impl AzraApp {
    /// Merge selected vertices to ONE survivor at their CENTROID (Blender "Merge at Center") — a single
    /// deterministic op, not a chain of midpoint merges that lands the survivor at an order-dependent
    /// running average (audit #13).
    pub(crate) fn poly_merge_vertices(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let mut verts: Vec<u32> = self.poly_sel.selected_vertices.iter().copied().collect();
        if verts.len() < 2 { return; }
        // Sort so the survivor (verts[0], the merge_vertices_to survivor) is the lowest id — deterministic
        // regardless of the unordered HashSet iteration (verification: nondeterministic survivor id).
        verts.sort_unstable();
        let survivor = verts[0];
        // centroid of the selection (read before any mutation)
        let target = {
            let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
            let mesh = match &node.payload {
                NodePayload::PolygonalBody { mesh, .. } => mesh,
                _ => return,
            };
            let sum: glam::Vec3 = verts.iter()
                .map(|&v| mesh.vert(VertexId(v)).position)
                .fold(glam::Vec3::ZERO, |a, b| a + b);
            sum / verts.len() as f32
        };
        let ops = vec![PolyEdit::MergeVerticesAt { verts, target: [target.x, target.y, target.z] }];
        // keep=&[survivor] — on success only the survivor stays selected (as before). On refusal the
        // op is all-or-nothing, so refused_targets() returns the FULL vert set; `survivor` is a
        // member of it, so the union is exactly the original selection. Nothing merged, nothing lost.
        let out = self.poly_op_batch(nid, ops, "Merge Vertices");
        self.poly_report_batch(&out, "merged", &[survivor]);
    }

    /// Merge selected vertices to the ACTIVE (last-selected) vertex — Blender "Merge at Last". The active
    /// vertex is the survivor and stays put; all others collapse onto it. Falls back to centroid merge if
    /// there is no active vertex in the selection (e.g. after a box-select).
    pub(crate) fn poly_merge_vertices_at_last(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let active = match self.poly_active_vertex { Some(v) => v, None => { self.poly_merge_vertices(); return; } };
        let mut verts: Vec<u32> = self.poly_sel.selected_vertices.iter().copied().collect();
        if verts.len() < 2 || !verts.contains(&active) { self.poly_merge_vertices(); return; }
        // Target = the active vertex's position; put it FIRST so merge_vertices_to keeps it as the survivor.
        let target = {
            let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
            let mesh = match &node.payload { NodePayload::PolygonalBody { mesh, .. } => mesh, _ => return };
            mesh.vert(VertexId(active)).position
        };
        verts.retain(|&v| v != active);
        verts.insert(0, active);
        let ops = vec![PolyEdit::MergeVerticesAt { verts, target: [target.x, target.y, target.z] }];
        let out = self.poly_op_batch(nid, ops, "Merge at Last");
        // Active vertex survives only if the merge actually ran; on refusal it is unchanged anyway.
        if out.any_applied() { self.poly_active_vertex = Some(active); }
        self.poly_report_batch(&out, "merged at last", &[active]);
    }

    /// Merge By Distance (Blender): weld only the selected vertices closer than a threshold to each other,
    /// each cluster collapsing to its own centroid (coincident/near verts join; far ones are untouched).
    pub(crate) fn poly_merge_by_distance(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let verts: Vec<u32> = self.poly_sel.selected_vertices.iter().copied().collect();
        if verts.len() < 2 { return; }
        // Read positions + a scale-relative threshold from the mesh bbox.
        let (pos, thr) = {
            let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
            let mesh = match &node.payload { NodePayload::PolygonalBody { mesh, .. } => mesh, _ => return };
            let pos: Vec<glam::Vec3> = verts.iter().map(|&v| mesh.vert(VertexId(v)).position).collect();
            let (mn, mx) = mesh.bounding_box();
            (pos, ((mx - mn).length() * 0.02).max(1e-4))
        };
        // Union-find clusters within the threshold.
        let n = verts.len();
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(p: &mut [usize], mut x: usize) -> usize { while p[x] != x { p[x] = p[p[x]]; x = p[x]; } x }
        for i in 0..n {
            for j in (i + 1)..n {
                if pos[i].distance(pos[j]) < thr {
                    let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                    if a != b { parent[a] = b; }
                }
            }
        }
        let mut groups: std::collections::BTreeMap<usize, Vec<usize>> = std::collections::BTreeMap::new();
        for i in 0..n { let r = find(&mut parent, i); groups.entry(r).or_default().push(i); }
        let mut ops: Vec<PolyEdit> = Vec::new();
        let mut survivors: Vec<u32> = Vec::new();
        for g in groups.values() {
            if g.len() < 2 { survivors.push(verts[g[0]]); continue; }
            let mut gv: Vec<u32> = g.iter().map(|&i| verts[i]).collect();
            gv.sort_unstable(); // survivor = lowest id (merge_vertices_to keeps verts[0])
            let target = g.iter().map(|&i| pos[i]).fold(glam::Vec3::ZERO, |a, b| a + b) / g.len() as f32;
            survivors.push(gv[0]);
            ops.push(PolyEdit::MergeVerticesAt { verts: gv, target: [target.x, target.y, target.z] });
        }
        if ops.is_empty() { self.toast("Merge by Distance: nothing within range"); return; }
        // keep=&survivors — the survivor of an APPLIED cluster stays selected. A REFUSED cluster
        // returns whole via refused_targets(), and its own survivor gv[0] is a member of its verts,
        // so the union is exactly {survivors of applied clusters} ∪ {full refused clusters}. No
        // per-cluster index bookkeeping, and no refused cluster silently collapsing to one vert in
        // the selection as if it had merged.
        let out = self.poly_op_batch(nid, ops, "Merge by Distance");
        if out.any_applied() { self.poly_active_vertex = None; }
        self.poly_report_batch(&out, "merged by distance", &survivors);
    }
}
