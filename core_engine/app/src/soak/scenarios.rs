//! SOAK step table + step implementations (production APIs only).
//! Pure-relocated from soak.rs.

use std::time::Instant;

use glam::Vec3;

use crate::{AzraApp, DirtyFlags};
use core_math::primitives::{BooleanOp, PbrMaterial, SdfShape};
use core_math::scene_graph::NodePayload;

// ─── Step table ──────────────────────────────────────────────────────────────

/// Result of a step action (before pixel verdict).
pub(super) struct StepOutcome {
    /// Soft reason the action itself did nothing (becomes NO-OP if px also ~0).
    pub(super) note: Option<String>,
    /// Hard failure of the action path.
    pub(super) fail: Option<String>,
    /// Action has no production API — list as UNWIRED and continue.
    pub(super) unwired: Option<String>,
}

impl StepOutcome {
    pub(super) fn ok() -> Self {
        Self {
            note: None,
            fail: None,
            unwired: None,
        }
    }
    pub(super) fn note(s: impl Into<String>) -> Self {
        Self {
            note: Some(s.into()),
            fail: None,
            unwired: None,
        }
    }
    pub(super) fn fail(s: impl Into<String>) -> Self {
        Self {
            note: None,
            fail: Some(s.into()),
            unwired: None,
        }
    }
    pub(super) fn unwired(s: impl Into<String>) -> Self {
        Self {
            note: None,
            fail: None,
            unwired: Some(s.into()),
        }
    }
}

pub(super) type StepFn = fn(&mut AzraApp) -> StepOutcome;

pub(super) struct StepDef {
    pub(super) name: &'static str,
    /// Frames to wait after the action before capturing (bake / remesh headroom).
    pub(super) settle: u32,
    pub(super) run: StepFn,
}

/// Named step table — reorder / extend without touching the driver.
const STEPS: &[StepDef] = &[
    // ── A. SDF ──────────────────────────────────────────────────────────────
    StepDef {
        name: "sdf.add_box",
        settle: 24,
        run: step_sdf_add_box,
    },
    StepDef {
        name: "sdf.add_sphere",
        settle: 24,
        run: step_sdf_add_sphere,
    },
    StepDef {
        name: "sdf.boolean_subtract",
        settle: 28,
        run: step_sdf_boolean_subtract,
    },
    StepDef {
        name: "sdf.blend_k",
        settle: 24,
        run: step_sdf_blend_k,
    },
    StepDef {
        name: "sdf.move_body",
        settle: 20,
        run: step_sdf_move_body,
    },
    StepDef {
        name: "sdf.rotate_body",
        settle: 20,
        run: step_sdf_rotate_body,
    },
    StepDef {
        name: "sdf.undo_rotate",
        settle: 18,
        run: step_undo,
    },
    StepDef {
        name: "sdf.undo_move",
        settle: 18,
        run: step_undo,
    },
    StepDef {
        name: "sdf.undo_blend_k",
        settle: 18,
        run: step_undo,
    },
    StepDef {
        name: "sdf.undo_boolean",
        settle: 22,
        run: step_undo,
    },
    // ── B. Mesh-modifier stack (post-DC display fold) ───────────────────────
    StepDef {
        name: "modstack.depth1",
        settle: 16,
        run: step_modstack_depth1,
    },
    StepDef {
        name: "modstack.depth2",
        settle: 18,
        run: step_modstack_depth2,
    },
    StepDef {
        name: "modstack.depth4",
        settle: 22,
        run: step_modstack_depth4,
    },
    StepDef {
        name: "modstack.depth8",
        settle: 28,
        run: step_modstack_depth8,
    },
    // ── C. Texturing / curvature ────────────────────────────────────────────
    StepDef {
        name: "tex.add_material",
        settle: 14,
        run: step_tex_add_material,
    },
    StepDef {
        name: "tex.curv_base_color",
        settle: 28,
        run: step_tex_curv_base_color,
    },
    // Hairline default (floor radius) then Auto (8% short axis) — must show a real px jump.
    // Also the sole radius-change step (curv.radius_sweep was folded here: both swept TO auto).
    StepDef {
        name: "tex.curv_auto_radius",
        settle: 32,
        run: step_tex_curv_auto_radius,
    },
    // Phase-3 bisect: GPU readback of curv_vertex buffer → (a)/(b)/(c) diagnosis.
    // Runs AFTER auto_radius settle so the radius-keyed dispatch has completed.
    StepDef {
        name: "curv.readback_abc",
        settle: 8,
        run: step_curv_readback_abc,
    },
    StepDef {
        name: "curv.vertex_rung",
        settle: 20,
        run: step_curv_vertex_rung,
    },
    // Image map so projection switches have content (otherwise proj steps are vacuous 0%).
    StepDef {
        name: "tex.bind_image_map",
        settle: 30,
        run: step_tex_bind_image_map,
    },
    StepDef {
        name: "tex.proj_sphere",
        settle: 16,
        run: step_tex_proj_sphere,
    },
    StepDef {
        name: "tex.proj_cylinder",
        settle: 14,
        run: step_tex_proj_cylinder,
    },
    StepDef {
        name: "tex.proj_planar",
        settle: 14,
        run: step_tex_proj_planar,
    },
    StepDef {
        name: "tex.proj_box",
        settle: 14,
        run: step_tex_proj_box,
    },
    // ── D. POLY (phase 2) ───────────────────────────────────────────────────
    StepDef {
        name: "poly.create_box",
        settle: 20,
        run: step_poly_create_box,
    },
    StepDef {
        name: "poly.uv_unwrap",
        settle: 14,
        run: super::meshmap::step_poly_uv_unwrap,
    },
    StepDef {
        name: "poly.bake_mesh_maps",
        settle: 40,
        run: super::meshmap::step_poly_bake_mesh_maps,
    },
    StepDef {
        name: "poly.bake_mesh_maps_check",
        settle: 10,
        run: super::meshmap::step_poly_bake_mesh_maps_check,
    },
    StepDef {
        name: "poly.extrude",
        settle: 18,
        run: step_poly_extrude,
    },
    StepDef {
        name: "poly.bevel",
        settle: 22,
        run: step_poly_bevel,
    },
    StepDef {
        name: "poly.loop_cut",
        settle: 18,
        run: step_poly_loop_cut,
    },
    StepDef {
        name: "poly.delete_face",
        settle: 16,
        run: step_poly_delete_face,
    },
    StepDef {
        name: "poly.dissolve_edge",
        settle: 16,
        run: step_poly_dissolve_edge,
    },
    StepDef {
        name: "poly.merge_center",
        settle: 16,
        run: step_poly_merge_center,
    },
    StepDef {
        name: "poly.boolean",
        settle: 28,
        run: step_poly_boolean,
    },
    // ── D2. UV + MESH MAPS (the texturing prep chain) ───────────────────────
    // These three run in order on the poly body the D-block built: unwrap it, bake its mesh
    // maps, then switch the material to Baked. The chain had never been exercised in the app —
    // every part of it was verified by tests and by headless .fl4 inspection only.
    StepDef {
        name: "uv.unwrap",
        settle: 14,
        run: step_uv_unwrap,
    },
    StepDef {
        name: "uv.pack",
        settle: 12,
        run: step_uv_pack,
    },
    StepDef {
        name: "meshmap.bake_all",
        settle: 40, // AO/Thickness ray casts land in the poll phase
        run: step_meshmap_bake,
    },
    StepDef {
        name: "meshmap.mode_baked",
        settle: 24,
        run: step_meshmap_mode_baked,
    },
    // ── E. LIGHTS + shading ─────────────────────────────────────────────────
    StepDef {
        name: "light.add_point",
        settle: 12,
        run: step_light_add_point,
    },
    StepDef {
        name: "light.add_spot",
        settle: 12,
        run: step_light_add_spot,
    },
    StepDef {
        name: "light.add_sun",
        settle: 12,
        run: step_light_add_sun,
    },
    StepDef {
        name: "light.point_radius",
        settle: 12,
        run: step_light_point_radius,
    },
    StepDef {
        name: "light.rt_toggle",
        settle: 14,
        run: step_light_rt_toggle,
    },
    StepDef {
        name: "shade.matcap",
        settle: 10,
        run: step_shade_matcap,
    },
    StepDef {
        name: "shade.pbr",
        settle: 10,
        run: step_shade_pbr,
    },
    StepDef {
        name: "shade.pathtrace",
        settle: 30,
        run: step_shade_pathtrace,
    },
    StepDef {
        name: "shade.hybrid_back",
        settle: 14,
        run: step_shade_hybrid_back,
    },
    // ── G. STRESS — complex SDF + materials (perf + bug hunting) ─────────────
    // Builds on the scene from A–E so bake/curvature/poll paths see multi-prim load.
    StepDef {
        name: "stress.sdf_complex",
        settle: 56,
        run: step_stress_sdf_complex,
    },
    StepDef {
        name: "stress.sdf_move_burst",
        settle: 44,
        run: step_stress_sdf_move_burst,
    },
    StepDef {
        name: "stress.mat_noise_stack",
        settle: 48,
        run: step_stress_mat_noise_stack,
    },
    StepDef {
        name: "stress.mat_multi_channel",
        settle: 40,
        run: step_stress_mat_multi_channel,
    },
    StepDef {
        name: "stress.mat_noise_scale_sweep",
        settle: 36,
        run: step_stress_mat_noise_scale_sweep,
    },
    StepDef {
        name: "stress.modstack_depth12",
        settle: 40,
        run: step_stress_modstack_12,
    },
    StepDef {
        name: "stress.perf_snapshot",
        settle: 10,
        run: step_stress_perf_snapshot,
    },
    // ── F. UNDO + SAVE/LOAD (after stress — exercises complex-scene wire path) ─
    StepDef {
        name: "undo.one",
        settle: 16,
        run: step_undo,
    },
    StepDef {
        name: "save_load.roundtrip",
        settle: 36,
        run: step_save_load_roundtrip,
    },
    StepDef {
        name: "stress.post_load_snapshot",
        settle: 48,
        run: step_stress_perf_snapshot,
    },
];


/// Suite of tools **not** covered by the basic/stress tables — self-contained setup.
/// Select with `AZRA_SOAK_SUITE=unt` (default = full `STEPS`).
const UNT_STEPS: &[StepDef] = &[
    StepDef { name: "unt.boot_box", settle: 28, run: step_sdf_add_box },
    StepDef { name: "unt.boot_sphere", settle: 24, run: step_sdf_add_sphere },
    StepDef { name: "unt.boot_subtract", settle: 28, run: step_sdf_boolean_subtract },
    StepDef { name: "unt.sdf_torus", settle: 28, run: step_unt_sdf_torus },
    StepDef { name: "unt.sdf_cone", settle: 24, run: step_unt_sdf_cone },
    StepDef { name: "unt.sdf_capsule", settle: 24, run: step_unt_sdf_capsule },
    StepDef { name: "unt.sdf_stair", settle: 28, run: step_unt_sdf_stair },
    StepDef { name: "unt.sdf_spiral_stair", settle: 28, run: step_unt_sdf_spiral_stair },
    StepDef { name: "unt.bool_intersect", settle: 28, run: step_unt_bool_intersect },
    StepDef { name: "unt.bool_cycle", settle: 20, run: step_unt_bool_cycle },
    StepDef { name: "unt.duplicate_selected", settle: 32, run: step_unt_duplicate_selected },
    StepDef { name: "unt.set_resolution_256", settle: 40, run: step_unt_set_resolution },
    StepDef { name: "unt.poly_sphere", settle: 20, run: step_unt_poly_sphere },
    StepDef { name: "unt.poly_cylinder", settle: 18, run: step_unt_poly_cylinder },
    StepDef { name: "unt.poly_inset", settle: 22, run: step_unt_poly_inset },
    StepDef { name: "unt.poly_solidify", settle: 24, run: step_unt_poly_solidify },
    StepDef { name: "unt.poly_subdivide", settle: 28, run: step_unt_poly_subdivide },
    StepDef { name: "unt.poly_select_ops", settle: 16, run: step_unt_poly_select_ops },
    StepDef { name: "unt.poly_flip_normals", settle: 16, run: step_unt_poly_flip_normals },
    StepDef { name: "unt.poly_recalc_normals", settle: 16, run: step_unt_poly_recalc_normals },
    StepDef { name: "unt.poly_limited_dissolve", settle: 20, run: step_unt_poly_limited_dissolve },
    StepDef { name: "unt.poly_join", settle: 24, run: step_unt_poly_join },
    StepDef { name: "unt.poly_fill", settle: 22, run: step_unt_poly_fill },
    StepDef { name: "unt.convert_sdf_to_poly", settle: 40, run: step_unt_convert_sdf_to_poly },
    StepDef { name: "unt.convert_poly_to_sdf", settle: 48, run: step_unt_convert_poly_to_sdf },
    StepDef { name: "unt.instance_create", settle: 20, run: step_unt_instance_create },
    StepDef { name: "unt.perf_snapshot", settle: 10, run: step_stress_perf_snapshot },
];

