use std::time::Instant;

/// Work priority classes for frame budget allocation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum WorkClass {
    /// Render dispatch, input handling — always runs, no budget check.
    Critical,
    /// Serialize, bind group rebuild — runs if budget permits.
    High,
    /// Bake readback, DC result, mesh upload — capped per frame.
    Medium,
    /// Composite rebuild, volume upload — deferred when busy.
    Low,
    /// Pipeline warmup, cache cleanup — only when no interaction.
    IdleOnly,
}

/// A ticket describing a unit of work to be scheduled.
pub(crate) struct WorkTicket {
    pub class: WorkClass,
    pub cost_hint_ms: f32,
    pub name: &'static str,
}

/// Per-frame log entry for the perf HUD.
#[derive(Clone)]
pub(crate) struct WorkLogEntry {
    pub name: &'static str,
    pub class: WorkClass,
    pub actual_ms: f32,
    pub ran: bool,
}

/// Budget-aware frame scheduler. Tracks per-frame work and decides what to defer.
pub(crate) struct FrameScheduler {
    pub frame_budget_ms: f32,       // target: 12.0ms (leaves 4ms for GPU + vsync)
    pub spent_ms: f32,              // accumulated this frame
    pub interaction_active: bool,   // set by viewport code each frame
    frame_start: Option<Instant>,
    pub work_log: Vec<WorkLogEntry>,
}

impl FrameScheduler {
    pub fn new() -> Self {
        Self {
            frame_budget_ms: 12.0,
            spent_ms: 0.0,
            interaction_active: false,
            frame_start: None,
            work_log: Vec::with_capacity(16),
        }
    }

    /// Call at start of each frame to reset counters.
    pub fn begin_frame(&mut self) {
        self.spent_ms = 0.0;
        self.frame_start = Some(Instant::now());
        self.work_log.clear();
    }

    /// Check if a work ticket should run given remaining budget.
    pub fn can_run(&self, ticket: &WorkTicket) -> bool {
        match ticket.class {
            WorkClass::Critical => true, // always
            WorkClass::High => self.remaining_ms() > ticket.cost_hint_ms * 0.5,
            WorkClass::Medium => self.remaining_ms() > ticket.cost_hint_ms,
            WorkClass::Low => self.remaining_ms() > ticket.cost_hint_ms * 2.0,
            WorkClass::IdleOnly => !self.interaction_active && self.remaining_ms() > ticket.cost_hint_ms,
        }
    }

    /// Record that a work ticket was executed with the given actual cost.
    pub fn record(&mut self, ticket: &WorkTicket, actual_ms: f32) {
        self.spent_ms += actual_ms;
        self.work_log.push(WorkLogEntry {
            name: ticket.name,
            class: ticket.class,
            actual_ms,
            ran: true,
        });
    }

    /// Record that a work ticket was skipped (for HUD logging).
    pub fn record_skipped(&mut self, ticket: &WorkTicket) {
        self.work_log.push(WorkLogEntry {
            name: ticket.name,
            class: ticket.class,
            actual_ms: 0.0,
            ran: false,
        });
    }

    /// Remaining budget in ms.
    pub fn remaining_ms(&self) -> f32 {
        (self.frame_budget_ms - self.spent_ms).max(0.0)
    }
}

#[cfg(test)]
mod bake_cost_hint_tests {
    use super::*;

