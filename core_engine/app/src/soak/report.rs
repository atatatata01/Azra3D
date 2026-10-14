//! SOAK report — step lines, pixel verdict, finish summary.
//! Pure-relocated from soak.rs.

use crate::AzraApp;
use core_math::scene_graph::NodePayload;

use super::SoakPhase;
use super::scenarios::soak_active_steps;

const SLOW_MS: f32 = 16.0;
/// Channel delta (0..255) below this is ignored (dither / AA).
const PX_CHANNEL_EPS: u8 = 2;
/// Below this changed-pixel fraction, treat as NO-OP (unless already FAIL/UNWIRED).
const PX_NOOP_FRAC: f32 = 0.0005; // 0.05%

impl AzraApp {
    pub(crate) fn soak_emit_line(&mut self) {
        let i = self.soak.step_i;
        let name = soak_active_steps().get(i).map(|s| s.name).unwrap_or("?");
        let ms = self.soak.step_ms;
        let (prims, verts, nodes) = self.soak_state_counts();

        // For texturing / curvature steps, attach a post-settle probe so NO-OP lines
        // name the gate (not just "0% pixels changed").
        if name.starts_with("tex.") || name.starts_with("curv.") {
            let probe = self.soak_probe_curvature_path();
            Self::soak_log(
                &self.soak.out_dir.clone(),
                &format!("[SOAK] probe {name}: {probe}"),
            );
            // Extra: GPU buffer readback bisect on the radius / readback steps (phase 3).
            if name == "tex.curv_auto_radius"
                || name == "tex.curv_base_color"
                || name == "curv.readback_abc"
            {
                let rb = self.soak_probe_curv_readback();
                Self::soak_log(
                    &self.soak.out_dir.clone(),
                    &format!("[SOAK] curv_readback {name}: {rb}"),
                );
                match &mut self.soak.step_note {
                    None => self.soak.step_note = Some(rb.clone()),
                    Some(n) => {
                        n.push_str(" | ");
                        n.push_str(&rb);
                    }
                }
            }
            match &mut self.soak.step_note {
                None => self.soak.step_note = Some(probe),
                Some(n) => {
                    n.push_str(" | ");
                    n.push_str(&probe);
                }
            }
        }

        let px_frac = match (&self.soak.prev_rgba, &self.soak.last_rgba) {
            (Some((pw, ph, prev)), Some((w, h, cur))) if *pw == *w && *ph == *h => {
                pixel_change_frac(prev, cur, PX_CHANNEL_EPS)
            }
            (None, Some(_)) => 1.0, // first shot — everything "changed"
            _ => 0.0,
        };
        let px_pct = px_frac * 100.0;

        // Promote last → prev for next step.
        if let Some(rgba) = self.soak.last_rgba.take() {
            self.soak.prev_rgba = Some(rgba);
        }

        let verdict = if let Some(ref u) = self.soak.step_unwired {
            self.soak.n_unwired += 1;
            format!("UNWIRED:{u}")
        } else if let Some(ref f) = self.soak.step_fail {
            self.soak.n_fail += 1;
            format!("FAIL:{f}")
        } else if px_frac < PX_NOOP_FRAC {
            self.soak.n_noop += 1;
            let why = self
                .soak
                .step_note
                .clone()
                .unwrap_or_else(|| "0% pixels changed".into());
            format!("NO-OP:{why}")
        } else if ms > SLOW_MS {
            self.soak.n_slow += 1;
            // Keep step notes (e.g. save/load count match) visible under SLOW.
            if let Some(ref n) = self.soak.step_note {
                format!("SLOW (>{SLOW_MS:.0}ms) ({n})")
            } else {
                format!("SLOW (>{SLOW_MS:.0}ms)")
            }
        } else {
            self.soak.n_ok += 1;
            if let Some(ref n) = self.soak.step_note {
                format!("OK ({n})")
            } else {
                "OK".into()
            }
        };

        let line = format!(
            "[SOAK] {i:03} {name:<24} {ms:6.1}ms  prims={prims} vcap={verts} nodes={nodes}  px={px_pct:5.1}%  {verdict}"
        );
        let dir = self.soak.out_dir.clone();
        Self::soak_log(&dir, &line);
    }

    /// Post-settle snapshot of the vertex-curvature pipeline (the 0% texturing block).
    /// Mirrors the gates in `poll_vertex_curvature` + draw flag arming — SOAK-visible, not eprintln spam.
    fn soak_probe_curvature_path(&self) -> String {
        use crate::node_graph_bake::vertex_curv_keys_for_graph;
        let body = self.scene.active_body_nid;
        let mid_body = self.props_material_resolve_for_body(body);
        let mid_soak = self.soak.mat_id;
        let prim_mids: Vec<u32> = self
            .scene
            .primitives
            .iter()
            .map(|p| p.material_id.0)
            .collect();
        let prevailing = self.scene.scene_graph.body_prevailing_material(body);

        let Some(mid) = mid_body else {
            return format!(
                "gate=no_body_mid soak_mid={mid_soak:?} prevailing={prevailing:?} prim_mids={prim_mids:?}"
            );
        };
        let has_graph = self.ui.material_node_graphs.contains_key(&mid.0);
        let keys = self
            .ui
            .material_node_graphs
            .get(&mid.0)
            .map(|g| vertex_curv_keys_for_graph(g).len())
            .unwrap_or(0);
        let quiet = self.cache.body_vol_cache.sparse_quiet(body);
        let span = self.sdf_body_mesh_vertex_span(body);
        let vcurv = self.body_has_current_vertex_curvature(body);
        let albedo = self
            .scene
            .scene_graph
            .material_by_id(mid)
            .map(|(_, m)| format!("albedo=({:.2},{:.2},{:.2})", m.albedo.x, m.albedo.y, m.albedo.z))
            .unwrap_or_else(|| "albedo=?".into());

        let gate = if !has_graph {
            "no_node_graph"
        } else if keys == 0 {
            "no_vertex_curv_keys"
        } else if span.is_none() {
            "no_resident_mesh_span"
        } else if !quiet {
            "sparse_not_quiet"
        } else if vcurv.is_none() {
            "dispatch_or_slot_stale"
        } else {
            "armed"
        };

        format!(
            "gate={gate} mid={mid:?} soak_mid={mid_soak:?} prevailing={prevailing:?} \
             prim_mids={prim_mids:?} graph={has_graph} keys={keys} quiet={quiet} \
             span={span:?} flag={vcurv:?} {albedo}"
        )
    }