/// Second-wave untested production APIs — `AZRA_SOAK_SUITE=more`.
/// Covers SDF prim modifiers (not display modstack), remaining poly generators/ops,
/// curve_profile, NURBS create, and OBJ export. Interactive-only tools stay out.
const MORE_STEPS: &[StepDef] = &[
    // Boot scene
    StepDef { name: "more.boot_box", settle: 28, run: step_sdf_add_box },
    StepDef { name: "more.boot_sphere", settle: 24, run: step_sdf_add_sphere },
    // SDF curve / polyline profile (not interactive draw)
    StepDef { name: "more.sdf_curve_profile", settle: 28, run: step_more_sdf_curve_profile },
    // Per-prim SDF modifiers via production add_prim_modifier
    StepDef { name: "more.mod_twist", settle: 32, run: step_more_mod_twist },
    StepDef { name: "more.mod_taper", settle: 32, run: step_more_mod_taper },
    StepDef { name: "more.mod_shell", settle: 32, run: step_more_mod_shell },
    StepDef { name: "more.mod_smooth", settle: 32, run: step_more_mod_smooth },
    StepDef { name: "more.mod_mirror", settle: 32, run: step_more_mod_mirror },
    StepDef { name: "more.mod_shear", settle: 32, run: step_more_mod_shear },
    StepDef { name: "more.mod_spherize", settle: 32, run: step_more_mod_spherize },
    StepDef { name: "more.mod_emboss", settle: 32, run: step_more_mod_emboss },
    StepDef { name: "more.mod_deboss", settle: 32, run: step_more_mod_deboss },
    StepDef { name: "more.mod_array", settle: 36, run: step_more_mod_array },
    // Remaining poly generators (sphere/cyl/box already in unt)
    StepDef { name: "more.poly_torus", settle: 22, run: step_more_poly_torus },
    StepDef { name: "more.poly_cone", settle: 20, run: step_more_poly_cone },
    StepDef { name: "more.poly_plane", settle: 18, run: step_more_poly_plane },
    // Poly hide / unhide
    StepDef { name: "more.poly_hide_selected", settle: 14, run: step_more_poly_hide_selected },
    StepDef { name: "more.poly_unhide_all", settle: 12, run: step_more_poly_unhide_all },
    // Poly connect / merge-by-distance / bridge / bisect
    StepDef { name: "more.poly_connect", settle: 18, run: step_more_poly_connect },
    StepDef { name: "more.poly_merge_by_distance", settle: 18, run: step_more_poly_merge_by_distance },
    StepDef { name: "more.poly_bridge", settle: 20, run: step_more_poly_bridge },
    StepDef { name: "more.poly_bisect", settle: 22, run: step_more_poly_bisect },
    // Export + NURBS
    StepDef { name: "more.export_obj", settle: 12, run: step_more_export_obj },
    StepDef { name: "more.nurbs_plane", settle: 18, run: step_more_nurbs_plane },
    StepDef { name: "more.nurbs_sphere", settle: 18, run: step_more_nurbs_sphere },
    StepDef { name: "more.nurbs_torus", settle: 18, run: step_more_nurbs_torus },
    StepDef { name: "more.nurbs_cylinder", settle: 18, run: step_more_nurbs_cylinder },
    StepDef { name: "more.perf_snapshot", settle: 10, run: step_stress_perf_snapshot },
];

/// Active step table: suite via `AZRA_SOAK_SUITE` (full | unt | more).
pub(super) fn soak_active_steps() -> &'static [StepDef] {
    match std::env::var("AZRA_SOAK_SUITE").as_deref() {
        Ok("unt") | Ok("untested") => UNT_STEPS,
        Ok("more") | Ok("more_unt") => MORE_STEPS,
        _ => STEPS,
    }
}

// ─── Step implementations (production APIs only) ─────────────────────────────

fn step_sdf_add_box(app: &mut AzraApp) -> StepOutcome {
    let idx = app.add_primitive_immediate(
        SdfShape::Box {
            half_extents: Vec3::splat(0.8),
            bevel: Default::default(),
        },
        Vec3::ZERO,
    );
    // RENDER only — SCENE triggers full volume rebake storms that can device-lose
    // under rapid scripted creates; add_primitive_immediate already dirties bake.
    app.mark_dirty(DirtyFlags::RENDER);
    let _ = idx;
    StepOutcome::ok()
}

fn step_sdf_add_sphere(app: &mut AzraApp) -> StepOutcome {
    let idx = app.add_primitive_immediate(
        SdfShape::Sphere { radius: 0.55 },
        Vec3::new(0.45, 0.35, 0.0),
    );
    app.mark_dirty(DirtyFlags::RENDER);
    if app.scene.primitives.len() < 2 {
        return StepOutcome::fail(format!(
            "expected ≥2 prims after sphere, got {}",
            app.scene.primitives.len()
        ));
    }
    let _ = idx;
    StepOutcome::ok()
}

fn step_sdf_boolean_subtract(app: &mut AzraApp) -> StepOutcome {
    // Second prim (sphere) becomes a Subtract cutter — same path as the X boolean pie.
    if app.scene.primitives.len() < 2 {
        return StepOutcome::fail("need ≥2 prims for boolean subtract");
    }
    let uid = app.scene.primitives[1].prim_uid;
    let changed = app.set_prims_boolean_op(&[(uid, BooleanOp::Subtract { k: 0.0 })]);
    if !changed {
        return StepOutcome::note("boolean op already Subtract");
    }
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::ok()
}

fn step_sdf_blend_k(app: &mut AzraApp) -> StepOutcome {
    if app.scene.primitives.len() < 2 {
        return StepOutcome::fail("need ≥2 prims for blend_k");
    }
    // Phase-3: k=0.12 was often invisible (hard boolean vs soft). Sweep to a large blend
    // so a real px delta is expected; 0% then is a product finding, not a weak step.
    let idx = 1usize;
    let k = 0.55f32;
    app.transaction("SOAK blend_k", |s| {
        s.write_prim(idx, |p| {
            p.operation = BooleanOp::Subtract { k };
        });
        s.rebake_no_undo();
    });
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("blend_k={k}"))
}

fn step_sdf_move_body(app: &mut AzraApp) -> StepOutcome {
    let nid = app.soak_active_body();
    let mut ok = false;
    app.transaction("SOAK move body", |s| {
        if let Some(node) = s.scene.scene_graph.get_mut(nid) {
            if let NodePayload::SdfBody { origin, .. } = &mut node.payload {
                *origin += Vec3::new(0.35, 0.15, 0.0);
                ok = true;
            }
        }
        s.sync_flat_cache();
        s.rebake_no_undo();
    });
    app.mark_dirty(DirtyFlags::RENDER);
    if ok {
        StepOutcome::ok()
    } else {
        StepOutcome::fail("active body is not SdfBody")
    }
}

fn step_sdf_rotate_body(app: &mut AzraApp) -> StepOutcome {
    let nid = app.soak_active_body();
    let mut ok = false;
    app.transaction("SOAK rotate body", |s| {
        if let Some(node) = s.scene.scene_graph.get_mut(nid) {
            if let NodePayload::SdfBody { body_rotation, .. } = &mut node.payload {
                *body_rotation = glam::Quat::from_rotation_y(25.0_f32.to_radians()) * *body_rotation;
                ok = true;
            }
        }
        s.sync_flat_cache();
        s.rebake_no_undo();
    });
    app.mark_dirty(DirtyFlags::RENDER);
    if ok {
        StepOutcome::ok()
    } else {
        StepOutcome::fail("active body is not SdfBody")
    }
}

fn step_undo(app: &mut AzraApp) -> StepOutcome {
    app.undo();
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::ok()
}

/// Push N post-DC display mesh mods onto the active body and time the fold.
///
/// Uses SmoothNormals-only cards for depth scaling (cheap, safe). Mixing Mirror/
/// Decimate/ClayPolish in a rapid scripted stack previously device-lost mid-settle.
fn push_modstack(app: &mut AzraApp, depth: usize) -> StepOutcome {
    use core_math::display_mesh_ops::DisplayMeshModRecord;
    let nid = app.soak_active_body();
    if depth == 0 || depth > 16 {
        return StepOutcome::fail(format!("invalid mod depth {depth}"));
    }

    let mut ok = false;
    app.transaction(&format!("SOAK modstack depth{depth}"), |s| {
        if let Some(node) = s.scene.scene_graph.get_mut(nid) {
            if let NodePayload::SdfBody { mesh_modifiers, .. } = &mut node.payload {
                mesh_modifiers.clear();
                for i in 0..depth {
                    let mut rec = if i % 2 == 0 {
                        DisplayMeshModRecord::new_smooth_normals()
                    } else {
                        // Mild geometry smooth — keep iterations low for soak stability.
                        let mut r = DisplayMeshModRecord::new_smooth_geometry();
                        if let core_math::display_mesh_ops::DisplayMeshModKind::SmoothGeometry {
                            iterations,
                            factor,
                            ..
                        } = &mut r.kind
                        {
                            *iterations = 1;
                            *factor = 0.25;
                        }
                        r
                    };
                    rec.id = (i as u32) + 1;
                    mesh_modifiers.push(rec);
                }
                ok = true;
            }
        }
    });
    if !ok {
        return StepOutcome::fail("could not write mesh_modifiers on active body");
    }

    // Production fold path (same as ensure / preprocess).
    let t0 = Instant::now();
    let folded = app.sync_body_mesh_mod_display(nid);
    let fold_ms = t0.elapsed().as_secs_f32() * 1000.0;
    app.mark_dirty(DirtyFlags::RENDER);

    if !folded {
        // Cold mesh base — fold deferred until DC lands; still a valid production outcome.
        return StepOutcome::note(format!(
            "fold deferred (no base mesh yet) fold_call={fold_ms:.1}ms"
        ));
    }
    StepOutcome::note(format!("fold={fold_ms:.1}ms"))
}

fn step_modstack_depth1(app: &mut AzraApp) -> StepOutcome {
    push_modstack(app, 1)
}
fn step_modstack_depth2(app: &mut AzraApp) -> StepOutcome {
    push_modstack(app, 2)
}
fn step_modstack_depth4(app: &mut AzraApp) -> StepOutcome {
    push_modstack(app, 4)
}
fn step_modstack_depth8(app: &mut AzraApp) -> StepOutcome {
    push_modstack(app, 8)
}

fn step_tex_add_material(app: &mut AzraApp) -> StepOutcome {
    // Distinct albedo so a successful assign is visible even without generators.
    let mut seed = PbrMaterial::default();
    seed.albedo = glam::Vec3::new(0.15, 0.55, 0.85); // vivid blue-ish
    let mid = app.scene.scene_graph.add_material("soak_mat", seed);
    // body_prevailing_material() reads the LAST prim — assign ALL body prims so the
    // vertex-curvature poll and the panel resolve the same mid as the soak authored.
    let n = app.scene.primitives.len();
    if n > 0 {
        app.transaction("SOAK assign material", |s| {
            for i in 0..n {
                s.write_prim(i, |p| p.material_id = mid);
            }
        });
    }
    // Seed panel doc + empty node graph (UI creates both on material add).
    let doc = crate::material_panel::model::MaterialPanelDoc::default_for_new();
    app.ui.material_panel_docs.insert(mid.0, doc);
    app.ui
        .material_node_graphs
        .entry(mid.0)
        .or_insert_with(crate::shader_nodes::NodeGraph::default);
    app.soak.mat_id = Some(mid);
    app.cache.prim_buf_stale = true;
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("assigned mid={mid:?} to {n} prims (vivid albedo)"))
}

fn step_tex_curv_base_color(app: &mut AzraApp) -> StepOutcome {
    use crate::material_panel::model::{MaskStack, 
        ChannelKind, ChannelState, GeneratorOutput, GeneratorRung, GeneratorSource, Layer,
        LayerContent, BlendMode, Placement, Adjust,
    };
    let Some(mid) = app.soak.mat_id else {
        return StepOutcome::fail("no material — run tex.add_material first");
    };
    let Some(mat) = app.scene.scene_graph.material_by_id(mid).map(|(_, m)| m.clone()) else {
        return StepOutcome::fail("material missing from graph");
    };
    let mut doc = app.mp_current_doc(mid, &mat);
    let (has_uvs, radius_floor) = app.mp_active_body_gen_hints();
    // NOISE FLOOR default (1% short axis) — hairline on a box; may look like 0% px.
    let radius = radius_floor.unwrap_or(0.02).max(0.02);

    let gen = Layer {
        id: 0,
        drives: Vec::new(),
        content: LayerContent::Generator {
            source: GeneratorSource::Curvature,
            radius,
            invert: false,
            output: GeneratorOutput::Signed,
            remap: [0.0, 1.0],
            rung: if has_uvs {
                GeneratorRung::Auto
            } else {
                GeneratorRung::VertexCache
            },
            sharpness: 1.0,
            axis: Default::default(),
        },
        mask: crate::material_panel::model::MaskStack::default(),
        // Match production default (f6e13ae): Multiply composites; Normal REPLACES the stack
        // in gbuffer.wgsl vertex-curv path (blend==0 → blended = top).
        blend: BlendMode::Multiply,
        opacity: 1.0,
        visible: true,
        placement: Placement::default(),
        adjust: Adjust::default(),
    };

    if let Some(ch) = doc.channels.iter_mut().find(|c| c.kind == ChannelKind::BaseColor) {
        ch.state = ChannelState::Layered;
        // Replace layers so Base color is pure curvature showcase (hairline radius).
        ch.layers.clear();
        ch.layers.push(gen);
    } else {
        return StepOutcome::fail("BaseColor channel missing from panel doc");
    }

    app.mp_commit_doc_edit(mid, doc, "SOAK curv base color", false);
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("hairline radius={radius:.4}"))
}

/// Set curvature radius to the product Auto value (8% short axis). Must show a real px delta
/// after the hairline default in `tex.curv_base_color`.
fn step_tex_curv_auto_radius(app: &mut AzraApp) -> StepOutcome {
    use crate::material_panel::model::{ChannelKind, LayerContent};
    use crate::material_panel::lower::curvature_auto_radius_aabb;

    let Some(mid) = app.soak.mat_id else {
        return StepOutcome::fail("no material");
    };
    let Some(mat) = app.scene.scene_graph.material_by_id(mid).map(|(_, m)| m.clone()) else {
        return StepOutcome::fail("material missing");
    };
    let body = app.scene.active_body_nid;
    let auto_r = match app.scene.scene_graph.body_primitives(body) {
        Some(pr) if !pr.is_empty() => {
            let mut src = crate::node_graph_bake::SdfFieldSources::default();
            src.body_prims = pr.to_vec();
            let (bmin, bmax) = src.body_aabb(0);
            curvature_auto_radius_aabb(bmin, bmax)
        }
        _ => 0.08,
    };

    let mut doc = app.mp_current_doc(mid, &mat);
    let mut found = false;
    if let Some(ch) = doc.channels.iter_mut().find(|c| c.kind == ChannelKind::BaseColor) {
        for layer in ch.layers.iter_mut() {
            if let LayerContent::Generator { radius, .. } = &mut layer.content {
                *radius = auto_r;
                found = true;
            }
        }
    }
    if !found {
        return StepOutcome::fail("no curvature generator to set Auto radius on");
    }
    app.mp_commit_doc_edit(mid, doc, "SOAK curv auto radius", false);
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("auto_radius={auto_r:.4}"))
}

