/// Vertex → sorted, deduped neighbour vertices, built from the triangle list.
///
/// Searched before writing: this body existed VERBATIM in `mesh_smooth_laplacian` and
/// `mesh_flat_relax_sdf_snap`. (`mesh_analytical_feature_project`'s `cadj` looks similar but is
/// built from CREASE EDGES, not triangles — a different table, deliberately left alone.)
///
/// **This is the dominant cost of the ops that use it**, not the work they do afterwards.
/// Measured on a 40k-tri sphere (`bench_display_mesh_ops`): `SmoothGeometry` is 3.19 ms for ONE
/// iteration and 4.47 ms for TWENTY — i.e. ~3.1 ms of table build plus ~0.07 ms per iteration.
/// So reusing the table across a fold matters far more than speeding up any iteration loop.
pub fn build_vertex_adjacency(n: usize, indices: &[u32]) -> Vec<Vec<u32>> {
    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n];
    for t in indices.chunks_exact(3) {
        let (a, b, c) = (t[0], t[1], t[2]);
        adj[a as usize].push(b);
        adj[a as usize].push(c);
        adj[b as usize].push(a);
        adj[b as usize].push(c);
        adj[c as usize].push(a);
        adj[c as usize].push(b);
    }
    for list in &mut adj {
        list.sort_unstable();
        list.dedup();
    }
    adj
}

// MEASURED DEAD END (2026-07-29): a fold-scoped `AdjacencyCache` that reused this table across
// ops was built and REVERTED — it measured SLOWER (depth-8 stack, 40k tris: 108.7 ms -> 124.6 ms).
//
// Two reasons, both worth knowing before trying again:
//   * A realistic stack interleaves topology-CHANGING ops (Mirror, Decimate) with vertex-moving
//     ones, so the table is invalidated almost every op and the cache never amortises.
//   * Instrumenting hits/misses on a 5-stack benchmark gave 2 hits to 16 rebuilds.
//
// A cache only pays off for RUNS of consecutive smooths. If that shape shows up in real authored
// stacks, revisit — but measure the real stacks first, do not assume.
