//! Primitive and mesh creation — shared actions called from pie menu,
//! outliner "+", keyboard shortcuts, and agent bar.

use crate::*;

impl AzraApp {
    // ---- SDF Primitives --------------------------------------------------------
    // Shape data lives in `ops::geo::shape_presets` (pure, node-ready). These
    // methods are thin App façades so existing call sites keep compiling.

    pub(crate) const SHAPE_PRESET_COUNT: usize = crate::ops::geo::SHAPE_PRESET_COUNT;

    /// Default shape presets — delegates to pure geo kernel.
    #[inline]
    pub(crate) fn shape_preset(idx: usize) -> SdfShape {
        crate::ops::geo::by_index(idx)
    }
    /// Tiny version of each shape for creation start (nearly invisible, grows during footprint drag).
    #[inline]
    pub(crate) fn shape_preset_tiny(idx: usize) -> SdfShape {
        crate::ops::geo::by_index_tiny(idx)
    }
    /// Default polyline / curve-extrude preset — pure geo kernel.
    /// Kept OUT of indexed `shape_preset()` (footprint tool cycles by radius/half-extent fields).
    #[inline]
    pub(crate) fn curve_preset() -> SdfShape {
        crate::ops::geo::curve_profile()
    }

    /// Default 3D spine / pipe (Phase A): short S-shaped tube in local space.
    pub(crate) fn spine_preset() -> SdfShape {
        core_math::primitives::spine_pipe_preset()
    }
    pub(crate) fn next_name(&mut self, shape:&str)->String { let n=format!("{}.{:03}",shape,self.scene.next_prim_id); self.scene.next_prim_id+=1; n }