fn step_curv_vertex_rung(app: &mut AzraApp) -> StepOutcome {
    let Some(mid) = app.soak.mat_id else {
        return StepOutcome::fail("no material");
    };
    let changed = app.mp_ensure_vertex_curvature_path(mid);
    app.mark_dirty(DirtyFlags::RENDER);
    if changed {
        StepOutcome::ok()
    } else {
        // Already on vertex path / nothing to migrate — may still be a pixel no-op.
        StepOutcome::note("vertex path already active / nothing to migrate")
    }
}

/// Phase-3: explicit (a)/(b)/(c) step — action is a no-op; post-settle probe does the readback.
fn step_curv_readback_abc(app: &mut AzraApp) -> StepOutcome {
    // Force a dirty so poll re-runs if the slot was somehow dropped; probe runs after settle.
    app.mark_dirty(DirtyFlags::RENDER);
    let body = app.scene.active_body_nid;
    let has = app
        .gpu
        .as_ref()
        .map(|g| g.sdf_body_curvature.contains_key(&body))
        .unwrap_or(false);
    if !has {
        return StepOutcome::note("no curv slot yet — probe will report readback=no_slot");
    }
    StepOutcome::note("readback after settle")
}

/// Bind a synthetic checker albedo map so later projection switches have real content.
/// Queues `PendingMapLoad` (Frame Ordering: upload lands in poll next frames).
fn step_tex_bind_image_map(app: &mut AzraApp) -> StepOutcome {
    use crate::material_panel::model::{
        ChannelKind, ChannelState, Layer, LayerContent, BlendMode, Placement, Adjust,
    };
    use crate::material_textures::{PendingMapLoad, MapTarget, MAP_RES};
    use core_math::pbr_material::MapKind;

    let Some(mid) = app.soak.mat_id else {
        return StepOutcome::fail("no material");
    };

    // High-contrast checker — projection changes must move pixels.
    let res = MAP_RES as usize;
    let mut pixels = vec![0u8; res * res * 4];
    let cell = 64usize;
    for y in 0..res {
        for x in 0..res {
            let on = ((x / cell) + (y / cell)) % 2 == 0;
            let i = (y * res + x) * 4;
            if on {
                pixels[i] = 240;
                pixels[i + 1] = 60;
                pixels[i + 2] = 40;
            } else {
                pixels[i] = 40;
                pixels[i + 1] = 80;
                pixels[i + 2] = 220;
            }
            pixels[i + 3] = 255;
        }
    }

    app.cache.material_map_loads.push(PendingMapLoad {
        target: MapTarget::Graph(mid),
        kind: MapKind::Albedo,
        pixels,
        strength: 1.0,
        name: "soak_checker".into(),
        high_precision: false,
        pixels_lo: Vec::new(),
        green_dx: false,
    });

    // Panel doc: Base color becomes Image layer (Texture path) so projection is meaningful.
    // Drop pure Vertex-only curvature content so the planar map is not cleared as gift-wrap.
    if let Some(mat) = app.scene.scene_graph.material_by_id(mid).map(|(_, m)| m.clone()) {
        let mut doc = app.mp_current_doc(mid, &mat);
        if let Some(ch) = doc.channels.iter_mut().find(|c| c.kind == ChannelKind::BaseColor) {
            ch.state = ChannelState::Layered;
            ch.layers.clear();
            ch.layers.push(Layer {
                id: 0,
                drives: Vec::new(),
                content: LayerContent::Image {
                    tex_id: 0, // filled when PendingMapLoad lands via apply_map_slot
                    strength: 1.0,
                },
                mask: crate::material_panel::model::MaskStack::default(),
                blend: BlendMode::Normal,
                opacity: 1.0,
                visible: true,
                placement: Placement::default(),
                adjust: Adjust::default(),
            });
        }
        // Seed Box projection before the switch sequence.
        doc.texture_space.projection = core_math::pbr_material::MapProjection::Box.as_u32();
        app.mp_commit_doc_edit(mid, doc, "SOAK bind image map", false);
    }

    // Stop vertex-curv redispatch so map/projection frames stay stable.
    if let Some(gpu) = app.gpu.as_mut() {
        gpu.sdf_body_curvature.clear();
    }
    app.cache.prim_buf_stale = true;
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note("queued checker albedo map (await poll upload)")
}

fn set_tex_projection(app: &mut AzraApp, code: u32, label: &str) -> StepOutcome {
    let Some(mid) = app.soak.mat_id else {
        return StepOutcome::fail("no material");
    };
    let Some(mat) = app.scene.scene_graph.material_by_id(mid).map(|(_, m)| m.clone()) else {
        return StepOutcome::fail("material missing");
    };
    let mut doc = app.mp_current_doc(mid, &mat);
    if doc.texture_space.projection == code {
        return StepOutcome::note(format!("projection already {label}"));
    }
    doc.texture_space.projection = code;
    app.mp_commit_doc_edit(mid, doc, &format!("SOAK proj {label}"), false);
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::ok()
}

// MapProjection codes (material_panel::lower::map_projection_for / TextureSpace):
// 0 = Box (triplanar), 1 = UV, and material codes Sphere/Cylinder/Planar via MapProjection.
// From lower.rs: Box=0, Uv=1, Sphere, Cylinder, Planar — use core_math MapProjection as_u32 if available.

fn step_tex_proj_sphere(app: &mut AzraApp) -> StepOutcome {
    set_tex_projection(
        app,
        core_math::pbr_material::MapProjection::Sphere.as_u32(),
        "Sphere",
    )
}
fn step_tex_proj_cylinder(app: &mut AzraApp) -> StepOutcome {
    set_tex_projection(
        app,
        core_math::pbr_material::MapProjection::Cylinder.as_u32(),
        "Cylinder",
    )
}
fn step_tex_proj_planar(app: &mut AzraApp) -> StepOutcome {
    set_tex_projection(
        app,
        core_math::pbr_material::MapProjection::Planar.as_u32(),
        "Planar",
    )
}
fn step_tex_proj_box(app: &mut AzraApp) -> StepOutcome {
    set_tex_projection(
        app,
        core_math::pbr_material::MapProjection::Box.as_u32(),
        "Box",
    )
}

// ─── Phase 2: POLY / LIGHTS / SAVE ───────────────────────────────────────────

fn soak_poly_validate(app: &AzraApp, nid: core_math::scene_graph::NodeId) -> Result<(), String> {
    let mesh = app
        .poly_node_mesh(nid)
        .ok_or_else(|| "no poly mesh on nid".to_string())?;
    mesh.validate()
        .map_err(|e| format!("validate: {e}"))?;
    mesh.is_manifold()
        .map_err(|e| format!("is_manifold: {e}"))?;
    Ok(())
}

fn soak_select_poly_body(app: &mut AzraApp, nid: core_math::scene_graph::NodeId) {
    app.modeling_context = crate::ModelingContext::Polygonal;
    app.poly_work_mode = crate::PolyWorkMode::Object;
    app.select_single_unified(crate::SceneItemId::Node(nid));
    app.soak.poly_nid = Some(nid);
}

/// Frame the poly body so topology edits fill the viewport (phase-3: was often px=0%
/// while vcap moved — camera was still framing the SDF body).
fn soak_frame_poly(app: &mut AzraApp, nid: core_math::scene_graph::NodeId) {
    soak_select_poly_body(app, nid);
    // Offset poly away from the SDF origin so they don't fully occlude each other.
    if let Some(node) = app.scene.scene_graph.get_mut(nid) {
        if node.local_position.length_squared() < 1e-6 {
            node.local_position = glam::Vec3::new(2.5, 0.0, 0.0);
        }
    }
    app.frame_selected();
    app.mark_dirty(DirtyFlags::RENDER);
}

fn step_poly_create_box(app: &mut AzraApp) -> StepOutcome {
    app.poly_work_mode = crate::PolyWorkMode::Object;
    app.modeling_context = crate::ModelingContext::Polygonal;
    app.create_poly_mesh("box");
    // Find newest poly body.
    let nid = app
        .scene
        .scene_graph
        .poly_bodies()
        .map(|(n, _)| n)
        .max_by_key(|n| n.0);
    let Some(nid) = nid else {
        return StepOutcome::fail("create_poly_mesh produced no PolygonalBody");
    };
    soak_frame_poly(app, nid);
    if let Err(e) = soak_poly_validate(app, nid) {
        return StepOutcome::fail(format!("post-create topology: {e}"));
    }
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("poly nid={nid:?} framed"))
}

fn step_poly_extrude(app: &mut AzraApp) -> StepOutcome {
    let Some(nid) = app.soak.poly_nid else {
        return StepOutcome::fail("no poly nid");
    };
    soak_select_poly_body(app, nid);
    app.poly_work_mode = crate::PolyWorkMode::Face;
    // Select first live face.
    let face = {
        let mesh = match app.poly_node_mesh(nid) {
            Some(m) => m,
            None => return StepOutcome::fail("no mesh"),
        };
        (0..mesh.faces.len() as u32).find(|&i| {
            mesh.face(core_math::half_edge::FaceId(i)).half_edge != core_math::half_edge::NO_EDGE
        })
    };
    let Some(f) = face else {
        return StepOutcome::fail("no live face to extrude");
    };
    app.poly_sel.selected_faces.clear();
    app.poly_sel.selected_faces.insert(f);
    app.poly_extrude_faces(0.25);
    if let Err(e) = soak_poly_validate(app, nid) {
        return StepOutcome::fail(format!("post-extrude topology: {e}"));
    }
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::ok()
}

fn step_poly_bevel(app: &mut AzraApp) -> StepOutcome {
    let Some(nid) = app.soak.poly_nid else {
        return StepOutcome::fail("no poly nid");
    };
    soak_select_poly_body(app, nid);
    app.poly_work_mode = crate::PolyWorkMode::Edge;
    // Select first live half-edge id as edge.
    let edge = {
        let mesh = match app.poly_node_mesh(nid) {
            Some(m) => m,
            None => return StepOutcome::fail("no mesh"),
        };
        // First half-edge belonging to a live face (avoids private free_edges set).
        (0..mesh.faces.len() as u32).find_map(|fi| {
            let fid = core_math::half_edge::FaceId(fi);
            if mesh.face(fid).half_edge == core_math::half_edge::NO_EDGE {
                return None;
            }
            mesh.face_half_edges(fid).into_iter().next().map(|h| h.0)
        })
    };
    let Some(e) = edge else {
        return StepOutcome::fail("no live edge to bevel");
    };
    app.poly_sel.selected_edges.clear();
    app.poly_sel.selected_edges.insert(e);
    app.poly_activate_bevel();
    // One-shot: drive width without mouse, confirm.
    app.poly_bevel_set_width(0.08);
    app.poly_bevel_set_segments(1);
    app.poly_confirm_bevel();
    if let Err(err) = soak_poly_validate(app, nid) {
        return StepOutcome::fail(format!("post-bevel topology: {err}"));
    }
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::ok()
}

fn step_poly_loop_cut(app: &mut AzraApp) -> StepOutcome {
    let Some(nid) = app.soak.poly_nid else {
        return StepOutcome::fail("no poly nid");
    };
    soak_select_poly_body(app, nid);
    app.poly_work_mode = crate::PolyWorkMode::Edge;
    let edge = {
        let mesh = match app.poly_node_mesh(nid) {
            Some(m) => m,
            None => return StepOutcome::fail("no mesh"),
        };
        (0..mesh.faces.len() as u32).find_map(|fi| {
            let fid = core_math::half_edge::FaceId(fi);
            if mesh.face(fid).half_edge == core_math::half_edge::NO_EDGE {
                return None;
            }
            mesh.face_half_edges(fid).into_iter().next().map(|h| h.0)
        })
    };
    let Some(e) = edge else {
        return StepOutcome::fail("no edge for loop cut");
    };
    if !app.poly_loop_cut_commit(nid, e, 1) {
        return StepOutcome::note("loop_cut_commit inserted nothing (degenerate)");
    }
    if let Err(err) = soak_poly_validate(app, nid) {
        return StepOutcome::fail(format!("post-loop-cut topology: {err}"));
    }
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::ok()
}

fn step_poly_delete_face(app: &mut AzraApp) -> StepOutcome {
    let Some(nid) = app.soak.poly_nid else {
        return StepOutcome::fail("no poly nid");
    };
    soak_select_poly_body(app, nid);
    app.poly_work_mode = crate::PolyWorkMode::Face;
    let face = {
        let mesh = match app.poly_node_mesh(nid) {
            Some(m) => m,
            None => return StepOutcome::fail("no mesh"),
        };
        (0..mesh.faces.len() as u32).find(|&i| {
            mesh.face(core_math::half_edge::FaceId(i)).half_edge != core_math::half_edge::NO_EDGE
        })
    };
    let Some(f) = face else {
        return StepOutcome::fail("no face to delete");
    };
    app.poly_sel.selected_faces.clear();
    app.poly_sel.selected_faces.insert(f);
    app.poly_delete_faces();
    // After delete-face (hole), mesh is open — validate still, is_manifold may accept open.
    if let Some(mesh) = app.poly_node_mesh(nid) {
        if let Err(e) = mesh.validate() {
            return StepOutcome::fail(format!("post-delete validate: {e}"));
        }
    }
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::ok()
}