    /// Phase-3 (a)/(b)/(c) bisect: blocking readback of the body's `curv_vertex` buffer.
    /// Reports min/max/mean per channel over a sample + a diagnosis tag.
    fn soak_probe_curv_readback(&self) -> String {
        let body = self.scene.active_body_nid;
        let Some(gpu) = self.gpu.as_ref() else {
            return "readback=no_gpu".into();
        };
        let Some(slot) = gpu.sdf_body_curvature.get(&body) else {
            return "readback=no_slot".into();
        };
        let cap = slot.cap;
        let stats = crate::gpu_curvature::readback_curv_vertex_stats(
            &gpu.device,
            &gpu.queue,
            &slot.buf,
            cap,
        );
        let Some(s) = stats else {
            return format!("readback=failed cap={cap}");
        };
        let (smin, smax, smean) = s.written[1];
        let (mmin, mmax, mmean) = s.written[0];
        let pref = s
            .prefix_signed
            .map(|(a, b, c)| format!(" prefix_signed=[{a:.4},{b:.4}] mean={c:.4}"))
            .unwrap_or_default();
        format!(
            "diag={} base={} cap={} arrayLen={} maxVsIdx={} sample={} \
             ch1_signed=[{smin:.5},{smax:.5}] mean={smean:.5} \
             ch0_mask=[{mmin:.5},{mmax:.5}] mean={mmean:.5}{pref}",
            s.diagnosis,
            s.base_vertex,
            s.cap,
            s.array_len_f32,
            s.max_vs_idx,
            s.sample_n,
        )
    }

    pub(crate) fn soak_finish(&mut self) {
        let n_ok = self.soak.n_ok;
        let n_noop = self.soak.n_noop;
        let n_slow = self.soak.n_slow;
        let n_fail = self.soak.n_fail;
        let n_unwired = self.soak.n_unwired;
        let out = self.soak.out_dir.display().to_string();
        let dir = self.soak.out_dir.clone();
        Self::soak_log(
            &dir,
            &format!(
                "[SOAK] complete — OK={n_ok} NO-OP={n_noop} SLOW={n_slow} FAIL={n_fail} UNWIRED={n_unwired}  shots→{out}"
            ),
        );
        self.soak.phase = SoakPhase::Done;
        std::process::exit(if n_fail > 0 { 1 } else { 0 });
    }

    pub(crate) fn soak_state_counts(&self) -> (usize, usize, usize) {
        let prims = self.scene.primitives.len();
        // SDF bodies: report the GPU-RESIDENT span, not `cpu_mesh.vertices`.
        //
        // The CPU mesh is a lazily-rebuilt cache (`cpu_grid_stale`), refreshed only for rare
        // readers like save/export — so reading it made this column report 15608 for an entire
        // 22-step run, unchanged by add-sphere OR boolean-subtract. A state column that never
        // moves cannot reveal a regression; it just looks reassuring.
        //
        // This is CAPACITY, not the exact vertex count: the true count lives only in the
        // GPU-written indirect args and `resident.rs` states "NO mesh readback" as a contract, so
        // reading it back would stall the frame. Capacity is re-allocated on every re-mesh, so it
        // DOES track topology changes — which is what this column is for. Named `vcap` so the log
        // never implies a precision it does not have.
        let mut verts = 0usize;
        for (nid, node) in self.scene.scene_graph.sdf_bodies() {
            if let NodePayload::SdfBody { mesh_cache_id, .. } = &node.payload {
                if let Some((_, _, vert_cap, _)) = self.sdf_body_mesh_vertex_span(nid) {
                    verts += vert_cap as usize;
                } else if let Some(cid) = mesh_cache_id {
                    // Not resident (CPU-mesh fallback path) — the CPU mesh IS the truth here.
                    if let Some(m) = self.cache.mesh_cache.get(*cid) {
                        verts += m.cpu_mesh.vertices.len();
                    }
                }
            }
        }
        for (_nid, node) in self.scene.scene_graph.poly_bodies() {
            if let NodePayload::PolygonalBody {
                mesh_cache_id: Some(cid),
                ..
            } = &node.payload
            {
                if let Some(m) = self.cache.mesh_cache.get(*cid) {
                    verts += m.cpu_mesh.vertices.len();
                }
            }
        }
        let nodes = self.scene.scene_graph.node_count();
        (prims, verts, nodes)
    }
}

fn pixel_change_frac(prev: &[u8], cur: &[u8], eps: u8) -> f32 {
    if prev.len() != cur.len() || prev.is_empty() {
        return 1.0;
    }
    let n = prev.len() / 4;
    if n == 0 {
        return 0.0;
    }
    let mut changed = 0usize;
    for i in 0..n {
        let o = i * 4;
        let dr = prev[o].abs_diff(cur[o]);
        let dg = prev[o + 1].abs_diff(cur[o + 1]);
        let db = prev[o + 2].abs_diff(cur[o + 2]);
        if dr > eps || dg > eps || db > eps {
            changed += 1;
        }
    }
    changed as f32 / n as f32
}

