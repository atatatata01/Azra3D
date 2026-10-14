//! Queue + worker + GPU upload. Frame-thread only for `upload_map`.

use super::{MeshMapJob, MeshMapKind, MeshMapResult, PendingMeshMapBake};
use core_math::half_edge::FaceId;

impl crate::AzraApp {
    /// Menu / button entry. Validates loudly, then QUEUES — never bakes inline.
    pub(crate) fn poly_request_mesh_map_bake(&mut self) {
        let Some(body) = self.poly_curv_body() else {
            self.toast("Bake Mesh Maps: select a polygonal body first");
            return;
        };
        self.ui2.active_tool = "Mesh Maps".into();
        self.poly_request_mesh_map_bake_for(body, None);
    }

    /// Queue a bake for a specific body. `enabled_maps = None` uses the body's settings.
    pub(crate) fn poly_request_mesh_map_bake_for(
        &mut self,
        body: core_math::scene_graph::NodeId,
        enabled_maps: Option<u32>,
    ) {
        // Arm even when the bake cannot start (no UVs) — the user picked this tool.
        self.ui2.active_tool = "Mesh Maps".into();
        let has_uvs = self
            .cache
            .poly_custom_data
            .get(&body)
            .is_some_and(|cd| !cd.uv.is_empty());
        if !has_uvs {
            self.toast("Bake Mesh Maps: this body has no UVs — run Poly ▸ UV Unwrap first");
            return;
        }
        let mut settings = self.bake_settings_for(body).clamp();
        if let Some(bits) = enabled_maps {
            settings.enabled_maps = bits;
        }
        if settings.maps().is_empty() {
            self.toast("Bake Mesh Maps: no maps selected");
            return;
        }
        let res = settings.res_global;
        self.cache.pending_mesh_map_bake = Some(PendingMeshMapBake { body, settings });
        self.toast(&format!("Baking mesh maps at {res}²…"));
    }

    /// Poll phase: START a queued bake on a WORKER, and LAND finished ones.
    pub(crate) fn poll_mesh_map_bakes(&mut self) -> bool {
        self.poll_baked_image_export();
        if self.cache.mesh_map_rx.is_some() {
            let got = match self.cache.mesh_map_rx.as_ref().unwrap().try_recv() {
                Ok(done) => Some(Ok(done)),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err(())),
            };
            match got {
                None => return false,
                Some(Err(())) => {
                    self.cache.mesh_map_rx = None;
                    self.toast("Bake Mesh Maps: the bake worker died");
                    return false;
                }
                Some(Ok(done)) => {
                    self.cache.mesh_map_rx = None;
                    return self.land_mesh_map_result(done);
                }
            }
        }

        let Some(req) = self.cache.pending_mesh_map_bake.take() else {
            return false;
        };
        let Some(src) = self.mesh_map_source(req.body) else {
            return false;
        };
        let face_groups: Vec<u16> = self
            .cache
            .poly_custom_data
            .get(&req.body)
            .filter(|cd| !cd.groups.is_empty())
            .map(|cd| {
                (0..src.rings.face_count())
                    .map(|fi| {
                        cd.groups
                            .get(FaceId(fi as u32))
                            .copied()
                            .unwrap_or(0)
                    })
                    .collect()
            })
            .unwrap_or_default();

        let high = self.snapshot_high_target(req.body, &req.settings);
        let cage = self.snapshot_cage_target(req.body, &req.settings);
        let job = MeshMapJob {
            body: req.body,
            src,
            face_groups,
            settings: req.settings,
            high,
            cage,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        match std::thread::Builder::new()
            .name("azra-meshmap-bake".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let _ = tx.send(job.run());
            }) {
            Ok(_) => {
                self.cache.mesh_map_rx = Some(rx);
                true
            }
            Err(e) => {
                self.toast(&format!("Bake Mesh Maps: could not start worker ({e})"));
                false
            }
        }
    }

    fn land_mesh_map_result(&mut self, done: MeshMapResult) -> bool {
        use super::registry::{now_secs, BakedImage};
        self.ui.last_mesh_map_ms = done.ms;
        let body_name = self
            .scene
            .scene_graph
            .get(done.body)
            .map(|n| n.payload.display_name().to_string())
            .unwrap_or_else(|| format!("Body{}", done.body.0));
        let key = self
            .poly_display_cache
            .get(&done.body)
            .map(|e| e.key)
            .unwrap_or((0, 0));
        let mut landed: Vec<(MeshMapKind, u32, String)> = Vec::new();
        for baked in &done.pixels {
            let kind = baked.kind;
            let res = crate::material_textures::map_side_len(&baked.hi).unwrap_or(0);
            let multi = done.pixels.iter().any(|p| p.udim != baked.udim)
                || baked.udim != super::udim::UDIM_1001;
            let auto = super::BakedImageRegistry::auto_name_udim(
                &body_name,
                kind.label(),
                res,
                baked.udim,
                multi,
            );
            let name = self
                .ui
                .baked_images
                .by_body_kind_udim(done.body, kind.bit(), baked.udim)
                .map(|e| e.name.clone())
                .unwrap_or_else(|| self.ui.baked_images.uniquify(&auto));
            let Some(gpu) = self.gpu.as_mut() else {
                self.toast("Bake Mesh Maps: no GPU context");
                return false;
            };
            let (device, queue) = (gpu.device.clone(), gpu.queue.clone());
            let slot = if let Some(lo) = baked.lo.as_ref() {
                gpu.material_textures
                    .upload_map_16(&device, &queue, &baked.hi, lo, &name)
            } else {
                gpu.material_textures
                    .upload_map(&device, &queue, &baked.hi, &name)
            };
            self.ui.baked_images.upsert(BakedImage {
                name: name.clone(),
                body: done.body,
                kind_bit: kind.bit(),
                res,
                slot,
                display_key: key,
                baked_at_secs: now_secs(),
                pixels_missing: slot == 0,
                udim: baked.udim,
            });
            landed.push((kind, slot, name));
        }
        self.bake_notes_report(&done.notes);
        if landed.is_empty() {
            self.toast("Bake Mesh Maps: produced nothing");
            return false;
        }
        let list = landed
            .iter()
            .map(|(k, s, n)| format!("{} ({n} → slot {s})", k.label()))
            .collect::<Vec<_>>()
            .join(", ");
        self.toast(&format!("Baked mesh maps ({:.0} ms): {list}", done.ms));
        self.mark_dirty(crate::DirtyFlags::RENDER);
        true
    }
}