    /// Material a FRESHLY-CREATED prim should carry so it visually blends into the body it
    /// lands in (owner ask 2026-07-18: "a newly created primitive inside a body should receive
    /// the material from the body" — before this, every new prim arrived with the default
    /// material and visibly broke a textured body). Reads the TARGET body (`add_prim_to_graph`
    /// inserts into `active_body_nid`) via the graph-level `body_prevailing_material` (= LAST
    /// prim's `material_id`; empty body → NONE = the old behavior). Call at fresh-construction
    /// sites ONLY — paste / duplicate / instance / agent prims carry their own authored id and
    /// must NOT be rewritten (which is why this is not inside `add_prim_to_graph` itself).
    pub(crate) fn inherited_material_for_create(&self) -> core_math::pbr_material::MaterialId {
        self.scene.scene_graph.body_prevailing_material(self.scene.active_body_nid)
    }
    pub(crate) fn add_primitive_immediate(&mut self, shape:SdfShape, pos:Vec3) -> usize {
        let op = self.tool.create_op.to_boolean_op(self.tool.create_blend_k);
        self.add_primitive_with_op(shape, pos, op)
    }
    /// Add a primitive with an EXPLICIT boolean op (rather than the current create op).
    /// Used by the slice tool, which must force `Subtract` regardless of the create-op toggle.
    pub(crate) fn add_primitive_with_op(&mut self, shape:SdfShape, pos:Vec3, op:BooleanOp) -> usize {
        // If a body node is selected, target that body for the new prim
        if let Some(nid) = self.sel.usel.active_node() {
            if self.scene.scene_graph.get(nid)
                .map(|n| matches!(&n.payload, core_math::scene_graph::NodePayload::SdfBody { .. }))
                .unwrap_or(false)
            {
                self.scene.active_body_nid = nid;
            }
        }
        let uid = self.scene.next_prim_id;
        let label=shape.label().to_string(); let name=self.next_name(&label); let desc=format!("Add {}",name);
        // Determine placement position (and optional rotation) for the new prim.
        //
        // With BODY_LOCAL_PRIMS=true, prim.position must be body-local (relative to
        // body.origin). Use BodyTransform::world_to_local for the conversion so that
        // future body_rotation/body_scale support works without changes here.
        // With BODY_LOCAL_PRIMS=false (legacy), position is world-space.
        let (actual_pos, wp_rot) = if Self::BODY_LOCAL_PRIMS {
            // Get the active body's world transform.
            let bt = self.scene.scene_graph.body_world_transform(self.scene.active_body_nid)
                .unwrap_or_default();

            if self.viewport.wp_mode == WorkPlaneMode::Locked {
                let wp = &self.viewport.wp_resolved;
                let rot = Self::euler_from_workplane_normal(wp.normal);
                // wp.origin + pos is a world-space position; convert to body-local.
                let world_pos = wp.origin + pos;
                (bt.world_to_local(world_pos), rot)
            } else {
                // pos is a world-space offset passed by the caller (surface hit or Vec3::ZERO).
                // Reconstruct the intended world position relative to the body origin,
                // then convert back to body-local.
                let world_pos = bt.origin + pos;
                (bt.world_to_local(world_pos), Vec3::ZERO)
                // For identity rotation/scale: simplifies to (pos, Vec3::ZERO).
            }
        } else {
            // Legacy world-space path.
            if self.viewport.wp_mode == WorkPlaneMode::Locked {
                let wp = &self.viewport.wp_resolved;
                let rot = Self::euler_from_workplane_normal(wp.normal);
                (wp.origin + pos, rot)
            } else {
                let body_origin = self.scene.scene_graph.get(self.scene.active_body_nid).and_then(|n| {
                    if let core_math::scene_graph::NodePayload::SdfBody { origin, .. } = &n.payload {
                        if *origin != Vec3::ZERO { Some(*origin) } else { None }
                    } else { None }
                });
                (body_origin.unwrap_or(Vec3::ZERO) + pos, Vec3::ZERO)
            }
        };
        let is_spine = matches!(&shape, SdfShape::SpinePipe { .. });
        // Creating a non-spine while Spine tool is open → exit spine tool first.
        if !is_spine && self.tool.current.is_spine_point_edit() {
            self.spine_exit_tool();
        }
        // Terrain AABB clamp lives on the shared create path by design: every Shift+A /
        // create stamp must honour the lattice. Cheap no-op unless `is_terrain_tile_body`
        // (NodeId registry / origin+BaseHill). Drag path uses the same helper via
        // `write_prim_transform_ex` (placement concern — one clamp API, two call sites).
        let actual_pos =
            self.clamp_prim_pos_for_terrain_body(self.scene.active_body_nid, actual_pos);
        let mut prim = SdfPrimitive::with_uid(name,shape,actual_pos,uid);
        prim.rotation_deg = wp_rot;
        prim.operation = op;
        // Inherit the target body's prevailing material so the new prim blends in visually
        // (must be read AFTER the active-body retarget above — that's the body prim lands in).
        prim.material_id = self.inherited_material_for_create();
        let mut idx = 0usize;
        self.transaction(&desc, |s| {
            idx = s.add_prim_to_graph(prim);
            s.select_single(idx);
            // Spine authoring: point-edit first (seed sphere). Do NOT arm object gizmo —
            // spine_enter_edit (below, outside the transaction) owns the full tool setup.
            if is_spine {
                s.set_gizmo_armed(false);
            } else {
                // Owner: new cube/etc. leaves you in Object mode with only that prim
                // selected and gizmo armed — ready for G/R/S (including when created
                // from Vertex edit mode: exit edit, select the new prim).
                if s.modeling_context == ModelingContext::Sdf
                    && s.tool.work_mode != WorkMode::Object
                {
                    s.switch_work_mode(WorkMode::Object);
                }
                s.tool.gizmo_armed = true;
            }
            s.rebake_no_undo();
        });
        if is_spine {
            // Shared authoring setup (also used by the CREATE-tool click path):
            // Vertex mode, select prim, seed point selected, spine_pt_owner,
            // ActiveTool::SpinePointEdit + hint toast.
            self.spine_enter_edit(idx);
        }
        idx
    }
    /// Convert a workplane normal to Euler rotation (degrees, YXZ convention).
    /// Default prim up-axis is Y — rotate so that prim Y aligns with the workplane normal.
    /// Note: free twist around the normal is not fixed; prefer [`Self::euler_from_workplane_frame`]
    /// when U/V axes matter (Draw Polyline).
    pub(crate) fn euler_from_workplane_normal(normal: Vec3) -> Vec3 {
        let up = Vec3::Y;
        if normal.abs_diff_eq(up, 1e-4) {
            return Vec3::ZERO; // Already aligned — no rotation needed
        }
        if normal.abs_diff_eq(-up, 1e-4) {
            return Vec3::new(180.0, 0.0, 0.0); // Flip upside down
        }
        let q = Quat::from_rotation_arc(up, normal.normalize());
        // Extract YXZ Euler angles matching SdfPrimitive::rotation_quat()
        let (y, x, z) = q.to_euler(EulerRot::YXZ);
        Vec3::new(
            x.to_degrees(),
            y.to_degrees(),
            z.to_degrees(),
        )
    }