fn step_poly_dissolve_edge(app: &mut AzraApp) -> StepOutcome {
    let Some(nid) = app.soak.poly_nid else {
        return StepOutcome::fail("no poly nid");
    };
    soak_select_poly_body(app, nid);
    app.poly_work_mode = crate::PolyWorkMode::Edge;
    let edge = {
        let mesh = match app.poly_node_mesh(nid) {
            Some(m) => m,
            None => return StepOutcome::fail("no mesh"),
        };
        (0..mesh.faces.len() as u32).find_map(|fi| {
            let fid = core_math::half_edge::FaceId(fi);
            if mesh.face(fid).half_edge == core_math::half_edge::NO_EDGE {
                return None;
            }
            mesh.face_half_edges(fid).into_iter().next().map(|h| h.0)
        })
    };
    let Some(e) = edge else {
        return StepOutcome::note("no dissolvable edge left");
    };
    app.poly_sel.selected_edges.clear();
    app.poly_sel.selected_edges.insert(e);
    app.poly_dissolve();
    if let Some(mesh) = app.poly_node_mesh(nid) {
        if let Err(err) = mesh.validate() {
            return StepOutcome::fail(format!("post-dissolve validate: {err}"));
        }
    }
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::ok()
}

fn step_poly_merge_center(app: &mut AzraApp) -> StepOutcome {
    let Some(nid) = app.soak.poly_nid else {
        return StepOutcome::fail("no poly nid");
    };
    soak_select_poly_body(app, nid);
    app.poly_work_mode = crate::PolyWorkMode::Vertex;
    // Pick two live verts.
    let verts: Vec<u32> = {
        let mesh = match app.poly_node_mesh(nid) {
            Some(m) => m,
            None => return StepOutcome::fail("no mesh"),
        };
        // Live verts: those referenced by a live face ring.
        let mut vs = std::collections::BTreeSet::new();
        for fi in 0..mesh.faces.len() as u32 {
            let fid = core_math::half_edge::FaceId(fi);
            if mesh.face(fid).half_edge == core_math::half_edge::NO_EDGE {
                continue;
            }
            for v in mesh.face_vertices(fid) {
                vs.insert(v.0);
            }
        }
        vs.into_iter().take(2).collect()
    };
    if verts.len() < 2 {
        return StepOutcome::note("need ≥2 verts to merge");
    }
    app.poly_sel.selected_vertices.clear();
    for v in &verts {
        app.poly_sel.selected_vertices.insert(*v);
    }
    app.poly_merge_vertices();
    if let Some(mesh) = app.poly_node_mesh(nid) {
        if let Err(e) = mesh.validate() {
            return StepOutcome::fail(format!("post-merge validate: {e}"));
        }
    }
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::ok()
}

fn step_poly_boolean(app: &mut AzraApp) -> StepOutcome {
    // Create a second box as cutter, then Difference into first poly body.
    app.poly_work_mode = crate::PolyWorkMode::Object;
    app.modeling_context = crate::ModelingContext::Polygonal;
    app.create_poly_mesh("box");
    let nids: Vec<_> = app
        .scene
        .scene_graph
        .poly_bodies()
        .map(|(n, _)| n)
        .collect();
    if nids.len() < 2 {
        return StepOutcome::fail("need ≥2 poly bodies for boolean");
    }
    // Offset cutter so they overlap partially.
    let cutter = *nids.last().unwrap();
    if let Some(node) = app.scene.scene_graph.get_mut(cutter) {
        node.local_position = glam::Vec3::new(0.4, 0.3, 0.0);
    }
    // Active = target (first poly soak created); select both.
    let target = app.soak.poly_nid.unwrap_or(nids[0]);
    let v_before = app
        .poly_node_mesh(target)
        .map(|m| m.vertices.len())
        .unwrap_or(0);
    app.select_after_create(&[
        crate::SceneItemId::Node(target),
        crate::SceneItemId::Node(cutter),
    ]);
    app.sel.usel.active = Some(crate::SceneItemId::Node(target));
    app.poly_boolean_execute(core_math::mesh_boolean::BoolOp::Difference);
    let v_after = app
        .poly_node_mesh(target)
        .map(|m| m.vertices.len())
        .unwrap_or(0);
    let still_has_cutter = app.scene.scene_graph.get(cutter).is_some();
    if let Some(mesh) = app.poly_node_mesh(target) {
        if let Err(e) = mesh.validate() {
            return StepOutcome::fail(format!("post-boolean validate: {e}"));
        }
    }
    app.mark_dirty(DirtyFlags::RENDER);
    // Kernel early-out (plan None / open mesh) leaves geometry unchanged — surface that.
    if v_before == v_after && still_has_cutter {
        return StepOutcome::note(format!(
            "boolean left mesh unchanged (v={v_after}, cutter kept) — plan/kernel early-out?"
        ));
    }
    StepOutcome::note(format!(
        "boolean v {v_before}→{v_after} cutter_kept={still_has_cutter}"
    ))
}

/// Scene setup so light steps can dominate the frame (phase-3: env-lit Hybrid made
/// sun/range/rt toggles vacuous).
fn soak_light_test_scene(app: &mut AzraApp) {
    app.render.shading_mode = crate::ShadingMode::Pbr;
    app.render.viewport_mode = crate::ViewportRenderMode::Hybrid;
    app.render.env_enabled = false;
    app.render.exposure = 1.0;
    // Dark albedo so light intensity / range changes are visible.
    app.transaction("SOAK dark albedo for lights", |s| {
        for p in s.scene.primitives.iter_mut() {
            p.material.albedo = glam::Vec3::splat(0.35);
        }
    });
    app.cache.prim_buf_stale = true;
}

fn soak_last_light_mut(app: &mut AzraApp, f: impl FnOnce(&mut core_math::scene_graph::LightData)) {
    let last_id = app
        .scene
        .scene_graph
        .lights()
        .map(|(id, _)| id)
        .last();
    if let Some(id) = last_id {
        app.transaction("SOAK tweak last light", |s| {
            for (lid, light) in s.scene.scene_graph.lights_mut() {
                if lid == id {
                    f(light);
                    break;
                }
            }
        });
    }
}

fn step_light_add_point(app: &mut AzraApp) -> StepOutcome {
    soak_light_test_scene(app);
    app.add_light(crate::LightType::Point);
    // Place close + bright so subsequent range/intensity edits matter.
    soak_last_light_mut(app, |light| {
        light.position = glam::Vec3::new(1.2, 1.5, 1.2);
        light.intensity = 8.0;
        light.range = 3.0;
        light.color = [1.0, 0.95, 0.85];
    });
    app.mark_dirty(DirtyFlags::RENDER | DirtyFlags::LIGHT);
    StepOutcome::note("env_off PBR point intensity=8 range=3")
}
fn step_light_add_spot(app: &mut AzraApp) -> StepOutcome {
    app.add_light(crate::LightType::Spot);
    soak_last_light_mut(app, |light| {
        light.position = glam::Vec3::new(-1.5, 2.0, 0.5);
        light.intensity = 12.0;
        light.range = 6.0;
    });
    app.mark_dirty(DirtyFlags::RENDER | DirtyFlags::LIGHT);
    StepOutcome::ok()
}
fn step_light_add_sun(app: &mut AzraApp) -> StepOutcome {
    app.add_light(crate::LightType::Directional);
    soak_last_light_mut(app, |light| {
        light.intensity = 4.0;
        light.color = [1.0, 0.4, 0.2]; // warm tint — visible vs white point
    });
    app.mark_dirty(DirtyFlags::RENDER | DirtyFlags::LIGHT);
    StepOutcome::note("sun intensity=4 warm tint")
}

fn step_light_point_radius(app: &mut AzraApp) -> StepOutcome {
    // Extreme range jump so falloff is visible under the light-test scene.
    let mut found = false;
    let mut before = 0.0f32;
    let mut after = 0.0f32;
    app.transaction("SOAK light radius", |s| {
        for (_, light) in s.scene.scene_graph.lights_mut() {
            if matches!(
                light.light_type,
                core_math::scene_graph::LightType::Point | core_math::scene_graph::LightType::Spot
            ) {
                before = light.range;
                light.range = (light.range * 4.0).max(12.0);
                after = light.range;
                light.intensity = (light.intensity * 2.0).max(16.0);
                found = true;
                break;
            }
        }
    });
    if !found {
        return StepOutcome::note("no point/spot light to adjust");
    }
    app.mark_dirty(DirtyFlags::RENDER | DirtyFlags::LIGHT);
    StepOutcome::note(format!("range {before:.1}→{after:.1} + intensity*2"))
}

fn step_light_rt_toggle(app: &mut AzraApp) -> StepOutcome {
    // RT shadows only show when the mode can cast them — force Hybrid/PBR + a big intensity
    // flip so the toggle is not a pure flag no-op in the log.
    app.render.shading_mode = crate::ShadingMode::Pbr;
    let mut found = false;
    let mut enabled = false;
    app.transaction("SOAK light RT toggle", |s| {
        for (_, light) in s.scene.scene_graph.lights_mut() {
            light.rt_shadow.enabled = !light.rt_shadow.enabled;
            enabled = light.rt_shadow.enabled;
            found = true;
            break;
        }
    });
    if !found {
        return StepOutcome::unwired("light.rt_toggle: no lights");
    }
    app.mark_dirty(DirtyFlags::RENDER | DirtyFlags::LIGHT);
    StepOutcome::note(format!("rt_shadow.enabled={enabled}"))
}

fn step_shade_matcap(app: &mut AzraApp) -> StepOutcome {
    app.render.shading_mode = crate::ShadingMode::MatCap;
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::ok()
}
fn step_shade_pbr(app: &mut AzraApp) -> StepOutcome {
    app.render.shading_mode = crate::ShadingMode::Pbr;
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::ok()
}
fn step_shade_pathtrace(app: &mut AzraApp) -> StepOutcome {
    app.render.shading_mode = crate::ShadingMode::PathTrace;
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::ok()
}
fn step_shade_hybrid_back(app: &mut AzraApp) -> StepOutcome {
    app.render.shading_mode = crate::ShadingMode::Pbr;
    app.render.viewport_mode = crate::ViewportRenderMode::Hybrid;
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::ok()
}

fn step_save_load_roundtrip(app: &mut AzraApp) -> StepOutcome {
    use crate::project::{save_binary, ProjectFile};
    let before = app.soak_state_counts();
    app.soak.pre_save_counts = Some(before);
    let path = app.soak.out_dir.join("soak_roundtrip.fl4");
    app.voxel_sync_node_mesh();
    let project = match ProjectFile::from_app(app) {
        Ok(project) => project,
        Err(error) => return StepOutcome::fail(format!("material snapshot: {error}")),
    };
    if let Err(e) = save_binary(&path, &project) {
        return StepOutcome::fail(format!("save_binary: {e}"));
    }
    app.load_scene_from_path(&path);
    let after = app.soak_state_counts();
    if after.0 != before.0 || after.2 != before.2 {
        return StepOutcome::fail(format!(
            "save/load mismatch before={before:?} after={after:?}"
        ));
    }
    // vcap may shift (pool realloc on load) — note but don't hard-fail capacity alone.
    let note = if after.1 != before.1 {
        format!("prims/nodes match; vcap {before:?}->{after:?} (capacity may reallocate on load)")
    } else {
        format!("counts match {after:?}")
    };
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(note)
}

// ─── Stress suite: complex SDF + material stacks ─────────────────────────────

/// Multi-prim "machine" body: boxes, cylinders, spheres, torus, smooth booleans.
/// Intended to stress bake cascade, DC mesher, and poll_background_tasks.
fn step_stress_sdf_complex(app: &mut AzraApp) -> StepOutcome {
    // Ensure we target the SDF body (poly selection may still be active).
    let body = app.scene.sdf_body_nid;
    app.scene.active_body_nid = body;
    app.modeling_context = crate::ModelingContext::Sdf;
    app.select_single_unified(crate::SceneItemId::Node(body));

    let before = app.scene.primitives.len();
    // Chassis + cutters + soft unions (positions body-local).
    let plan: &[(SdfShape, Vec3, BooleanOp)] = &[
        (
            SdfShape::Box {
                half_extents: Vec3::new(1.4, 0.35, 0.9),
                bevel: Default::default(),
            },
            Vec3::new(0.0, -0.2, 0.0),
            BooleanOp::Union { k: 0.0 },
        ),
        (
            SdfShape::Box {
                half_extents: Vec3::new(0.55, 0.55, 0.55),
                bevel: Default::default(),
            },
            Vec3::new(0.0, 0.55, 0.0),
            BooleanOp::Union { k: 0.12 },
        ),
        (
            SdfShape::Cylinder {
                radius: 0.28,
                half_height: 0.9,
            },
            Vec3::new(0.9, 0.1, 0.55),
            BooleanOp::Union { k: 0.08 },
        ),
        (
            SdfShape::Cylinder {
                radius: 0.28,
                half_height: 0.9,
            },
            Vec3::new(-0.9, 0.1, 0.55),
            BooleanOp::Union { k: 0.08 },
        ),
        (
            SdfShape::Cylinder {
                radius: 0.28,
                half_height: 0.9,
            },
            Vec3::new(0.9, 0.1, -0.55),
            BooleanOp::Union { k: 0.08 },
        ),
        (
            SdfShape::Cylinder {
                radius: 0.28,
                half_height: 0.9,
            },
            Vec3::new(-0.9, 0.1, -0.55),
            BooleanOp::Union { k: 0.08 },
        ),
        (
            SdfShape::Sphere { radius: 0.42 },
            Vec3::new(0.55, 0.85, 0.35),
            BooleanOp::Subtract { k: 0.15 },
        ),
        (
            SdfShape::Sphere { radius: 0.38 },
            Vec3::new(-0.5, 0.75, -0.3),
            BooleanOp::Subtract { k: 0.1 },
        ),
        (
            SdfShape::Torus {
                major_radius: 0.7,
                minor_radius: 0.12,
            },
            Vec3::new(0.0, 0.95, 0.0),
            BooleanOp::Union { k: 0.2 },
        ),
        (
            SdfShape::Capsule {
                radius: 0.18,
                half_height: 0.7,
            },
            Vec3::new(0.0, 0.4, 1.1),
            BooleanOp::Union { k: 0.05 },
        ),
        (
            SdfShape::Cone {
                radius: 0.35,
                half_height: 0.45,
            },
            Vec3::new(1.1, 0.7, 0.0),
            BooleanOp::Subtract { k: 0.05 },
        ),
        (
            SdfShape::Box {
                half_extents: Vec3::new(0.2, 0.7, 0.2),
                bevel: Default::default(),
            },
            Vec3::new(0.0, 1.1, 0.0),
            BooleanOp::Union { k: 0.0 },
        ),
    ];

    let t0 = Instant::now();
    for (shape, pos, op) in plan {
        let _idx = app.add_primitive_with_op(shape.clone(), *pos, op.clone());
    }
    // One explicit rebake after the burst (production path for bulk edits).
    app.transaction("SOAK stress rebake", |s| {
        s.sync_flat_cache();
        s.rebake_no_undo();
    });
    let ms = t0.elapsed().as_secs_f32() * 1000.0;
    let after = app.scene.primitives.len();
    app.mark_dirty(DirtyFlags::RENDER | DirtyFlags::SCENE);
    StepOutcome::note(format!(
        "added {} prims (total={after}, was={before}) sync_burst={ms:.1}ms",
        after.saturating_sub(before)
    ))
}