    /// OWNER PERF 2026-07-29: a 128³ bake readback costs ~668 ms and stalled the frame after every
    /// project load (`[STALL] frame=686ms`). The `bake_readback` ticket declared
    /// `cost_hint_ms: 1.0`, and Medium admits work whenever `remaining_ms() > cost_hint_ms` — so
    /// with a 12 ms budget the gate could NEVER fire. The scheduler was not failing to do its job;
    /// it was told the job was cheap.
    ///
    /// This pins the mechanism: an honest hint defers, a lying hint does not.
    #[test]
    fn oversized_readback_is_deferred_only_when_the_hint_is_honest() {
        let mut s = FrameScheduler::new();
        s.begin_frame();

        let lying = WorkTicket {
            class: WorkClass::Medium,
            cost_hint_ms: 1.0, // the old hardcoded value
            name: "bake_readback",
        };
        let honest = WorkTicket {
            class: WorkClass::Medium,
            cost_hint_ms: 668.0, // what a 128³ readback actually costs
            name: "bake_readback",
        };

        assert!(s.can_run(&lying),
            "premise check: the OLD hint is admitted on a fresh frame — this is why a 668ms job \
             ran on the frame thread");
        assert!(!s.can_run(&honest),
            "a readback costing 668ms must NOT be admitted against a {}ms budget - if this passes, \
             the budget gate is not enforcing anything and the stall returns",
            s.frame_budget_ms);
    }

    /// The learned hint must not starve the FIRST readback — nothing is known yet, and blocking it
    /// would deadlock the initial bake. `bake_readback_worst_ms` starts at 0.0 and the ticket
    /// applies `.max(1.0)`.
    #[test]
    fn first_readback_still_runs_before_any_cost_is_learned() {
        let mut s = FrameScheduler::new();
        s.begin_frame();
        let seeded = WorkTicket {
            class: WorkClass::Medium,
            cost_hint_ms: 0.0f32.max(1.0), // what the ticket computes on a cold cache
            name: "bake_readback",
        };
        assert!(s.can_run(&seeded),
            "the first readback must run - with nothing learned the hint is 1.0, and blocking it \
             would mean no bake ever completes");
    }
}

#[cfg(test)]
mod starvation_tests {
    use super::*;

    /// OWNER 2026-07-29: "the SDF boolean works but sometimes the surface stops updating, and the
    /// viewport is NOT frozen."
    ///
    /// That was a DEADLOCK I introduced in `df44281`. A Medium ticket is admitted only while
    /// `remaining_ms() > cost_hint_ms`, and the frame budget is 12 ms. The bake-readback hint is
    /// LEARNED from the worst observed cost — so the moment any readback exceeded 12 ms, the hint
    /// permanently exceeded the whole budget and `can_run` returned false on every later frame. The
    /// pending queue never drained; the surface froze while the UI stayed perfectly responsive,
    /// because nothing was actually blocking.
    ///
    /// This pins the PREMISE — that a realistic learned hint really is unrunnable — so the
    /// starvation guard in `poll_primary_bake` cannot be removed as redundant.
    #[test]
    fn a_learned_hint_above_the_frame_budget_is_never_admitted() {
        let mut s = FrameScheduler::new();
        s.begin_frame();
        // Measured readback costs from the owner's own perf report.
        for worst in [12.0f32, 95.0, 668.0, 2958.0] {
            let t = WorkTicket {
                class: WorkClass::Medium,
                cost_hint_ms: worst,
                name: "bake_readback",
            };
            assert!(!s.can_run(&t),
                "a {worst}ms hint is admitted against a {}ms budget — then the gate is not gating; \
                 but if it is REFUSED on every frame with no escape, the bake queue starves forever \
                 (owner: 'surface stops updating, viewport not frozen'). The escape lives in \
                 poll_primary_bake's MAX_DEFERRED_FRAMES, which is why that guard must stay.",
                s.frame_budget_ms);
        }
    }

    /// The counterpart: a cheap hint must still be admitted on a fresh frame, or the guard would be
    /// masking a gate that never lets anything through.
    #[test]
    fn a_cheap_hint_is_still_admitted() {
        let mut s = FrameScheduler::new();
        s.begin_frame();
        let t = WorkTicket {
            class: WorkClass::Medium,
            cost_hint_ms: 1.0,
            name: "bake_readback",
        };
        assert!(s.can_run(&t), "a 1ms hint must run on a fresh 12ms frame");
    }
}