    /// Full workplane frame → Euler (YXZ degrees) for a prim whose local axes should match:
    /// **local X = u**, **local Y = normal** (extrude), **local Z = v**.
    /// Used by Draw Polyline so CurveExtrude (profile in XZ, extrude along Y) sits on the plane.
    pub(crate) fn euler_from_workplane_frame(u: Vec3, normal: Vec3, v: Vec3) -> Vec3 {
        let xu = u.normalize_or_zero();
        let yn = normal.normalize_or_zero();
        let mut zv = v.normalize_or_zero();
        // Enforce right-handed orthonormal basis (handle slight non-ortho plane frames).
        if xu.length_squared() < 1e-8 || yn.length_squared() < 1e-8 {
            return Self::euler_from_workplane_normal(normal);
        }
        // Rebuild Z from X × Y if V is degenerate or flipped.
        let z_from_xy = xu.cross(yn).normalize_or_zero();
        if zv.length_squared() < 1e-8 || zv.dot(z_from_xy) < 0.0 {
            zv = z_from_xy;
        } else {
            zv = z_from_xy; // always prefer consistent RH frame
        }
        let x_from_yz = yn.cross(zv).normalize_or_zero();
        let xu = if x_from_yz.length_squared() > 1e-8 { x_from_yz } else { xu };
        let mat = glam::Mat3::from_cols(xu, yn, zv);
        let q = Quat::from_mat3(&mat);
        let (y, x, z) = q.to_euler(EulerRot::YXZ);
        Vec3::new(x.to_degrees(), y.to_degrees(), z.to_degrees())
    }

    /// Inject primitives from the AI agent into the scene
    pub(crate) fn inject_agent_primitives(&mut self, prims: Vec<SdfPrimitive>) {
        if prims.is_empty() { return; }
        let count = prims.len();
        self.transaction("Agent generation", |s| {
            // Fix names to avoid collisions and add to graph
            let mut indices = Vec::with_capacity(count);
            for mut p in prims {
                p.prim_uid = s.scene.next_prim_id;
                p.name = s.next_name(&p.name);
                indices.push(s.add_prim_to_graph(p));
            }
            // Select newly created prims (clears old prim selection, preserves node/light/layer).
            let new_ids: Vec<SceneItemId> = indices.iter()
                .filter(|&&i| i < s.scene.primitives.len())
                .map(|&i| SceneItemId::Prim(s.scene.primitives[i].prim_uid))
                .collect();
            s.select_after_create(&new_ids);
            s.rebake_no_undo();
        });
        self.toast(&format!("🤖 Agent: {} primitives generated!", count));
    }

    // ---- Polygonal Mesh --------------------------------------------------------

    pub(crate) fn create_poly_mesh(&mut self, gen: &str) {
        use core_math::primitive_spec::PrimitiveSpec;
        // An unknown kind creates NOTHING — never a silently substituted shape.
        let Some(spec) = PrimitiveSpec::from_kind_id(gen) else {
            self.toast(&format!("Unknown poly primitive '{gen}'"));
            return;
        };
        self.create_poly_primitive(spec);
    }