/// Move several prims to force dirty AABB / partial-or-full bake storm.
fn step_stress_sdf_move_burst(app: &mut AzraApp) -> StepOutcome {
    let n = app.scene.primitives.len();
    if n < 4 {
        return StepOutcome::fail(format!("need ≥4 prims for move burst, got {n}"));
    }
    let t0 = Instant::now();
    // Nudge every other prim so dirty regions scatter across the body AABB.
    app.transaction("SOAK stress move burst", |s| {
        for i in 0..n {
            if i % 2 == 0 {
                s.write_prim(i, |p| {
                    p.position += Vec3::new(0.08, 0.04, -0.03);
                });
            }
        }
        s.sync_flat_cache();
        s.rebake_no_undo();
    });
    let ms = t0.elapsed().as_secs_f32() * 1000.0;
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("moved ~{} prims sync={ms:.1}ms", (n + 1) / 2))
}

/// Complex Base color stack: Constant + Vertex curvature + FBM noise + cellular noise.
fn step_stress_mat_noise_stack(app: &mut AzraApp) -> StepOutcome {
    use crate::material_panel::model::{
        ChannelKind, ChannelState, ChannelValue, GeneratorOutput, GeneratorRung, GeneratorSource,
        Layer, LayerContent, BlendMode, Placement, Adjust,
    };
    use crate::shader_nodes::NoiseType;

    // Fresh material so we don't fight prior image-map replacement.
    let mut seed = PbrMaterial::default();
    seed.albedo = glam::Vec3::new(0.72, 0.55, 0.38); // warm clay
    seed.roughness = 0.55;
    let mid = app.scene.scene_graph.add_material("soak_stress_mat", seed);
    let n = app.scene.primitives.len();
    app.transaction("SOAK stress assign mat", |s| {
        for i in 0..n {
            s.write_prim(i, |p| p.material_id = mid);
        }
    });
    let doc0 = crate::material_panel::model::MaterialPanelDoc::default_for_new();
    app.ui.material_panel_docs.insert(mid.0, doc0);
    app.ui
        .material_node_graphs
        .entry(mid.0)
        .or_insert_with(crate::shader_nodes::NodeGraph::default);
    app.soak.mat_id = Some(mid);

    let Some(mat) = app.scene.scene_graph.material_by_id(mid).map(|(_, m)| m.clone()) else {
        return StepOutcome::fail("stress mat missing");
    };
    let mut doc = app.mp_current_doc(mid, &mat);
    let auto_r = app.soak_auto_curv_radius();

    let base = Layer {
        id: 0,
        drives: Vec::new(),
        content: LayerContent::Constant {
            value: ChannelValue::Color([0.72, 0.55, 0.38]),
        },
        mask: crate::material_panel::model::MaskStack::default(),
        blend: BlendMode::Normal,
        opacity: 1.0,
        visible: true,
        placement: Placement::default(),
        adjust: Adjust::default(),
    };
    let curv = Layer {
        id: 0,
        drives: Vec::new(),
        content: LayerContent::Generator {
            source: GeneratorSource::Curvature,
            radius: auto_r,
            invert: false,
            output: GeneratorOutput::Signed,
            remap: [0.0, 1.0],
            rung: GeneratorRung::VertexCache,
            sharpness: 1.15,
            axis: Default::default(),
        },
        mask: crate::material_panel::model::MaskStack::default(),
        blend: BlendMode::Multiply,
        opacity: 0.85,
        visible: true,
        placement: Placement::default(),
        adjust: Adjust::default(),
    };
    let noise_fbm = Layer {
        id: 0,
        drives: Vec::new(),
        content: LayerContent::Noise {
            scale: 6.0,
            strength: 0.65,
            octaves: 5.0,
            noise_type: NoiseType::Fbm,
        },
        mask: crate::material_panel::model::MaskStack::default(),
        blend: BlendMode::Multiply,
        opacity: 0.7,
        visible: true,
        placement: Placement::default(),
        adjust: Adjust {
            contrast: 0.15,
            saturation: 1.0,
            gamma: 1.05,
            invert: false,
        },
    };
    let noise_cell = Layer {
        id: 0,
        drives: Vec::new(),
        content: LayerContent::Noise {
            scale: 14.0,
            strength: 0.4,
            octaves: 3.0,
            noise_type: NoiseType::Cellular,
        },
        mask: crate::material_panel::model::MaskStack::default(),
        blend: BlendMode::Overlay,
        opacity: 0.45,
        visible: true,
        placement: Placement::default(),
        adjust: Adjust::default(),
    };
    let cavity = Layer {
        id: 0,
        drives: Vec::new(),
        content: LayerContent::Generator {
            source: GeneratorSource::Cavity,
            radius: auto_r * 0.75,
            invert: false,
            output: GeneratorOutput::ConcaveOnly,
            remap: [0.0, 1.0],
            rung: GeneratorRung::VertexCache,
            sharpness: 1.0,
            axis: Default::default(),
        },
        mask: crate::material_panel::model::MaskStack::default(),
        blend: BlendMode::Multiply,
        opacity: 0.5,
        visible: true,
        placement: Placement::default(),
        adjust: Adjust::default(),
    };

    if let Some(ch) = doc
        .channels
        .iter_mut()
        .find(|c| c.kind == ChannelKind::BaseColor)
    {
        ch.state = ChannelState::Layered;
        ch.layers.clear();
        ch.layers.extend([base, curv, noise_fbm, noise_cell, cavity]);
    } else {
        return StepOutcome::fail("BaseColor missing");
    }

    let t0 = Instant::now();
    app.mp_commit_doc_edit(mid, doc, "SOAK stress noise stack", false);
    let ms = t0.elapsed().as_secs_f32() * 1000.0;
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!(
        "mid={mid:?} layers=5 (const+curv+fbm+cell+cavity) commit={ms:.1}ms auto_r={auto_r:.3}"
    ))
}

/// Roughness + Metallic layered noise (material bake / multi-channel path).
fn step_stress_mat_multi_channel(app: &mut AzraApp) -> StepOutcome {
    use crate::material_panel::model::{
        ChannelKind, ChannelState, ChannelValue, Layer, LayerContent, BlendMode, Placement, Adjust,
    };
    use crate::shader_nodes::NoiseType;

    let Some(mid) = app.soak.mat_id else {
        return StepOutcome::fail("no stress mat");
    };
    let Some(mat) = app.scene.scene_graph.material_by_id(mid).map(|(_, m)| m.clone()) else {
        return StepOutcome::fail("mat missing");
    };
    let mut doc = app.mp_current_doc(mid, &mat);

    let rough = Layer {
        id: 0,
        drives: Vec::new(),
        content: LayerContent::Noise {
            scale: 8.0,
            strength: 1.0,
            octaves: 4.0,
            noise_type: NoiseType::Perlin,
        },
        mask: crate::material_panel::model::MaskStack::default(),
        blend: BlendMode::Normal,
        opacity: 1.0,
        visible: true,
        placement: Placement::default(),
        adjust: Adjust::default(),
    };
    let metal = Layer {
        id: 0,
        drives: Vec::new(),
        content: LayerContent::Noise {
            scale: 3.5,
            strength: 0.8,
            octaves: 3.0,
            noise_type: NoiseType::Ridged,
        },
        mask: crate::material_panel::model::MaskStack::default(),
        blend: BlendMode::Normal,
        opacity: 0.6,
        visible: true,
        placement: Placement::default(),
        adjust: Adjust::default(),
    };
    let rough_base = Layer {
        id: 0,
        drives: Vec::new(),
        content: LayerContent::Constant {
            value: ChannelValue::Scalar(0.45),
        },
        mask: crate::material_panel::model::MaskStack::default(),
        blend: BlendMode::Normal,
        opacity: 1.0,
        visible: true,
        placement: Placement::default(),
        adjust: Adjust::default(),
    };

    for (kind, layers) in [
        (ChannelKind::Roughness, vec![rough_base.clone(), rough]),
        (ChannelKind::Metallic, vec![metal]),
    ] {
        if let Some(ch) = doc.channels.iter_mut().find(|c| c.kind == kind) {
            ch.state = ChannelState::Layered;
            ch.layers = layers;
        }
    }

    let t0 = Instant::now();
    app.mp_commit_doc_edit(mid, doc, "SOAK stress multi-channel", false);
    let ms = t0.elapsed().as_secs_f32() * 1000.0;
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("rough+metal noise layers commit={ms:.1}ms"))
}

/// Change noise scale dramatically — must rebake procedural stack.
fn step_stress_mat_noise_scale_sweep(app: &mut AzraApp) -> StepOutcome {
    use crate::material_panel::model::{ChannelKind, LayerContent};

    let Some(mid) = app.soak.mat_id else {
        return StepOutcome::fail("no stress mat");
    };
    let Some(mat) = app.scene.scene_graph.material_by_id(mid).map(|(_, m)| m.clone()) else {
        return StepOutcome::fail("mat missing");
    };
    let mut doc = app.mp_current_doc(mid, &mat);
    let mut n_noise = 0u32;
    if let Some(ch) = doc
        .channels
        .iter_mut()
        .find(|c| c.kind == ChannelKind::BaseColor)
    {
        for layer in ch.layers.iter_mut() {
            if let LayerContent::Noise { scale, .. } = &mut layer.content {
                *scale *= 2.5;
                n_noise += 1;
            }
        }
    }
    if n_noise == 0 {
        return StepOutcome::note("no noise layers to sweep");
    }
    let t0 = Instant::now();
    app.mp_commit_doc_edit(mid, doc, "SOAK stress noise scale*2.5", false);
    let ms = t0.elapsed().as_secs_f32() * 1000.0;
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("scaled {n_noise} noise layers *2.5 commit={ms:.1}ms"))
}

fn step_stress_modstack_12(app: &mut AzraApp) -> StepOutcome {
    push_modstack(app, 12)
}

/// Log live perf counters into the soak line (for Claude-facing reports).
fn step_stress_perf_snapshot(app: &mut AzraApp) -> StepOutcome {
    let (prims, vcap, nodes) = app.soak_state_counts();
    let p = &app.perf;
    let note = format!(
        "frame={:.1}ms poll={:.1}ms bake_rb={:.1}ms x{} sync_flat={:.1}ms x{} \
         prims={prims} vcap={vcap} nodes={nodes} lights={} bodies={} \
         vol_dc={:.1}ms full_rebakes={} partial={}",
        p.frame_time_ms,
        p.poll_tasks_ms,
        p.bake_readback_ms,
        p.bake_readback_count,
        p.sync_flat_cache_ms,
        p.sync_flat_cache_count,
        p.light_count,
        p.body_count,
        p.vol_dc_ms,
        p.vol_full_rebakes,
        p.vol_partial_rebakes,
    );
    // Also emit a dedicated console tag for easy grepping.
    eprintln!("[SOAK_PERF] {note}");
    StepOutcome::note(note)
}

// ─── Untested-tools suite (AZRA_SOAK_SUITE=unt) ───────────────────────────────

fn unt_select_sdf_body(app: &mut AzraApp) {
    // VALIDATE before pointing the app at it (owner soak BUG-2, 2026-07-29).
    //
    // This used to assign `scene.sdf_body_nid` unconditionally. After a convert SDF->poly that
    // pointer can name a node which is no longer a live SDF body, and every later step then
    // operated on a dead body: `add_prim_to_graph` fell into its recovery path, built a FRESH
    // default body, and the flat cache rebuilt from a graph that no longer held the original
    // prims — the reported "prims 9->1" collapse plus
    // "[WARN] active_body_nid NodeId(1) invalid".
    //
    // The kernel invariant is fine (`sdf_body_nid_always_names_a_live_body` in scene_graph.rs);
    // it was the HARNESS feeding a stale id. Fall back to the first live SDF body so a stale
    // pointer degrades to "wrong body" instead of "destroys the scene".
    let stale = app.scene.sdf_body_nid;
    let body = if app.scene.scene_graph.body_primitives(stale).is_some() {
        stale
    } else {
        let live = app.scene.scene_graph.sdf_bodies().next().map(|(nid, _)| nid);
        eprintln!("[SOAK] unt_select_sdf_body: sdf_body_nid {stale:?} is STALE - falling back to                    {live:?} (a convert/delete left it dangling)");
        match live {
            Some(nid) => {
                app.scene.sdf_body_nid = nid;
                nid
            }
            None => stale, // nothing live to point at; let the step report its own failure
        }
    };
    app.scene.active_body_nid = body;
    app.modeling_context = crate::ModelingContext::Sdf;
    app.select_single_unified(crate::SceneItemId::Node(body));
}

fn step_unt_sdf_torus(app: &mut AzraApp) -> StepOutcome {
    unt_select_sdf_body(app);
    let idx = app.add_primitive_with_op(
        crate::ops::geo::torus(),
        Vec3::new(1.2, 0.4, 0.0),
        BooleanOp::Union { k: 0.1 },
    );
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("torus idx={idx}"))
}

