//! Slice plumbing shared by the parallel mesh passes.

/// Split `data` into consecutive exclusive sub-slices at the given ELEMENT boundaries.
///
/// `elem_offsets` has one entry per block boundary (so `nblocks + 1` entries), each the index into
/// `data` where that boundary falls; it must be non-decreasing and end at `data.len()` offset by
/// `elem_offsets[0]`. Returns one `&mut` slice per block.
///
/// This is the safe alternative to handing threads raw pointers into one buffer: a `split_at_mut`
/// chain proves disjointness to the borrow checker, so a parallel pass that writes per-item output
/// into a preallocated buffer needs no `unsafe` and no per-item allocation. Used by the display
/// bake (per-face vertex/index runs) and the Laplacian 1-ring build (per-vertex neighbour runs) —
/// both had variable-length per-item output, which is exactly what `par_chunks_mut` cannot express.
pub(crate) fn split_at_boundaries<'a, T>(
    data: &'a mut [T],
    elem_offsets: &[usize],
) -> Vec<&'a mut [T]> {
    let mut out = Vec::with_capacity(elem_offsets.len().saturating_sub(1));
    let mut rest = data;
    let mut consumed = match elem_offsets.first() {
        Some(&f) => f,
        None => return out,
    };
    for w in elem_offsets.windows(2) {
        let take = w[1] - consumed;
        let (a, b) = rest.split_at_mut(take);
        out.push(a);
        rest = b;
        consumed = w[1];
    }
    out
}

/// Block boundaries over `n` items: `[0, .., n]`.
///
/// One block per worker with slack for load balance, but never smaller than `min_per_block` — a
/// small input collapses to ONE block so the same code runs serially instead of paying a fork that
/// would cost more than the work.
pub(crate) fn block_bounds(n: usize, min_per_block: usize) -> Vec<usize> {
    if n == 0 {
        return vec![0, 0];
    }
    let want = (rayon::current_num_threads() * 4).max(1);
    let nblocks = want.min(n.div_ceil(min_per_block.max(1))).max(1);
    let step = n.div_ceil(nblocks);
    let mut b: Vec<usize> = (0..nblocks).map(|i| (i * step).min(n)).collect();
    b.push(n);
    b.dedup();
    b
}
