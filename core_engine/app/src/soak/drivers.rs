//! SOAK driver — state machine tick + step helpers.
//! Pure-relocated from soak.rs.

use std::path::PathBuf;
use std::time::Instant;

use crate::{AzraApp, DirtyFlags};

use super::{SoakHarness, SoakPhase};
use super::scenarios::{soak_active_steps, StepOutcome};

const WAIT_SHOT_MAX: u32 = 90;
const BOOT_FRAMES: u32 = 45; // wait for GPU init + first layout

// ─── Driver ──────────────────────────────────────────────────────────────────

impl AzraApp {
    /// Mirror a soak line to stderr + `<out_dir>/soak.log` (Windows GUI launch often
    /// detaches the console; the file is the durable sample log).
    pub(crate) fn soak_log(out_dir: &std::path::Path, line: &str) {
        eprintln!("{line}");
        let path = out_dir.join("soak.log");
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            use std::io::Write;
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
    }

    /// One tick of the soak state machine. Call only when `AZRA_SOAK` is set.
    pub(crate) fn soak_tick(&mut self) {
        let out_dir = match std::env::var("AZRA_SOAK") {
            Ok(d) if !d.is_empty() => PathBuf::from(d),
            _ => return,
        };

        if !self.soak.active {
            let _ = std::fs::create_dir_all(&out_dir);
            // Truncate prior run log.
            let _ = std::fs::write(out_dir.join("soak.log"), "");
            self.soak = SoakHarness {
                active: true,
                out_dir: out_dir.clone(),
                phase: SoakPhase::Boot,
                ..Default::default()
            };
            let n = soak_active_steps().len();
            let suite = std::env::var("AZRA_SOAK_SUITE").unwrap_or_else(|_| "full".into());
            Self::soak_log(
                &out_dir,
                &format!(
                    "[SOAK] start — {n} steps suite={suite} → {}",
                    out_dir.display()
                ),
            );
        }

        self.soak.frame = self.soak.frame.wrapping_add(1);

        match self.soak.phase {
            SoakPhase::Boot => {
                if self.soak.frame >= BOOT_FRAMES && self.gpu.is_some() {
                    self.soak.phase = SoakPhase::Action;
                    self.soak.step_i = 0;
                    let line = format!(
                        "[SOAK] boot done (frame {}) — running steps",
                        self.soak.frame
                    );
                    let dir = self.soak.out_dir.clone();
                    Self::soak_log(&dir, &line);
                }
            }
            SoakPhase::Action => {
                if self.soak.step_i >= soak_active_steps().len() {
                    self.soak_finish();
                    return;
                }
                let def = &soak_active_steps()[self.soak.step_i];
                let step_name = def.name;
                let settle = def.settle;
                self.soak.last_rgba = None;
                self.soak.waiting_shot = false;
                self.soak.step_note = None;
                self.soak.step_fail = None;
                self.soak.step_unwired = None;

                {
                    let dir = self.soak.out_dir.clone();
                    Self::soak_log(
                        &dir,
                        &format!("[SOAK] begin {:03} {step_name}", self.soak.step_i),
                    );
                }

                let t0 = Instant::now();
                // Isolate panics so one bad step cannot kill the whole session.
                let out = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    (soak_active_steps()[self.soak.step_i].run)(self)
                })) {
                    Ok(o) => o,
                    Err(_) => StepOutcome::fail("step panicked (caught)"),
                };
                self.soak.step_ms = t0.elapsed().as_secs_f32() * 1000.0;
                {
                    let dir = self.soak.out_dir.clone();
                    Self::soak_log(
                        &dir,
                        &format!(
                            "[SOAK] action_done {:03} {step_name} {:.1}ms",
                            self.soak.step_i, self.soak.step_ms
                        ),
                    );
                }
                self.soak.step_note = out.note;
                self.soak.step_fail = out.fail;
                self.soak.step_unwired = out.unwired;
                self.soak.step_t0 = Some(t0);

                self.soak.settle_left = settle;
                self.soak.phase = SoakPhase::Settle;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            SoakPhase::Settle => {
                if self.soak.settle_left > 0 {
                    self.soak.settle_left -= 1;
                    self.mark_dirty(DirtyFlags::RENDER);
                } else {
                    self.soak.wait_shot_frames = 0;
                    self.soak.phase = SoakPhase::RequestShot;
                }
            }
            SoakPhase::RequestShot => {
                // Only request when no screenshot is already in flight.
                // Cap wait on a stuck pending map — otherwise we never enter WaitShot
                // and the step has no timeout path (observed hang after poly.boolean).
                self.soak.wait_shot_frames = self.soak.wait_shot_frames.saturating_add(1);
                if self.pending_screenshot.is_none() && self.visdiff_shot.is_none() {
                    let def = &soak_active_steps()[self.soak.step_i];
                    let name = format!("{:03}_{}", self.soak.step_i, def.name.replace('.', "_"));
                    let path = self.soak.out_dir.join(format!("{name}.png"));
                    self.visdiff_shot = Some(path);
                    self.soak.waiting_shot = true;
                    self.soak.wait_shot_frames = 0;
                    self.soak.phase = SoakPhase::WaitShot;
                    self.mark_dirty(DirtyFlags::RENDER);
                } else if self.soak.wait_shot_frames >= WAIT_SHOT_MAX {
                    if self.soak.step_note.is_none() {
                        self.soak.step_note = Some(format!(
                            "RequestShot stuck (pending map) after {} frames",
                            self.soak.wait_shot_frames
                        ));
                    }
                    self.soak_emit_line();
                    self.soak.step_i += 1;
                    self.soak.phase = SoakPhase::Action;
                    self.soak.waiting_shot = false;
                    self.soak.wait_shot_frames = 0;
                } else {
                    self.mark_dirty(DirtyFlags::RENDER);
                }
            }
            SoakPhase::WaitShot => {
                self.soak.wait_shot_frames += 1;
                if self.soak.last_rgba.is_some() || self.soak.wait_shot_frames >= WAIT_SHOT_MAX {
                    if self.soak.last_rgba.is_none() {
                        // Still emit a line — NO-OP / FAIL will reflect missing pixels.
                        if self.soak.step_note.is_none() {
                            self.soak.step_note = Some(format!(
                                "shot timeout after {} frames",
                                self.soak.wait_shot_frames
                            ));
                        }
                    }
                    self.soak_emit_line();
                    self.soak.step_i += 1;
                    self.soak.phase = SoakPhase::Action;
                    self.soak.waiting_shot = false;
                } else {
                    self.mark_dirty(DirtyFlags::RENDER);
                }
            }
            SoakPhase::Done => {}
        }
    }

    /// Auto curvature radius for the soak's active body — the SAME production function the
    /// panel's "Auto" button calls, so the harness cannot drift from the shipped behaviour.
    /// Falls back to a visible fraction if there is no measurable body.
    pub(crate) fn soak_auto_curv_radius(&self) -> f32 {
        let nid = self.soak_active_body();
        self.scene
            .scene_graph
            .body_primitives(nid)
            .filter(|pr| !pr.is_empty())
            .map(|pr| {
                let mut src = crate::node_graph_bake::SdfFieldSources::default();
                src.body_prims = pr.to_vec();
                let (bmin, bmax) = src.body_aabb(0);
                crate::material_panel::lower::curvature_auto_radius_aabb(bmin, bmax)
            })
            .unwrap_or(0.16)
    }

    pub(crate) fn soak_active_body(&self) -> core_math::scene_graph::NodeId {
        self.scene.active_body_nid
    }
}