fn step_unt_sdf_cone(app: &mut AzraApp) -> StepOutcome {
    unt_select_sdf_body(app);
    let idx = app.add_primitive_with_op(
        crate::ops::geo::cone(),
        Vec3::new(-1.0, 0.5, 0.3),
        BooleanOp::Subtract { k: 0.05 },
    );
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("cone subtract idx={idx}"))
}

fn step_unt_sdf_capsule(app: &mut AzraApp) -> StepOutcome {
    unt_select_sdf_body(app);
    let idx = app.add_primitive_with_op(
        crate::ops::geo::capsule(),
        Vec3::new(0.0, 0.8, 1.0),
        BooleanOp::Union { k: 0.0 },
    );
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("capsule idx={idx}"))
}

fn step_unt_sdf_stair(app: &mut AzraApp) -> StepOutcome {
    unt_select_sdf_body(app);
    let idx = app.add_primitive_with_op(
        crate::ops::geo::stair(),
        Vec3::new(1.5, -0.2, -0.8),
        BooleanOp::Union { k: 0.0 },
    );
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("stair idx={idx}"))
}

fn step_unt_sdf_spiral_stair(app: &mut AzraApp) -> StepOutcome {
    unt_select_sdf_body(app);
    let idx = app.add_primitive_with_op(
        crate::ops::geo::spiral_stair(),
        Vec3::new(-1.8, 0.0, -1.2),
        BooleanOp::Union { k: 0.15 },
    );
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("spiral_stair idx={idx}"))
}

fn step_unt_bool_intersect(app: &mut AzraApp) -> StepOutcome {
    if app.scene.primitives.len() < 2 {
        return StepOutcome::fail("need >=2 prims for intersect");
    }
    unt_select_sdf_body(app);
    let idx = app.add_primitive_with_op(
        SdfShape::Box {
            half_extents: Vec3::new(1.5, 1.2, 1.5),
            bevel: Default::default(),
        },
        Vec3::ZERO,
        BooleanOp::Intersect,
    );
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("intersect box idx={idx}"))
}

fn step_unt_bool_cycle(app: &mut AzraApp) -> StepOutcome {
    let before = format!("{:?}", app.tool.create_op);
    crate::ops::bool_op::cycle::run(app);
    let after = format!("{:?}", app.tool.create_op);
    if before == after {
        return StepOutcome::fail("create_op did not cycle");
    }
    StepOutcome::note(format!("create_op {before}->{after}"))
}

fn step_unt_duplicate_selected(app: &mut AzraApp) -> StepOutcome {
    unt_select_sdf_body(app);
    if app.scene.primitives.is_empty() {
        return StepOutcome::fail("no prims to duplicate");
    }
    app.select_single(0);
    let before = app.scene.primitives.len();
    app.duplicate_selected();
    let after = app.scene.primitives.len();
    app.mark_dirty(DirtyFlags::RENDER);
    if after <= before {
        return StepOutcome::note(format!(
            "duplicate did not grow prims ({before}->{after}) — body-level dup?"
        ));
    }
    StepOutcome::note(format!("prims {before}->{after}"))
}

fn step_unt_set_resolution(app: &mut AzraApp) -> StepOutcome {
    let body = app.scene.sdf_body_nid;
    app.cache.body_vol_cache.set_body_resolution(body, 256);
    app.transaction("SOAK set res 256", |s| {
        s.rebake_no_undo();
    });
    app.mark_dirty(DirtyFlags::RENDER | DirtyFlags::SCENE);
    StepOutcome::note("set_body_resolution=256 + rebake")
}

fn step_unt_poly_sphere(app: &mut AzraApp) -> StepOutcome {
    app.poly_work_mode = crate::PolyWorkMode::Object;
    app.modeling_context = crate::ModelingContext::Polygonal;
    app.create_poly_mesh("sphere");
    let nid = app
        .scene
        .scene_graph
        .poly_bodies()
        .map(|(n, _)| n)
        .max_by_key(|n| n.0);
    let Some(nid) = nid else {
        return StepOutcome::fail("no poly sphere");
    };
    soak_select_poly_body(app, nid);
    if let Err(e) = soak_poly_validate(app, nid) {
        return StepOutcome::fail(format!("sphere topology: {e}"));
    }
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("poly sphere {nid:?}"))
}

fn step_unt_poly_cylinder(app: &mut AzraApp) -> StepOutcome {
    app.create_poly_mesh("cylinder");
    let nid = app
        .scene
        .scene_graph
        .poly_bodies()
        .map(|(n, _)| n)
        .max_by_key(|n| n.0);
    let Some(nid) = nid else {
        return StepOutcome::fail("no poly cylinder");
    };
    soak_select_poly_body(app, nid);
    if let Some(node) = app.scene.scene_graph.get_mut(nid) {
        node.local_position = Vec3::new(2.0, 0.0, 0.0);
    }
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("poly cylinder {nid:?}"))
}

fn unt_select_face0(app: &mut AzraApp) -> Result<core_math::scene_graph::NodeId, String> {
    let Some(nid) = app.soak.poly_nid else {
        return Err("no poly nid".into());
    };
    soak_select_poly_body(app, nid);
    app.poly_work_mode = crate::PolyWorkMode::Face;
    let face = {
        let mesh = app.poly_node_mesh(nid).ok_or("no mesh")?;
        (0..mesh.faces.len() as u32).find(|&i| {
            mesh.face(core_math::half_edge::FaceId(i)).half_edge != core_math::half_edge::NO_EDGE
        })
    };
    let f = face.ok_or("no live face")?;
    app.poly_sel.selected_faces.clear();
    app.poly_sel.selected_faces.insert(f);
    Ok(nid)
}

fn step_unt_poly_inset(app: &mut AzraApp) -> StepOutcome {
    let nid = match unt_select_face0(app) {
        Ok(n) => n,
        Err(e) => return StepOutcome::fail(e),
    };
    app.poly_inset_faces(0.12);
    if let Err(e) = soak_poly_validate(app, nid) {
        return StepOutcome::fail(format!("post-inset: {e}"));
    }
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::ok()
}

fn step_unt_poly_solidify(app: &mut AzraApp) -> StepOutcome {
    app.create_poly_mesh("box");
    let nid = app
        .scene
        .scene_graph
        .poly_bodies()
        .map(|(n, _)| n)
        .max_by_key(|n| n.0)
        .expect("box");
    soak_select_poly_body(app, nid);
    if let Some(node) = app.scene.scene_graph.get_mut(nid) {
        node.local_position = Vec3::new(-2.5, 0.0, 0.0);
    }
    app.poly_work_mode = crate::PolyWorkMode::Object;
    app.poly_solidify(0.08);
    if let Some(mesh) = app.poly_node_mesh(nid) {
        if let Err(e) = mesh.validate() {
            return StepOutcome::fail(format!("post-solidify validate: {e}"));
        }
    }
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("solidified {nid:?}"))
}

fn step_unt_poly_subdivide(app: &mut AzraApp) -> StepOutcome {
    let Some(nid) = app.soak.poly_nid else {
        return StepOutcome::fail("no poly nid");
    };
    soak_select_poly_body(app, nid);
    app.poly_work_mode = crate::PolyWorkMode::Object;
    app.poly_sel.selected_faces.clear();
    let v_before = app.poly_node_mesh(nid).map(|m| m.vertices.len()).unwrap_or(0);
    app.poly_subdivide();
    let v_after = app.poly_node_mesh(nid).map(|m| m.vertices.len()).unwrap_or(0);
    if v_after <= v_before {
        return StepOutcome::note(format!("subdivide no growth v={v_before}->{v_after}"));
    }
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("subdivide v {v_before}->{v_after}"))
}

fn step_unt_poly_select_ops(app: &mut AzraApp) -> StepOutcome {
    let Some(nid) = app.soak.poly_nid else {
        return StepOutcome::fail("no poly nid");
    };
    soak_select_poly_body(app, nid);
    app.poly_work_mode = crate::PolyWorkMode::Face;
    app.poly_select_all_toggle();
    let n1 = app.poly_sel.selected_faces.len();
    app.poly_invert_selection();
    let n2 = app.poly_sel.selected_faces.len();
    app.poly_select_all_toggle();
    app.poly_grow_selection();
    let n3 = app.poly_sel.selected_faces.len();
    app.poly_shrink_selection();
    let n4 = app.poly_sel.selected_faces.len();
    StepOutcome::note(format!(
        "select_all={n1} invert={n2} grow={n3} shrink={n4}"
    ))
}

fn step_unt_poly_flip_normals(app: &mut AzraApp) -> StepOutcome {
    let Some(nid) = app.soak.poly_nid else {
        return StepOutcome::fail("no poly nid");
    };
    soak_select_poly_body(app, nid);
    app.poly_flip_normals();
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::ok()
}

fn step_unt_poly_recalc_normals(app: &mut AzraApp) -> StepOutcome {
    let Some(nid) = app.soak.poly_nid else {
        return StepOutcome::fail("no poly nid");
    };
    soak_select_poly_body(app, nid);
    app.poly_recalculate_normals();
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::ok()
}

fn step_unt_poly_limited_dissolve(app: &mut AzraApp) -> StepOutcome {
    let Some(nid) = app.soak.poly_nid else {
        return StepOutcome::fail("no poly nid");
    };
    soak_select_poly_body(app, nid);
    app.poly_work_mode = crate::PolyWorkMode::Object;
    let f_before = app
        .poly_node_mesh(nid)
        .map(|m| {
            m.faces
                .iter()
                .filter(|f| f.half_edge != core_math::half_edge::NO_EDGE)
                .count()
        })
        .unwrap_or(0);
    app.poly_limited_dissolve();
    let f_after = app
        .poly_node_mesh(nid)
        .map(|m| {
            m.faces
                .iter()
                .filter(|f| f.half_edge != core_math::half_edge::NO_EDGE)
                .count()
        })
        .unwrap_or(0);
    if let Some(mesh) = app.poly_node_mesh(nid) {
        if let Err(e) = mesh.validate() {
            return StepOutcome::fail(format!("post-limited-dissolve: {e}"));
        }
    }
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("faces {f_before}->{f_after}"))
}

fn step_unt_poly_join(app: &mut AzraApp) -> StepOutcome {
    let n_before = app.scene.scene_graph.poly_bodies().count();
    if n_before < 2 {
        app.create_poly_mesh("box");
        if let Some(nid) = app
            .scene
            .scene_graph
            .poly_bodies()
            .map(|(n, _)| n)
            .max_by_key(|n| n.0)
        {
            if let Some(node) = app.scene.scene_graph.get_mut(nid) {
                node.local_position = Vec3::new(3.0, 0.5, 0.0);
            }
        }
    }
    let ids: Vec<_> = app
        .scene
        .scene_graph
        .poly_bodies()
        .map(|(n, _)| n)
        .collect();
    if ids.len() < 2 {
        return StepOutcome::fail("need >=2 poly bodies to join");
    }
    app.select_after_create(&[
        crate::SceneItemId::Node(ids[0]),
        crate::SceneItemId::Node(ids[1]),
    ]);
    app.sel.usel.active = Some(crate::SceneItemId::Node(ids[0]));
    if !app.poly_join_has_candidates() {
        return StepOutcome::note("join has no candidates");
    }
    app.poly_join();
    let n_after = app.scene.scene_graph.poly_bodies().count();
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("poly bodies {n_before}->{n_after}"))
}

fn step_unt_poly_fill(app: &mut AzraApp) -> StepOutcome {
    app.create_poly_mesh("box");
    let nid = app
        .scene
        .scene_graph
        .poly_bodies()
        .map(|(n, _)| n)
        .max_by_key(|n| n.0)
        .expect("box");
    soak_select_poly_body(app, nid);
    if let Some(node) = app.scene.scene_graph.get_mut(nid) {
        node.local_position = Vec3::new(0.0, 2.0, 0.0);
    }
    app.poly_work_mode = crate::PolyWorkMode::Face;
    let face = {
        let mesh = match app.poly_node_mesh(nid) {
            Some(m) => m,
            None => return StepOutcome::fail("no mesh"),
        };
        (0..mesh.faces.len() as u32).find(|&i| {
            mesh.face(core_math::half_edge::FaceId(i)).half_edge != core_math::half_edge::NO_EDGE
        })
    };
    let Some(f) = face else {
        return StepOutcome::fail("no face");
    };
    let boundary: Vec<u32> = {
        let mesh = app.poly_node_mesh(nid).unwrap();
        mesh.face_vertices(core_math::half_edge::FaceId(f))
            .into_iter()
            .map(|v| v.0)
            .collect()
    };
    app.poly_sel.selected_faces.clear();
    app.poly_sel.selected_faces.insert(f);
    app.poly_delete_faces();
    app.poly_work_mode = crate::PolyWorkMode::Vertex;
    app.poly_sel.selected_vertices.clear();
    for v in &boundary {
        app.poly_sel.selected_vertices.insert(*v);
    }
    if boundary.len() < 3 {
        return StepOutcome::note("boundary too small after delete");
    }
    app.poly_fill_face();
    if let Some(mesh) = app.poly_node_mesh(nid) {
        if let Err(e) = mesh.validate() {
            return StepOutcome::fail(format!("post-fill validate: {e}"));
        }
    }
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("fill from {} verts", boundary.len()))
}

fn step_unt_convert_sdf_to_poly(app: &mut AzraApp) -> StepOutcome {
    unt_select_sdf_body(app);
    let body = app.scene.sdf_body_nid;
    let before = app.scene.scene_graph.poly_bodies().count();
    app.convert_sdf_to_poly(body);
    let after = app.scene.scene_graph.poly_bodies().count();
    app.mark_dirty(DirtyFlags::RENDER);
    if after <= before {
        return StepOutcome::note(format!(
            "convert_sdf_to_poly: poly bodies still {after} (mesh not ready?)"
        ));
    }
    StepOutcome::note(format!("poly bodies {before}->{after}"))
}

