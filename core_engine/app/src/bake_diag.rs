//! **Why is this map empty?** — the reason channel for the material / mesh-map bake path.
//!
//! ## The class of bug this exists to kill
//!
//! Debugging poly curvature (2026-08-10) turned up **six real defects in a row, every one of them
//! invisible**: the wrong body resolved, a `?` bail on a missing panel doc, Box projection baking
//! a wrapped average, shaping that clipped to a binary mask, an aliasing rasteriser, and no
//! normalisation. Each produced a black map — indistinguishable from "this feature does not
//! exist" — so five rounds went into *finding* them and minutes into fixing them.
//!
//! The shape is always the same: an `Option`-returning step bails with `?`, its caller substitutes
//! an empty default, and the emptiness propagates to a texture. CLAUDE.md already forbids it —
//! *"Wrapping in `Option` and silently returning `None` where a value should always exist"* — but
//! a rule cannot be followed without somewhere to put the reason. This is that somewhere.
//!
//! ## The contract
//!
//! **A bake step that produces nothing must say why, exactly once.** Not log-and-continue, not a
//! debug `eprintln` behind an env var the user will never set — a message that reaches the person
//! looking at the black map.
//!
//! * [`AzraApp::bake_note`] — frame-thread steps report directly (deduped; the enqueue path runs
//!   on every live edit, so an undeduped toast would be a stream).
//! * [`BakeNotes`] — worker-thread steps cannot toast, so they accumulate reasons and ride back
//!   with the result for the poll phase to surface.
//!
//! Deduping is on the message, and the FIRST occurrence always shows. A repeated identical reason
//! is noise; a changed reason is news.

/// Reasons collected on a worker thread, carried back with the bake result.
///
/// A worker cannot toast, and returning `Option<Vec<u8>>` throws the reason away — which is how a
/// graph that was ineligible for curvature precomputation ended up indistinguishable from a graph
/// with no curvature in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BakeNotes {
    notes: Vec<String>,
}

impl BakeNotes {
    pub fn push(&mut self, reason: impl Into<String>) {
        let r = reason.into();
        if !self.notes.iter().any(|n| *n == r) {
            self.notes.push(r);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.notes.iter().map(|s| s.as_str())
    }

    /// One line for a toast / status row.
    pub fn summary(&self) -> String {
        self.notes.join(" · ")
    }
}

/// Worker→frame handoff for bake reasons.
///
/// A `static` rather than a channel because the reason is diagnostic, not data: it must never
/// change what a bake produces, must not require threading a sender through six call layers that
/// exist for other purposes, and losing one under contention is harmless. Bounded so a runaway
/// bake loop cannot grow it without limit.
static WORKER_NOTES: std::sync::Mutex<Vec<BakeNotes>> = std::sync::Mutex::new(Vec::new());

/// Cap — reasons dedupe, so more than a handful pending means the poll is not draining and
/// keeping them would be a leak, not information.
const MAX_PENDING: usize = 8;

/// Called from a bake worker. Never blocks meaningfully and never panics on a poisoned lock —
/// a diagnostic channel must not be able to take down the bake it is reporting on.
pub(crate) fn stash_worker_notes(notes: BakeNotes) {
    if notes.is_empty() {
        return;
    }
    let Ok(mut q) = WORKER_NOTES.lock() else {
        return;
    };
    if q.len() >= MAX_PENDING || q.iter().any(|n| *n == notes) {
        return;
    }
    q.push(notes);
}

/// Frame-thread drain.
fn take_worker_notes() -> Vec<BakeNotes> {
    match WORKER_NOTES.lock() {
        Ok(mut q) => std::mem::take(&mut *q),
        Err(_) => Vec::new(),
    }
}

impl crate::AzraApp {
    /// Surface anything the bake workers reported since the last frame. Called from the poll
    /// phase; cheap when there is nothing (one uncontended lock).
    pub(crate) fn poll_bake_notes(&mut self) {
        for n in take_worker_notes() {
            self.bake_notes_report(&n);
        }
    }
}

impl crate::AzraApp {
    /// Report a bake-path outcome ONCE per distinct message.
    ///
    /// Use for any step that is about to produce nothing, or to produce something the user did not
    /// ask for. The bar is: *would a user staring at a black map be able to act on this?*
    pub(crate) fn bake_note(&mut self, reason: &str) {
        if self.ui.last_bake_note.as_deref() == Some(reason) {
            return;
        }
        self.ui.last_bake_note = Some(reason.to_string());
        self.toast(reason);
    }

    /// Drain worker-side notes once their result lands.
    pub(crate) fn bake_notes_report(&mut self, notes: &BakeNotes) {
        if notes.is_empty() {
            return;
        }
        let s = notes.summary();
        self.bake_note(&s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GATE — notes dedupe by content, and the first of each distinct reason survives.
    ///
    /// Dedupe is the whole reason this is a type rather than a `Vec<String>`: the bake enqueue
    /// runs on every live edit, so a step that reports each time would bury the one message that
    /// mattered.
    #[test]
    fn notes_dedupe_but_keep_every_distinct_reason() {
        let mut n = BakeNotes::default();
        assert!(n.is_empty());
        n.push("no UVs");
        n.push("no UVs");
        n.push("projection is Box");
        assert_eq!(n.iter().collect::<Vec<_>>(), vec!["no UVs", "projection is Box"]);
        assert_eq!(n.summary(), "no UVs · projection is Box");
    }
}