    /// Create a parametric poly primitive from its spec. THE create path; `create_poly_mesh`
    /// resolves a menu id to a default spec and delegates here.
    pub(crate) fn create_poly_primitive(&mut self, spec: core_math::primitive_spec::PrimitiveSpec) {
        use core_math::primitive_spec::PrimitiveSpec;
        use core_math::scene_graph::NodePayload;
        let gen = spec.kind_id();

        // Cylinder/cone sides remember the last value the user set.
        let mut spec = spec;
        let remembered = self.ui.poly_create_last_sides.clamp(3, 128);
        match &mut spec {
            PrimitiveSpec::Cylinder(s) => s.sides = remembered,
            PrimitiveSpec::Cone(s) => s.sides = remembered,
            _ => {}
        }

        let mut adj = crate::types::PolyCreateAdjust {
            nid: core_math::scene_graph::NodeId(0), // filled in after the node is created
            spec,
            pos: Vec3::ZERO,
            scale: Vec3::ONE,
            rot: Vec3::ZERO,
            place_on: None,
            needs_settle: false,
        };
        // Placement priority (owner 2026-07-30 — final):
        // 1. **Workplane SET** (Locked, non-factory) → place at WP origin, oriented to WP normal.
        //    Selection must NOT override this.
        // 2. **No WP set + selection** → sit ON selection (sub-element or whole poly body).
        // 3. **Nothing selected** → world origin (pos/rot stay ZERO).
        self.poly_apply_create_placement(&mut adj);

        // Build mesh + source together so the spec and the signature it describes cannot drift.
        let (mesh, mesh_source) = core_math::primitive_spec::MeshSource::build(adj.spec);

        // Blender Shift+A in EDIT mode: append INTO the open body (one mesh, new island),
        // never spawn a second object. Workplane only affects **placement** (origin/orient),
        // not object vs island — owner 2026-08-07: cylinder + WP in edit mode wrongly made a
        // new object with nothing selected.
        if self.poly_work_mode != PolyWorkMode::Object {
            if let Some(nid) = self.sel.usel.active_node() {
                if self.poly_node_mesh(nid).is_some() {
                    let half_y = Self::poly_create_half_along_y(&adj);
                    self.poly_add_primitive_to_body(nid, mesh, gen, half_y);
                    if let Some(sides) = spec_sides_to_remember(&adj.spec) {
                        self.ui.poly_create_last_sides = sides;
                    }
                    // WP: arm Scale on the selected island (selection set inside add_to_body).
                    if self.poly_workplane_is_active_set() {
                        self.tool.action_center = crate::types::ActionCenter::Workplane;
                        self.tool.gizmo_mode = crate::types::GizmoMode::Scale;
                        self.tool.gizmo_armed = true;
                    }
                    self.ui.poly_create_adjust = Some(crate::types::PolyCreateAdjust {
                        nid,
                        ..adj
                    });
                    let tool_key = match gen {
                        "box" | "cube" => "Cube",
                        "plane" => "Plane",
                        "circle" => "Circle",
                        "sphere" => "Sphere",
                        "icosphere" => "Ico Sphere",
                        "cylinder" => "Cylinder",
                        "cone" => "Cone",
                        "torus" => "Torus",
                        _ => "Cube",
                    };
                    self.ui2.active_tool = tool_key.into();
                    self.ui2.active_pillar = "poly".into();
                    return;
                }
            }
        }

        let name = format!("Poly.{}", gen);
        let place_pos = adj.pos;
        let place_rot = adj.rot;
        let mut new_nid = None;
        self.transaction("Create poly mesh", |s| {
            let root = s.scene.scene_graph.root();
            let nid = s.scene.scene_graph.add_child(root, NodePayload::PolygonalBody {
                name: name.clone(),
                mesh: std::sync::Arc::new(mesh),
                // Parametric from birth: the params stay editable until the user applies them
                // or edits the topology.
                source: mesh_source,
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
            });
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                node.local_position = place_pos;
                node.local_rotation = place_rot;
                node.local_scale = Vec3::ONE;
            }
            // Select new poly body node (clears old prim selection, preserves light/layer).
            s.select_after_create(&[SceneItemId::Node(nid)]);
            s.render.viewport_mode = ViewportRenderMode::Hybrid;
            // Node creation is STRUCTURAL: without SCENE the cached structural draw list
            // never learns the new body exists and the solid appears only on the next
            // unrelated rebuild ("no surface until I click", owner 2026-08-03).
            s.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
            new_nid = Some(nid);
        });
        // Created ON an active workplane → full mesh island selected + Scale armed (owner
        // 2026-08-04 / 2026-08-07). Without selecting the island, Face/Edge/Vertex mode still
        // had an empty component selection so the Scale gizmo had nothing to drag.
        // `ActionCenter::Workplane` resolves to WP origin/orientation. Reverted to
        // `Automatic` by `reset_workplane()` when the workplane is disabled.
        if self.poly_workplane_is_active_set() {
            if let Some(nid) = new_nid {
                self.poly_select_full_body_island(nid);
                // Face domain: island is visible and is the gizmo Scale target.
                self.poly_work_mode = PolyWorkMode::Face;
            }
            self.tool.action_center = crate::types::ActionCenter::Workplane;
            self.tool.gizmo_mode = crate::types::GizmoMode::Scale;
            self.tool.gizmo_armed = true;
        }