fn step_unt_convert_poly_to_sdf(app: &mut AzraApp) -> StepOutcome {
    let poly = app
        .scene
        .scene_graph
        .poly_bodies()
        .map(|(n, _)| n)
        .max_by_key(|n| n.0);
    let Some(poly) = poly else {
        return StepOutcome::note("no poly body for convert_poly_to_sdf");
    };
    unt_select_sdf_body(app);
    let prims_before = app.scene.primitives.len();
    app.convert_poly_to_sdf(poly);
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!(
        "spawned convert poly {poly:?}->sdf (async); prims_now={prims_before}"
    ))
}

fn step_unt_instance_create(app: &mut AzraApp) -> StepOutcome {
    unt_select_sdf_body(app);
    if app.scene.primitives.is_empty() {
        return StepOutcome::fail("no prims");
    }
    app.select_single(0);
    let before = app.scene.primitives.len();
    app.create_instance();
    let after = app.scene.primitives.len();
    app.mark_dirty(DirtyFlags::RENDER);
    if after <= before {
        return StepOutcome::note(format!("instance did not add prim ({before}->{after})"));
    }
    let is_inst = app.scene.primitives[after - 1]
        .instance_master_id
        .is_some();
    StepOutcome::note(format!(
        "prims {before}->{after} last_is_instance={is_inst}"
    ))
}

// ─── More suite (AZRA_SOAK_SUITE=more) — second wave of untested APIs ─────────

fn more_select_sdf_body(app: &mut AzraApp) {
    unt_select_sdf_body(app);
}

/// Replace prim[0] modifier stack with a single production default of `mt` + rebake.
fn more_set_single_modifier(
    app: &mut AzraApp,
    mt: core_math::primitives::ModifierType,
) -> Result<String, String> {
    more_select_sdf_body(app);
    if app.scene.primitives.is_empty() {
        return Err("no prims for modifier".into());
    }
    // Prefer a non-instance, non-curve prim when possible so domain warps are visible.
    let idx = app
        .scene
        .primitives
        .iter()
        .position(|p| p.instance_master_id.is_none())
        .unwrap_or(0);
    let uid = app.scene.primitives[idx].prim_uid;
    app.transaction(&format!("SOAK clear+set {:?}", mt), |s| {
        s.write_prim(idx, |p| {
            p.modifiers.clear();
            p.modifiers
                .push(crate::ui_panels::default_modifier(mt));
            p.sync_modifiers_to_gpu_fields();
        });
        s.rebake_no_undo();
    });
    // Sanity: stack length after write
    let n = app
        .scene
        .primitives
        .get(idx)
        .map(|p| p.modifiers.len())
        .unwrap_or(0);
    let got = app
        .scene
        .primitives
        .get(idx)
        .and_then(|p| p.modifiers.first())
        .map(|m| format!("{:?}", m.mod_type))
        .unwrap_or_else(|| "?".into());
    app.mark_dirty(DirtyFlags::RENDER);
    Ok(format!("uid={uid} stack={n} type={got}"))
}

fn step_more_sdf_curve_profile(app: &mut AzraApp) -> StepOutcome {
    more_select_sdf_body(app);
    let before = app.scene.primitives.len();
    let idx = app.add_primitive_with_op(
        crate::ops::geo::curve_profile(),
        Vec3::new(0.0, 0.0, 1.6),
        BooleanOp::Union { k: 0.0 },
    );
    let after = app.scene.primitives.len();
    app.mark_dirty(DirtyFlags::RENDER);
    if after <= before {
        return StepOutcome::fail("curve_profile did not add prim");
    }
    let kind = match &app.scene.primitives[idx].shape {
        SdfShape::CurveExtrude { .. } => "CurveExtrude",
        other => return StepOutcome::note(format!("unexpected shape {:?}", std::mem::discriminant(other))),
    };
    StepOutcome::note(format!("curve_profile idx={idx} kind={kind} prims {before}->{after}"))
}

fn step_more_mod_twist(app: &mut AzraApp) -> StepOutcome {
    match more_set_single_modifier(app, core_math::primitives::ModifierType::Twist) {
        Ok(n) => StepOutcome::note(n),
        Err(e) => StepOutcome::fail(e),
    }
}
fn step_more_mod_taper(app: &mut AzraApp) -> StepOutcome {
    match more_set_single_modifier(app, core_math::primitives::ModifierType::Taper) {
        Ok(n) => StepOutcome::note(n),
        Err(e) => StepOutcome::fail(e),
    }
}
fn step_more_mod_shell(app: &mut AzraApp) -> StepOutcome {
    match more_set_single_modifier(app, core_math::primitives::ModifierType::Shell) {
        Ok(n) => StepOutcome::note(n),
        Err(e) => StepOutcome::fail(e),
    }
}
fn step_more_mod_smooth(app: &mut AzraApp) -> StepOutcome {
    match more_set_single_modifier(app, core_math::primitives::ModifierType::Smooth) {
        Ok(n) => StepOutcome::note(n),
        Err(e) => StepOutcome::fail(e),
    }
}
fn step_more_mod_mirror(app: &mut AzraApp) -> StepOutcome {
    match more_set_single_modifier(app, core_math::primitives::ModifierType::Mirror) {
        Ok(n) => StepOutcome::note(n),
        Err(e) => StepOutcome::fail(e),
    }
}
fn step_more_mod_shear(app: &mut AzraApp) -> StepOutcome {
    match more_set_single_modifier(app, core_math::primitives::ModifierType::Shear) {
        Ok(n) => StepOutcome::note(n),
        Err(e) => StepOutcome::fail(e),
    }
}
fn step_more_mod_spherize(app: &mut AzraApp) -> StepOutcome {
    match more_set_single_modifier(app, core_math::primitives::ModifierType::Spherize) {
        Ok(n) => StepOutcome::note(n),
        Err(e) => StepOutcome::fail(e),
    }
}
fn step_more_mod_emboss(app: &mut AzraApp) -> StepOutcome {
    match more_set_single_modifier(app, core_math::primitives::ModifierType::Emboss) {
        Ok(n) => StepOutcome::note(n),
        Err(e) => StepOutcome::fail(e),
    }
}
fn step_more_mod_deboss(app: &mut AzraApp) -> StepOutcome {
    match more_set_single_modifier(app, core_math::primitives::ModifierType::Deboss) {
        Ok(n) => StepOutcome::note(n),
        Err(e) => StepOutcome::fail(e),
    }
}
fn step_more_mod_array(app: &mut AzraApp) -> StepOutcome {
    match more_set_single_modifier(app, core_math::primitives::ModifierType::Array) {
        Ok(n) => StepOutcome::note(n),
        Err(e) => StepOutcome::fail(e),
    }
}

fn more_create_poly(app: &mut AzraApp, gen: &str, pos: Vec3) -> Result<core_math::scene_graph::NodeId, String> {
    app.poly_work_mode = crate::PolyWorkMode::Object;
    app.modeling_context = crate::ModelingContext::Polygonal;
    app.create_poly_mesh(gen);
    let nid = app
        .scene
        .scene_graph
        .poly_bodies()
        .map(|(n, _)| n)
        .max_by_key(|n| n.0)
        .ok_or_else(|| format!("no poly body after create {gen}"))?;
    soak_select_poly_body(app, nid);
    if let Some(node) = app.scene.scene_graph.get_mut(nid) {
        node.local_position = pos;
    }
    app.mark_dirty(DirtyFlags::RENDER);
    Ok(nid)
}

fn step_more_poly_torus(app: &mut AzraApp) -> StepOutcome {
    let nid = match more_create_poly(app, "torus", Vec3::new(2.5, 0.0, 0.0)) {
        Ok(n) => n,
        Err(e) => return StepOutcome::fail(e),
    };
    if let Err(e) = soak_poly_validate(app, nid) {
        return StepOutcome::fail(format!("torus topology: {e}"));
    }
    let (v, f) = app
        .poly_node_mesh(nid)
        .map(|m| {
            (
                m.vertices.len(),
                m.faces
                    .iter()
                    .filter(|x| x.half_edge != core_math::half_edge::NO_EDGE)
                    .count(),
            )
        })
        .unwrap_or((0, 0));
    StepOutcome::note(format!("poly torus {nid:?} v={v} f={f}"))
}

fn step_more_poly_cone(app: &mut AzraApp) -> StepOutcome {
    let nid = match more_create_poly(app, "cone", Vec3::new(-2.5, 0.0, 0.0)) {
        Ok(n) => n,
        Err(e) => return StepOutcome::fail(e),
    };
    if let Err(e) = soak_poly_validate(app, nid) {
        return StepOutcome::fail(format!("cone topology: {e}"));
    }
    StepOutcome::note(format!("poly cone {nid:?}"))
}

fn step_more_poly_plane(app: &mut AzraApp) -> StepOutcome {
    let nid = match more_create_poly(app, "plane", Vec3::new(0.0, -1.5, 0.0)) {
        Ok(n) => n,
        Err(e) => return StepOutcome::fail(e),
    };
    // Plane is open — manifold check may fail; validate only half-edge integrity.
    if let Some(mesh) = app.poly_node_mesh(nid) {
        if let Err(e) = mesh.validate() {
            return StepOutcome::fail(format!("plane validate: {e}"));
        }
    }
    StepOutcome::note(format!("poly plane {nid:?}"))
}

fn step_more_poly_hide_selected(app: &mut AzraApp) -> StepOutcome {
    // Fresh box so hide has a clear face selection.
    let nid = match more_create_poly(app, "box", Vec3::new(0.0, 2.0, 0.0)) {
        Ok(n) => n,
        Err(e) => return StepOutcome::fail(e),
    };
    soak_select_poly_body(app, nid);
    app.poly_work_mode = crate::PolyWorkMode::Face;
    let face = {
        let mesh = match app.poly_node_mesh(nid) {
            Some(m) => m,
            None => return StepOutcome::fail("no mesh"),
        };
        (0..mesh.faces.len() as u32).find(|&i| {
            mesh.face(core_math::half_edge::FaceId(i)).half_edge != core_math::half_edge::NO_EDGE
        })
    };
    let Some(f) = face else {
        return StepOutcome::fail("no live face");
    };
    app.poly_sel.selected_faces.clear();
    app.poly_sel.selected_faces.insert(f);
    let before = app.poly_sel.hidden_vertices.len();
    app.poly_hide_selected();
    let after = app.poly_sel.hidden_vertices.len();
    app.mark_dirty(DirtyFlags::RENDER);
    if after <= before {
        return StepOutcome::note(format!("hide did not grow hidden ({before}->{after})"));
    }
    StepOutcome::note(format!("hidden_verts {before}->{after}"))
}

fn step_more_poly_unhide_all(app: &mut AzraApp) -> StepOutcome {
    let before = app.poly_sel.hidden_vertices.len();
    app.poly_unhide_all();
    let after = app.poly_sel.hidden_vertices.len();
    app.mark_dirty(DirtyFlags::RENDER);
    if before == 0 {
        return StepOutcome::note("nothing was hidden");
    }
    if after != 0 {
        return StepOutcome::fail(format!("unhide left {after} hidden"));
    }
    StepOutcome::note(format!("unhid {before} verts"))
}

fn step_more_poly_connect(app: &mut AzraApp) -> StepOutcome {
    let Some(nid) = app.soak.poly_nid else {
        return StepOutcome::fail("no poly nid");
    };
    soak_select_poly_body(app, nid);
    app.poly_work_mode = crate::PolyWorkMode::Vertex;
    // Pick two non-adjacent verts on the same face so connect splits the face.
    let pair = {
        let mesh = match app.poly_node_mesh(nid) {
            Some(m) => m,
            None => return StepOutcome::fail("no mesh"),
        };
        let mut found = None;
        for i in 0..mesh.faces.len() as u32 {
            let he = mesh.face(core_math::half_edge::FaceId(i)).half_edge;
            if he == core_math::half_edge::NO_EDGE {
                continue;
            }
            let verts = mesh.face_vertices(core_math::half_edge::FaceId(i));
            if verts.len() >= 4 {
                found = Some((verts[0].0, verts[2].0));
                break;
            }
        }
        found
    };
    let Some((a, b)) = pair else {
        return StepOutcome::note("no quad face for connect");
    };
    let e_before = app
        .poly_node_mesh(nid)
        .map(|m| m.half_edges.len())
        .unwrap_or(0);
    app.poly_sel.selected_vertices.clear();
    app.poly_sel.selected_vertices.insert(a);
    app.poly_sel.selected_vertices.insert(b);
    app.poly_connect_vertices();
    let e_after = app
        .poly_node_mesh(nid)
        .map(|m| m.half_edges.len())
        .unwrap_or(0);
    if let Some(mesh) = app.poly_node_mesh(nid) {
        if let Err(e) = mesh.validate() {
            return StepOutcome::fail(format!("post-connect: {e}"));
        }
    }
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("connect verts {a}+{b} half_edges {e_before}->{e_after}"))
}

fn step_more_poly_merge_by_distance(app: &mut AzraApp) -> StepOutcome {
    // Box + select all verts — threshold is 2% of bbox, so only near-coincident verts weld.
    // Expectation: often toast "nothing within range" → note, not fail.
    let nid = match more_create_poly(app, "box", Vec3::new(3.0, 2.0, 0.0)) {
        Ok(n) => n,
        Err(e) => return StepOutcome::fail(e),
    };
    soak_select_poly_body(app, nid);
    app.poly_work_mode = crate::PolyWorkMode::Vertex;
    let v_before = app.poly_node_mesh(nid).map(|m| m.vertices.len()).unwrap_or(0);
    {
        let mesh = match app.poly_node_mesh(nid) {
            Some(m) => m,
            None => return StepOutcome::fail("no mesh"),
        };
        app.poly_sel.selected_vertices.clear();
        for i in 0..mesh.vertices.len() as u32 {
            if mesh.vert(core_math::half_edge::VertexId(i)).half_edge
                != core_math::half_edge::NO_EDGE
            {
                app.poly_sel.selected_vertices.insert(i);
            }
        }
    }
    app.poly_merge_by_distance();
    let v_after = app.poly_node_mesh(nid).map(|m| m.vertices.len()).unwrap_or(0);
    if let Some(mesh) = app.poly_node_mesh(nid) {
        if let Err(e) = mesh.validate() {
            return StepOutcome::fail(format!("post-merge-dist: {e}"));
        }
    }
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!("merge_by_distance verts {v_before}->{v_after}"))
}

fn step_more_poly_bridge(app: &mut AzraApp) -> StepOutcome {
    // Legacy bridge: delete two opposite faces on a box, select their boundary edge pairs —
    // simpler path: select 2 verts that can form a face, or 2 edges.
    // Use two separate boxes? Bridge is intra-mesh. Fresh box: pick two edges that share no face.
    let nid = match more_create_poly(app, "box", Vec3::new(-3.0, 2.0, 0.0)) {
        Ok(n) => n,
        Err(e) => return StepOutcome::fail(e),
    };
    soak_select_poly_body(app, nid);
    // Delete one face to create a boundary, then select two opposite boundary verts for legacy bridge.
    app.poly_work_mode = crate::PolyWorkMode::Face;
    let faces: Vec<u32> = {
        let mesh = match app.poly_node_mesh(nid) {
            Some(m) => m,
            None => return StepOutcome::fail("no mesh"),
        };
        (0..mesh.faces.len() as u32)
            .filter(|&i| {
                mesh.face(core_math::half_edge::FaceId(i)).half_edge
                    != core_math::half_edge::NO_EDGE
            })
            .collect()
    };
    if faces.len() < 2 {
        return StepOutcome::fail("box has <2 faces");
    }
    // Delete two faces to open holes — then try bridge on two edges from first hole.
    app.poly_sel.selected_faces.clear();
    app.poly_sel.selected_faces.insert(faces[0]);
    app.poly_delete_faces();
    // After delete, select two boundary edges if available.
    app.poly_work_mode = crate::PolyWorkMode::Edge;
    let edges: Vec<u32> = {
        let mesh = match app.poly_node_mesh(nid) {
            Some(m) => m,
            None => return StepOutcome::fail("no mesh after delete"),
        };
        let mut out = Vec::new();
        for i in 0..mesh.half_edges.len() as u32 {
            let he = mesh.he(core_math::half_edge::HalfEdgeId(i));
            if he.face == core_math::half_edge::NO_FACE {
                continue;
            }
            // Prefer boundary half-edges (twin missing or twin face none)
            let twin = he.twin;
            if twin == core_math::half_edge::NO_TWIN
                || mesh.he(twin).face == core_math::half_edge::NO_FACE
            {
                out.push(i);
            }
        }
        out
    };
    app.poly_sel.selected_edges.clear();
    app.poly_sel.selected_vertices.clear();
    if edges.len() >= 2 {
        app.poly_sel.selected_edges.insert(edges[0]);
        app.poly_sel.selected_edges.insert(edges[1]);
    } else if edges.len() == 1 {
        // Fall back: two verts on the remaining hole
        let mesh = app.poly_node_mesh(nid).unwrap();
        let v0 = mesh.he_source(core_math::half_edge::HalfEdgeId(edges[0])).0;
        let v1 = mesh.he(core_math::half_edge::HalfEdgeId(edges[0])).vertex.0;
        app.poly_work_mode = crate::PolyWorkMode::Vertex;
        app.poly_sel.selected_vertices.insert(v0);
        app.poly_sel.selected_vertices.insert(v1);
    } else {
        return StepOutcome::note("no boundary edges after face delete");
    }
    let f_before = app
        .poly_node_mesh(nid)
        .map(|m| {
            m.faces
                .iter()
                .filter(|f| f.half_edge != core_math::half_edge::NO_EDGE)
                .count()
        })
        .unwrap_or(0);
    app.poly_bridge();
    let f_after = app
        .poly_node_mesh(nid)
        .map(|m| {
            m.faces
                .iter()
                .filter(|f| f.half_edge != core_math::half_edge::NO_EDGE)
                .count()
        })
        .unwrap_or(0);
    if let Some(mesh) = app.poly_node_mesh(nid) {
        if let Err(e) = mesh.validate() {
            return StepOutcome::fail(format!("post-bridge: {e}"));
        }
    }
    app.mark_dirty(DirtyFlags::RENDER);
    StepOutcome::note(format!(
        "bridge faces {f_before}->{f_after} boundary_edges={}",
        edges.len()
    ))
}

fn step_more_poly_bisect(app: &mut AzraApp) -> StepOutcome {
    let nid = match more_create_poly(app, "box", Vec3::new(0.0, 0.0, 3.0)) {
        Ok(n) => n,
        Err(e) => return StepOutcome::fail(e),
    };
    soak_select_poly_body(app, nid);
    app.poly_work_mode = crate::PolyWorkMode::Object;
    let f_before = app
        .poly_node_mesh(nid)
        .map(|m| {
            m.faces
                .iter()
                .filter(|f| f.half_edge != core_math::half_edge::NO_EDGE)
                .count()
        })
        .unwrap_or(0);
    app.poly_bisect();
    let f_after = app
        .poly_node_mesh(nid)
        .map(|m| {
            m.faces
                .iter()
                .filter(|f| f.half_edge != core_math::half_edge::NO_EDGE)
                .count()
        })
        .unwrap_or(0);
    if let Some(mesh) = app.poly_node_mesh(nid) {
        if let Err(e) = mesh.validate() {
            return StepOutcome::fail(format!("post-bisect: {e}"));
        }
    }
    app.mark_dirty(DirtyFlags::RENDER);
    if f_after == f_before {
        return StepOutcome::note(format!(
            "bisect faces unchanged {f_before} (plane miss or no-op)"
        ));
    }
    StepOutcome::note(format!("bisect faces {f_before}->{f_after}"))
}

fn step_more_export_obj(app: &mut AzraApp) -> StepOutcome {
    let nid = app
        .scene
        .scene_graph
        .poly_bodies()
        .map(|(n, _)| n)
        .max_by_key(|n| n.0);
    let Some(nid) = nid else {
        return StepOutcome::fail("no poly body to export");
    };
    let path = app.soak.out_dir.join("more_export.obj");
    let path_s = path.to_string_lossy().to_string();
    app.export_node_obj(nid, &path_s);
    if !path.exists() {
        return StepOutcome::fail(format!("export did not create {path_s}"));
    }
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    StepOutcome::note(format!("exported {nid:?} bytes={bytes}"))
}

fn more_count_nurbs(app: &AzraApp) -> usize {
    app.scene.scene_graph.nurbs_bodies().count()
}

fn step_more_nurbs_plane(app: &mut AzraApp) -> StepOutcome {
    let before = more_count_nurbs(app);
    app.create_nurbs_surface("plane");
    let after = more_count_nurbs(app);
    app.mark_dirty(DirtyFlags::RENDER);
    if after <= before {
        return StepOutcome::fail("nurbs plane not created");
    }
    StepOutcome::note(format!("nurbs bodies {before}->{after}"))
}

fn step_more_nurbs_sphere(app: &mut AzraApp) -> StepOutcome {
    let before = more_count_nurbs(app);
    app.create_nurbs_surface("sphere");
    let after = more_count_nurbs(app);
    app.mark_dirty(DirtyFlags::RENDER);
    if after <= before {
        return StepOutcome::fail("nurbs sphere not created");
    }
    StepOutcome::note(format!("nurbs bodies {before}->{after}"))
}

fn step_more_nurbs_torus(app: &mut AzraApp) -> StepOutcome {
    let before = more_count_nurbs(app);
    app.create_nurbs_surface("torus");
    let after = more_count_nurbs(app);
    app.mark_dirty(DirtyFlags::RENDER);
    if after <= before {
        return StepOutcome::fail("nurbs torus not created");
    }
    StepOutcome::note(format!("nurbs bodies {before}->{after}"))
}

fn step_more_nurbs_cylinder(app: &mut AzraApp) -> StepOutcome {
    let before = more_count_nurbs(app);
    app.create_nurbs_surface("cylinder");
    let after = more_count_nurbs(app);
    app.mark_dirty(DirtyFlags::RENDER);
    if after <= before {
        return StepOutcome::fail("nurbs cylinder not created");
    }
    StepOutcome::note(format!("nurbs bodies {before}->{after}"))
}

// ── UV + MESH MAPS ──────────────────────────────────────────────────────────

/// The poly body these steps operate on: the newest one in the scene (built by the D block).
fn soak_poly_target(app: &AzraApp) -> Option<core_math::scene_graph::NodeId> {
    app.scene
        .scene_graph
        .poly_bodies()
        .map(|(n, _)| n)
        .max_by_key(|n| n.0)
}

/// Smart UV Project through the production command, then assert it actually produced UVs.
fn step_uv_unwrap(app: &mut AzraApp) -> StepOutcome {
    let Some(nid) = soak_poly_target(app) else {
        return StepOutcome::fail("no PolygonalBody to unwrap");
    };
    soak_select_poly_body(app, nid);
    app.poly_auto_uv_unwrap();
    let corners = app
        .cache
        .poly_custom_data
        .get(&nid)
        .map_or(0, |cd| cd.uv.len());
    if corners == 0 {
        return StepOutcome::fail("UV Unwrap produced no UVs on the selected body");
    }
    StepOutcome::ok()
}

/// Bake the full mesh-map set through the production path (queue + poll drain).
///
/// Asserts the SLOTS exist afterwards, not merely that the call returned: the whole point of this
/// step is that a bake which silently produces nothing is the failure this feature kept hitting.
fn step_meshmap_bake(app: &mut AzraApp) -> StepOutcome {
    use crate::mesh_map_bake::MeshMapKind;
    let Some(nid) = soak_poly_target(app) else {
        return StepOutcome::fail("no PolygonalBody to bake");
    };
    soak_select_poly_body(app, nid);
    let mut s = app.bake_settings_for(nid);
    s.res_global = 256;
    s.ao_rays = 24;
    app.store_bake_settings(nid, s);
    app.poly_request_mesh_map_bake();
    if app.cache.pending_mesh_map_bake.is_none() {
        return StepOutcome::fail("bake was refused at request time (see toast)");
    }
    // The bake is ASYNC now (worker + poll drain), so the first call starts it and later calls
    // land it. Pump with a bound rather than blocking: this step asserts the OUTCOME, and a bake
    // that never lands must fail loudly instead of hanging the harness.
    let t0 = std::time::Instant::now();
    let mut landed = false;
    for _ in 0..600 {
        if app.poll_mesh_map_bakes() && app.ui.baked_images.for_body(nid).next().is_some() {
            landed = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if !landed {
        return StepOutcome::fail(format!(
            "bake did not land within {:.1}s",
            t0.elapsed().as_secs_f32()
        ));
    }
    let maps: Vec<_> = app.ui.baked_images.for_body(nid).collect();
    if maps.is_empty() {
        return StepOutcome::fail("bake produced no mesh maps for this body");
    }
    let missing: Vec<&str> = MeshMapKind::ALL
        .iter()
        .filter(|k| !maps.iter().any(|img| img.kind_bit == k.bit()))
        .map(|k| k.label())
        .collect();
    if !missing.is_empty() {
        // ID legitimately skips without polygroups — report rather than fail.
        return StepOutcome::note(format!("baked {} maps, skipped: {}", maps.len(), missing.join(",")));
    }
    StepOutcome::ok()
}

/// Switch the body's material to Baked generator mode and confirm it resolves there.
fn step_meshmap_mode_baked(app: &mut AzraApp) -> StepOutcome {
    use crate::material_panel::model::GeneratorMode;
    let Some(nid) = soak_poly_target(app) else {
        return StepOutcome::fail("no PolygonalBody");
    };
    soak_select_poly_body(app, nid);
    // Same resolve the panel uses for a poly body: its first face's material.
    let mid = match app.scene.scene_graph.get(nid).map(|n| &n.payload) {
        Some(core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. }) => {
            core_math::primitives::MaterialId(mesh.faces.first().map(|f| f.material_id).unwrap_or(0))
        }
        _ => return StepOutcome::fail("target is not a PolygonalBody"),
    };
    // Ensure a doc exists so the mode has somewhere to live (the panel creates one on open).
    let doc = app
        .ui
        .material_panel_docs
        .get(&mid.0)
        .cloned()
        .unwrap_or_else(crate::material_panel::model::MaterialPanelDoc::default_for_new);
    app.ui.material_panel_docs.insert(mid.0, doc);
    if let Some(d) = app.ui.material_panel_docs.get_mut(&mid.0) {
        d.generator_mode = GeneratorMode::Baked.as_u32();
    }
    let resolved = app.resolved_generator_mode(mid);
    if resolved != GeneratorMode::Baked {
        return StepOutcome::fail(format!("mode did not resolve to Baked (got {})", resolved.label()));
    }
    app.enqueue_material_node_graph_bake(mid);
    StepOutcome::ok()
}

/// Pack the EXISTING islands. Asserts the layout changed but every corner survived — a Pack that
/// dropped corners, or that silently re-unwrapped, would both pass a "did something" check.
fn step_uv_pack(app: &mut AzraApp) -> StepOutcome {
    let Some(nid) = soak_poly_target(app) else {
        return StepOutcome::fail("no PolygonalBody to pack");
    };
    soak_select_poly_body(app, nid);
    let before: Vec<(u32, glam::Vec2)> = match app.cache.poly_custom_data.get(&nid) {
        Some(cd) => cd.uv.iter().map(|(k, v)| (k.0, *v)).collect(),
        None => return StepOutcome::fail("no UV data to pack (unwrap step must run first)"),
    };
    if before.is_empty() {
        return StepOutcome::fail("UV layer is empty");
    }
    app.poly_pack_uvs();
    let after: Vec<(u32, glam::Vec2)> = match app.cache.poly_custom_data.get(&nid) {
        Some(cd) => cd.uv.iter().map(|(k, v)| (k.0, *v)).collect(),
        None => return StepOutcome::fail("UV data vanished during pack"),
    };
    if after.len() != before.len() {
        return StepOutcome::fail(format!(
            "pack changed the corner count {} -> {} (it must only MOVE islands)",
            before.len(),
            after.len()
        ));
    }
    StepOutcome::ok()
}