        // Open the Blender-style "adjust last create" panel (Sides / Radius / Height live).
        if let Some(nid) = new_nid {
            if let Some(sides) = spec_sides_to_remember(&adj.spec) {
                self.ui.poly_create_last_sides = sides;
            }
            self.ui.poly_create_adjust = Some(crate::types::PolyCreateAdjust { nid, ..adj });
            // Tool Parameters dock shows the same Adjust fields (owner: last action = tool params).
            let tool_key = match gen {
                "box" | "cube" => "Cube",
                "plane" => "Plane",
                "circle" => "Circle",
                "sphere" => "Sphere",
                "icosphere" => "Ico Sphere",
                "cylinder" => "Cylinder",
                "cone" => "Cone",
                "torus" => "Torus",
                _ => "Cube",
            };
            self.ui2.active_tool = tool_key.into();
            self.ui2.active_pillar = "poly".into();
        }
        let place_toast = if self.poly_workplane_is_active_set() && adj.place_on.is_some() {
            " on workplane"
        } else if adj.place_on.is_some() {
            " on selection"
        } else {
            ""
        };
        self.toast(&format!("Created {}{place_toast}", name));
    }

    /// True when the construction workplane is **Locked** (Home / save-load / explicit set).
    /// Auto mode follows the camera and is not used as a create target.
    pub(crate) fn poly_workplane_is_active_set(&self) -> bool {
        self.viewport.wp_mode == WorkPlaneMode::Locked
            && self.viewport.wp_resolved.normal.normalize_or_zero().length_squared() > 0.25
    }

    /// Select every live face / edge / vertex of poly body `nid` (one body = one island).
    /// Used after create-on-workplane so Scale has a full selection target immediately.
    pub(crate) fn poly_select_full_body_island(&mut self, nid: core_math::scene_graph::NodeId) {
        use core_math::half_edge::{FaceId, HalfEdgeId, VertexId, NO_EDGE, NO_TWIN};
        use core_math::scene_graph::NodePayload;

        self.poly_sel.selected_vertices.clear();
        self.poly_sel.selected_edges.clear();
        self.poly_sel.selected_faces.clear();

        let Some(node) = self.scene.scene_graph.get(nid) else {
            return;
        };
        let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
            return;
        };

        for i in 0..mesh.faces.len() as u32 {
            let fid = FaceId(i);
            if mesh.face(fid).half_edge == NO_EDGE {
                continue;
            }
            self.poly_sel.selected_faces.insert(i);
        }
        for i in 0..mesh.vertices.len() as u32 {
            let vid = VertexId(i);
            if mesh.vert(vid).half_edge == NO_EDGE {
                continue;
            }
            self.poly_sel.selected_vertices.insert(i);
        }
        for i in 0..mesh.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !mesh.is_edge_live(he) {
                continue;
            }
            let twin = mesh.he(he).twin;
            // Twin-dedupe undirected edges (same as loop-cut / bridge selection).
            if twin != NO_TWIN && twin.0 < i {
                continue;
            }
            self.poly_sel.selected_edges.insert(i);
        }
        self.poly_invalidate_pick_cache();
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Apply place_on + rot/pos for a poly create.
    /// Priority: (1) locked workplane → WP origin (2) selection (3) world origin.
    fn poly_apply_create_placement(&self, adj: &mut crate::types::PolyCreateAdjust) {
        let place = if self.poly_workplane_is_active_set() {
            // Priority 1: locked workplane wins over any selection.
            let wp = &self.viewport.wp_resolved;
            let n = wp.normal.normalize_or_zero();
            if n.length_squared() > 0.25 {
                Some(crate::types::PolyCreatePlaceOn {
                    point: wp.origin,
                    normal: n,
                })
            } else {
                None
            }
        } else if let Some((point, normal)) = self.poly_selection_place_frame() {
            // Priority 2a: face / edge / vert selection.
            Some(crate::types::PolyCreatePlaceOn { point, normal })
        } else if let Some((point, normal)) = self.poly_object_place_frame() {
            // Priority 2b: whole poly body selected.
            Some(crate::types::PolyCreatePlaceOn { point, normal })
        } else {
            // Priority 3: nothing → leave pos/rot at origin.
            None
        };
        if let Some(place) = place {
            if place.normal.length_squared() > 0.25 {
                // Full WP frame when locked (U/V + normal) so cylinder axis matches the plane.
                if self.poly_workplane_is_active_set() {
                    let wp = &self.viewport.wp_resolved;
                    adj.rot = Self::euler_from_workplane_frame(
                        wp.tangent_u,
                        place.normal,
                        wp.tangent_v,
                    );
                } else {
                    adj.rot = Self::euler_from_workplane_normal(place.normal);
                }
                // Base sits on the plane: center = origin + normal * half_height.
                adj.pos = poly_create_pos_on_surface(adj, &place);
                adj.place_on = Some(place);
            }
        }
    }

    /// World frame for placing a prim on the active poly body (Object mode selection).
    /// Top of local AABB, normal = body +Y (or world Y if degenerate).
    fn poly_object_place_frame(&self) -> Option<(Vec3, Vec3)> {
        use core_math::scene_graph::NodePayload;
        let nid = self.sel.usel.active_node()?;
        // Only when the active selection is a poly body node (not a light/SDF-only scene).
        let node = self.scene.scene_graph.get(nid)?;
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return None,
        };
        // Prefer object-mode whole-body placement; also used in edit mode when no sub-elements
        // are selected so Shift+A still sits the new prim on the open body.
        if self.poly_work_mode != PolyWorkMode::Object {
            let has_sub = !self.poly_sel.selected_faces.is_empty()
                || !self.poly_sel.selected_edges.is_empty()
                || !self.poly_sel.selected_vertices.is_empty();
            if has_sub {
                return None; // sub-selection handled by poly_selection_place_frame
            }
        }
        let world = self.scene.scene_graph.world_transform(nid);
        let (mn, mx) = mesh.bounding_box();
        let local_top = Vec3::new(
            (mn.x + mx.x) * 0.5,
            mx.y,
            (mn.z + mx.z) * 0.5,
        );
        let point = world.transform_point3(local_top);
        let normal = world.transform_vector3(Vec3::Y).normalize_or_zero();
        if normal.length_squared() < 0.25 {
            Some((point, Vec3::Y))
        } else {
            Some((point, normal))
        }
    }

    /// Half-extent of the primitive along its local +Y (after orient-to-normal), used to sit
    /// the mesh ON the surface without intersecting it.
    pub(crate) fn poly_create_half_along_y(a: &crate::types::PolyCreateAdjust) -> f32 {
        use core_math::primitive_spec::PrimitiveSpec as P;
        let sy = a.scale.y.abs().max(1e-6);
        // Derived from the spec's own dimensions instead of a parallel table of magic numbers, so
        // adding a primitive can no longer forget this and place the mesh half-buried.
        let half = match &a.spec {
            P::Box(s) => s.size.y * 0.5,
            P::Plane(_) | P::Circle(_) => 0.0, // coplanar with the surface
            P::UvSphere(s) => s.radius,
            P::IcoSphere(s) => s.radius,
            P::Cylinder(s) => s.height * 0.5,
            P::Cone(s) => s.height * 0.5,
            P::Torus(s) => s.minor_r,
        };
        half.max(0.0) * sy
    }

    // Floating "Adjust box" window removed — same UI is docked in Tool Parameters only
    // (`tool_params::draw_tool_params_poly_create_adjust`). No second panel.

    // ---- NURBS Surface ---------------------------------------------------------

    /// Create a NURBS surface and add it as a scene graph node.
    pub(crate) fn create_nurbs_surface(&mut self, gen: &str) {
        use core_math::nurbs::{NurbsSurface as NS, NurbsCurve, ControlPoint};
        use core_math::scene_graph::NodePayload;

        // Closed solid primitives = ONE NurbsSolid (N faces). Legacy single-surface
        // NurbsBody is only for open reference surfaces (plane / revolve / loft).
        let solid: Option<(&str, Vec<core_math::nurbs::NurbsFace>)> = match gen {
            "cube" | "box" => Some(("Cube", NS::new_box_nurbs_faces(0.5, 0.5, 0.5))),
            "cylinder" => Some(("Cylinder", NS::new_cylinder_nurbs_faces(0.6, 2.0))),
            "sphere" => Some(("Sphere", NS::new_sphere_nurbs_faces(0.8))),
            "torus" => Some(("Torus", NS::new_torus_nurbs_faces(0.6, 0.2))),
            _ => None,
        };
        if let Some((label, faces)) = solid {
            let n_faces = faces.len();
            self.transaction(&format!("Create NURBS {label}"), |s| {
                let root = s.scene.scene_graph.root();
                let nid = s.scene.scene_graph.add_child(
                    root,
                    NodePayload::NurbsSolid {
                        name: format!("NURBS.{label}"),
                        faces,
                        mesh_cache_id: None,
                        material: core_math::primitives::PbrMaterial::default(),
                    },
                );
                s.select_after_create(&[SceneItemId::Node(nid)]);
                s.modeling_context = ModelingContext::Nurbs;
                s.nurbs_work_mode = NurbsWorkMode::Object;
                s.nurbs_sel.clear_sub();
                s.nurbs_sync_mesh_cache(nid);
                s.render.viewport_mode = ViewportRenderMode::Hybrid;
                // Structural: new node must enter the cached draw list this frame.
                s.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
                s.mark_outliner_dirty();
            });
            self.toast(&format!(
                "Created {label} — {n_faces} face solid · Face/Edge/Vertex edit modes"
            ));
            return;
        }

        let surface = match gen {
            "plane" => NS::new_plane(2.0, 2.0),
            "revolve" => {
                // Default profile: semicircle-ish in XY plane, revolve around Y axis
                let profile = NurbsCurve {
                    control_points: vec![
                        ControlPoint { position: Vec3::new(0.5, -0.8, 0.0), weight: 1.0 },
                        ControlPoint { position: Vec3::new(0.6, -0.3, 0.0), weight: 1.0 },
                        ControlPoint { position: Vec3::new(0.8,  0.0, 0.0), weight: 1.0 },
                        ControlPoint { position: Vec3::new(0.6,  0.3, 0.0), weight: 1.0 },
                        ControlPoint { position: Vec3::new(0.3,  0.8, 0.0), weight: 1.0 },
                    ],
                    knots: NurbsCurve::uniform_knots(5, 3),
                    degree: 3,
                    name: "profile".into(),
                };
                NS::from_revolve(&profile, Vec3::Y, 360.0, 12)
            }
            "loft" => {
                // Default: 3 cross-section curves at different heights
                let make_circle = |y: f32, r: f32| NurbsCurve {
                    control_points: vec![
                        ControlPoint { position: Vec3::new(r, y, 0.0), weight: 1.0 },
                        ControlPoint { position: Vec3::new(0.0, y, r), weight: 1.0 },
                        ControlPoint { position: Vec3::new(-r, y, 0.0), weight: 1.0 },
                        ControlPoint { position: Vec3::new(0.0, y, -r), weight: 1.0 },
                        ControlPoint { position: Vec3::new(r, y, 0.0), weight: 1.0 },
                    ],
                    knots: NurbsCurve::uniform_knots(5, 2),
                    degree: 2,
                    name: "section".into(),
                };
                NS::from_loft(&[make_circle(-0.8, 0.3), make_circle(0.0, 0.7), make_circle(0.8, 0.4)])
            }
            _ => return,
        };
        let name = format!("NURBS.{}", gen);
        self.transaction("Create NURBS surface", |s| {
            let root = s.scene.scene_graph.root();
            let nid = s.scene.scene_graph.add_child(root, NodePayload::NurbsBody {
                name: name.clone(),
                surface: std::sync::Arc::new(surface),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
            });
            // Select new NURBS body node (clears old prim selection, preserves light/layer).
            s.select_after_create(&[SceneItemId::Node(nid)]);
            s.render.viewport_mode = ViewportRenderMode::Hybrid;
            // Structural: new node must enter the cached draw list this frame.
            s.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
        });
        self.toast(&format!("Created {} (open surface)", name));
    }

    // ---- Scene bounds ----------------------------------------------------------

    /// Compute the bounding radius of all visible primitives from the scene origin.
    /// Used for dynamic through-cut depth so infinite cuts always pierce all geometry.
    pub(crate) fn scene_bound_radius(&self) -> f32 {
        let mut max_r: f32 = 0.0;
        for p in &self.scene.primitives {
            if !p.visible { continue; }
            let prim_r = match &p.shape {
                SdfShape::Sphere{radius} => *radius * p.scale.max_element(),
                SdfShape::Box{half_extents,..} => (p.scale * *half_extents).length(),
                SdfShape::Cylinder{radius,half_height}|SdfShape::Cone{radius,half_height}|SdfShape::Capsule{radius,half_height} => {
                    let r = p.scale.x.max(p.scale.z) * *radius;
                    let hh = p.scale.y * *half_height;
                    (r*r+hh*hh).sqrt()
                },
                SdfShape::Torus{major_radius,minor_radius} => p.scale.max_element()*(*major_radius+*minor_radius),
                SdfShape::Stair{half_extents,..} => (*half_extents * p.scale).length(),
                SdfShape::SpiralStair{radius,half_height,..} => p.scale.max_element()*(radius+half_height),
                SdfShape::Billboard{width,height,..} => p.scale.max_element()*(width+height)*0.5,
                SdfShape::CurveExtrude { half_depth, .. } => p.scale.max_element() * *half_depth,
                SdfShape::MeshVolume { grid, .. } => (p.scale * grid.corner_abs_max()).length(),
                // Infinite plane cutter — contributes no finite geometry radius to the through-cut depth.
                SdfShape::HalfSpace => 0.0,
                SdfShape::SpinePipe { radius, .. } => *radius,
            };
            let dist_from_origin = p.position.length() + prim_r;
            max_r = max_r.max(dist_from_origin);
        }
        max_r.max(1.0) // floor at 1.0 for empty scenes
    }
}

/// World position for a primitive sitting ON `place` (surface point + normal):
/// center = point + normal * half_extent_along_Y so the mesh does not intersect the surface.
pub(crate) fn poly_create_pos_on_surface(
    a: &crate::types::PolyCreateAdjust,
    place: &crate::types::PolyCreatePlaceOn,
) -> Vec3 {
    let half = AzraApp::poly_create_half_along_y(a);
    let n = place.normal.normalize_or_zero();
    if n.length_squared() < 0.25 {
        return place.point;
    }
    place.point + n * half
}

/// Sides value worth remembering as the next create default.
///
/// Only the revolved primitives whose silhouette the user actively tunes (cylinder/cone) feed
/// `poly_create_last_sides` — same behaviour as before, now driven by the typed spec instead of
/// a `matches!(gen, "cylinder" | "cone")` string test repeated at three call sites.
pub(crate) fn spec_sides_to_remember(spec: &core_math::primitive_spec::PrimitiveSpec) -> Option<u32> {
    use core_math::primitive_spec::PrimitiveSpec as P;
    match spec {
        P::Cylinder(s) => Some(s.sides),
        P::Cone(s) => Some(s.sides),
        _ => None,
    }
}
