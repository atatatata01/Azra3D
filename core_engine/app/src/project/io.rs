//! Save/load I/O — atomic write, bincode v7 + FL4ORDER side-channel, JSON fallback.
//! PURE RELOCATION from project.rs (2026-08-03 god-file split). Save-format red lines
//! documented on the fns themselves — the serialized SHAPES live in mod.rs.
use super::*;

/// Sibling temp path in the SAME directory as `path` (`file.fl4` → `file.fl4.tmp`).
/// Same directory keeps the rename on one filesystem, which is what makes it atomic.
fn sibling_tmp(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("azra-save"));
    name.push(".tmp");
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(name),
        _ => PathBuf::from(name),
    }
}

/// The panic value a worker thread actually panicked with. `join()` only hands back
/// `Box<dyn Any>`; dropping it is how a serialize failure became the fixed string
/// "thread panicked" with no OS or bincode detail.
fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else {
        "unknown panic".to_string()
    }
}

/// Write `data` to a temporary file in the same directory as `path`, fsync it, then
/// rename over the target. A failure leaves any existing target byte-for-byte intact:
/// the destination is never opened for write, and a failed attempt deletes only the temp.
pub(crate) fn atomic_write(path: &Path, data: &[u8]) -> Result<(), String> {
    let tmp = sibling_tmp(path);
    let write_result = (|| -> Result<(), String> {
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| format!("Create {}: {e}", tmp.display()))?;
        if let Err(e) = f.write_all(data) {
            return Err(format!("Write {}: {e}", tmp.display()));
        }
        // `flush` only reaches the OS page cache. `sync_all` (fsync / FlushFileBuffers)
        // waits until the bytes are durable. Rename after that, so a crash cannot
        // publish a truncated target.
        if let Err(e) = f.sync_all() {
            return Err(format!("Sync {}: {e}", tmp.display()));
        }
        drop(f);
        std::fs::rename(&tmp, path).map_err(|e| {
            format!("Rename {} → {}: {e}", tmp.display(), path.display())
        })?;
        // Directory fsync makes the rename itself durable. Windows cannot open a
        // directory this way; the file sync above is the durability guarantee there.
        #[cfg(unix)]
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                if let Ok(df) = std::fs::File::open(dir) {
                    let _ = df.sync_all();
                }
            }
        }
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    write_result
}

/// Magic delimiter for the optional, self-delimiting `child_order` side-channel appended AFTER the
/// bincode `ProjectFile` blob. Layout: `[ bincode(ProjectFile) ][ FL4ORDER ][ bincode(order map) ]`.
/// Absent on genuine v7 files (and on v7 files with all-default orders) → loader leaves child_order
/// empty and self-heals to legacy order. The blob itself is byte-identical to v7 (child_order is
/// `#[serde(skip)]`), so the version number stays 7 and this MAGIC is the only format-extension signal.
pub(crate) const CHILD_ORDER_MAGIC: &[u8; 8] = b"FL4ORDER";
/// Second self-delimiting FL4 tail section: persisted material maps (#2). Distinct 8-byte magic (same
/// length as CHILD_ORDER_MAGIC → uniform sniff math) so the loader can read it after the child_orders
/// section. Adding this as a SEPARATE magic section is safe ONLY because the loader now reads each
/// section with `deserialize_from` (position-tracked) instead of `deserialize` (which consumed all).
pub(crate) const MATERIAL_MAP_MAGIC: &[u8; 8] = b"FL4MATTX";
/// Third self-delimiting FL4 tail section: per-material non-albedo map-channel params (the
/// `#[serde(skip)]` normal/roughness/metallic `TexMap`s). Same 8-byte-magic + bincode-value shape
/// as the other two sections; the loader's sniff loop is order-independent, and an OLDER AZRA that
/// doesn't know this magic simply stops reading the tail there (graceful: it loses only the new
/// channels, never the blob).
pub(crate) const MATERIAL_CHANNEL_MAGIC: &[u8; 8] = b"FL4MATX2";
/// Fourth self-delimiting FL4 tail section: the per-project environment HDRI record
/// (`EnvRecord` — path + brightness + rotation). Same 8-byte-magic + bincode-value shape as the
/// other sections; written LAST so an older AZRA (whose sniff loop stops at the first unknown
/// magic) still reads every section it knows. Absent whenever no HDRI is loaded → files stay
/// byte-identical to pre-env saves.
pub(crate) const ENV_MAGIC: &[u8; 8] = b"FL4ENVIR";
/// Fifth self-delimiting FL4 tail section: per-light volumetric ("god ray") params
/// (`LightVolRecord` — id + enabled + density + softness + zone). Same 8-byte-magic + bincode-value
/// shape as the others; written AFTER the env section (append-newest-last — `FL4ENVIR` predates it,
/// so an env-aware/ltvol-unaware reader reaches env before stopping at this unknown magic). Absent
/// whenever no light has authored volumetrics → files stay byte-identical to pre-volumetric saves.
pub(crate) const LIGHTVOL_MAGIC: &[u8; 8] = b"FL4LTVOL";
/// Sixth self-delimiting FL4 tail section: per-light RAY-TRACED SHADOW params (RT_SHADOWS_DESIGN
/// Stage 4 — `LightRtRecord` = id + enabled + radius). Same 8-byte-magic + bincode-value shape.
/// Written after `FL4LTVOL` (was newest at Stage 4). **`FL4NODES` is now written after this**
/// (append-newest-last). Absent whenever no light has authored RT params → files stay byte-identical
/// to pre-Stage-4 saves.
pub(crate) const LIGHTRT_MAGIC: &[u8; 8] = b"FL4LTRTS";
/// Seventh self-delimiting FL4 tail section: per-prim procedural shader node graphs
/// (`PROCEDURAL_TEXTURING_DESIGN` PR0 — `NodeGraphRecord` = prim_uid + JSON graph). Same
/// 8-byte-magic + bincode-value shape as sibling sections. Written **LAST (the NEWEST section —
/// after `FL4LTRTS`, append-newest-last)**, so an older AZRA whose sniff loop stops at this unknown
/// magic still reads every section it knows (child_order / material maps / env / volumetrics / RT)
/// and loses only node graphs, never the blob. Absent whenever no prim has a node graph → files
/// stay byte-identical to pre-FL4NODES saves.
pub(crate) const NODE_GRAPH_MAGIC: &[u8; 8] = b"FL4NODES";
/// Eighth self-delimiting FL4 tail section: per-material MATERIAL PANEL docs
/// (MATERIAL_PANEL_SPEC P0 — `MaterialPanelRecord` = material_id + JSON doc). Same
/// 8-byte-magic + bincode-value shape as sibling sections. Written **LAST (the NEWEST
/// section — after `FL4NODES`, append-newest-last)**, so an older AZRA stops at this
/// unknown magic having already read every section it knows. Absent whenever no material
/// has a panel doc → files stay byte-identical to pre-FL4MPNLS saves.
pub(crate) const MATERIAL_PANEL_MAGIC: &[u8; 8] = b"FL4MPNLS";
/// Ninth self-delimiting FL4 tail section: per-poly-body AUTO SMOOTH angle (Blender-style normal
/// control — `PolyNormalRecord` = node_id + degrees). Same 8-byte-magic + bincode-value shape as
/// sibling sections. Written **LAST (the NEWEST section — after `FL4MPNLS`, append-newest-last)**,
/// so an older AZRA stops at this unknown magic having already read every section it knows.
/// Absent whenever no body has an authored angle → files stay byte-identical to pre-FL4PNORM
/// saves, and an older build loses only the angle (the mesh, and the per-edge `sharp` marks that
/// ride the in-blob `poly_layers`, are untouched).
pub(crate) const POLY_NORMAL_MAGIC: &[u8; 8] = b"FL4PNORM";

/// Parametric poly primitives (`PolygonalBody.source`). Same 8-byte-magic + bincode-value shape as
/// its siblings, written **LAST (newest section, after `FL4MPNLS`)** so an older AZRA reaches every
/// section it knows before stopping at this unknown magic. Absent whenever no body is parametric →
/// files stay byte-identical to pre-FL4PPRM saves.
///
/// This is a TAIL SECTION rather than an in-blob `#[serde(default)]` field for a load-bearing
/// reason: an in-blob trailing field was tried and broke every committed `.fl4` fixture with
/// `failed to fill whole buffer` — bincode is positional, so `serde(default)` cannot rescue bytes
/// that were never written. See `docs/planning/poly/PARAMETRIC_PRIMITIVES.md`.
pub(crate) const POLY_PARAMS_MAGIC: &[u8; 8] = b"FL4PPRM\0";

/// Per-poly-modifier `#[serde(skip)]` flags (`PolyModFlagRecord` — currently Bevel "Consume
/// Geometry"). Same 8-byte-magic + bincode-value shape as its siblings, written **LAST (newest
/// section, after `FL4PPRM`)** so an older AZRA reaches every section it knows before stopping at
/// this unknown magic. Absent whenever no modifier authored a flag → files stay byte-identical to
/// pre-FL4PMODF saves. Tail section, not an in-blob field, for the same load-bearing reason as
/// `FL4PPRM` above (bincode is positional; `serde(default)` cannot rescue unwritten bytes).
pub(crate) const POLY_MOD_FLAGS_MAGIC: &[u8; 8] = b"FL4PMODF";

/// Per-poly-modifier Smooth MASK FEATHER (`PolySmoothRecord` — the `#[serde(skip)]`
/// `PolyModifier.smooth_falloff`). Same 8-byte-magic + bincode-value shape as its siblings,
/// written **LAST (newest section, after `FL4PMODF`)** so an older AZRA reaches every section it
/// knows before stopping at this unknown magic. Absent whenever no Smooth modifier has both a
/// geometry channel and a non-default falloff → files stay byte-identical to pre-FL4PSMTH saves.
///
/// This is a NEW section rather than a field appended to `PolyModFlagRecord`, even though that
/// record exists for exactly this class of state. `load_binary` propagates a section decode error
/// with `?`, so widening an existing record does not degrade to "lose that section" — it fails the
/// ENTIRE load for any `.fl4` that already carries an `FL4PMODF` written by an older build.
pub(crate) const POLY_SMOOTH_MAGIC: &[u8; 8] = b"FL4PSMTH";

/// Per-poly-modifier VOXEL REMESH params (`PolyModifier.remesh_resolution` / `_smooth` /
/// `_padding` / `_project` — all four `#[serde(skip)]`). Same 8-byte-magic + bincode-value shape
/// as its siblings, written **LAST (newest section, after `FL4PSMTH`)** so an older AZRA reaches
/// every section it knows before stopping at this unknown magic. Absent whenever no modifier has
/// an authored non-default remesh param → files stay byte-identical to pre-FL4PRMSH saves.
///
/// **This section fixes a live data-loss bug (`REMESHPARAMSAVE`), it is not new plumbing for a new
/// feature.** The four fields were `#[serde(skip)]` — correctly, since bincode is positional and
/// putting them in the blob would corrupt every existing `.fl4` — but the doc comment on them
/// claimed they "ride the `FL4PMODF` tail", and **no such carrier was ever written**:
/// `PolyModFlagRecord` is only `{ node_id, mod_index, bevel_consume }`. So every authored Remesh
/// param silently reset to 32 / true / 0.1 / 1.0 on load.
///
/// A NEW section rather than four fields appended to `PolyModFlagRecord` / `PolySmoothRecord`, for
/// the reason `POLY_SMOOTH_MAGIC` above already records: `load_binary` propagates a section decode
/// error with `?`, so widening an existing record does not degrade to "lose that section" — it
/// fails the ENTIRE load for any `.fl4` that already carries that section from an older build.
///
/// Note this section deliberately has **no `geo_channel.is_all()` gate**, unlike `FL4PSMTH`. That
/// gate is right for mask params (inert without a channel) and would be wrong here: a Remesh
/// modifier's resolution is authored state whether or not it has a geometry channel.
pub(crate) const POLY_REMESH_MAGIC: &[u8; 8] = b"FL4PRMSH";

/// Remesh 2³ collapse flag (`PolyModifier.remesh_collapse`). After `FL4PRMSH`.
/// Absent → collapse stays off (old remesh files stay uniform).
pub(crate) const POLY_REMESH_COLLAPSE_MAGIC: &[u8; 8] = b"FL4PRMCL";

/// Remesh skip-relax-while-dragging (`PolyModifier.remesh_skip_relax_drag`).
/// After `FL4PRMCL`. Absent → off (old files keep full relax on drag).
pub(crate) const POLY_REMESH_SKIP_RELAX_MAGIC: &[u8; 8] = b"FL4PRMRL";

/// Remesh skip-seal-while-dragging (`PolyModifier.remesh_skip_seal_drag`).
/// After `FL4PRMRL`. Absent → off (old files keep full seal on drag).
pub(crate) const POLY_REMESH_SKIP_SEAL_MAGIC: &[u8; 8] = b"FL4PRMSL";

/// Remesh draft-res-while-dragging (`PolyModifier.remesh_drop_res_drag`).
/// After `FL4PRMSL`. Absent → off (old files keep authored res on drag).
pub(crate) const POLY_REMESH_DROP_RES_MAGIC: &[u8; 8] = b"FL4PRMDR";

/// Per-body mesh-map bake settings (`BakeSettingsRecord`). After remesh drag tails.
/// Absent whenever no body has authored non-default bake settings → files stay
/// byte-identical to pre-FL4BAKE saves.
///
/// Tail section, not an in-blob field: an in-blob trailing `#[serde(default)]` Vec was tried
/// for later features and broke every committed `.fl4` (`failed to fill whole buffer`).
pub(crate) const BAKE_SETTINGS_MAGIC: &[u8; 8] = b"FL4BAKE\0";
/// G7 16-bit flag. After FL4BAKE so a G4 reader stops here and only loses bit16.
pub(crate) const BAKE_BIT16_MAGIC: &[u8; 8] = b"FL4B16\0\0";
/// G7 mesh-cage nids. After FL4B16 so a 16-bit reader stops here and only loses the cage.
pub(crate) const BAKE_CAGE_MAGIC: &[u8; 8] = b"FL4CAGE\0";
/// G7 UDIM flags + tile numbers. After FL4CAGE.
pub(crate) const BAKE_UDIM_MAGIC: &[u8; 8] = b"FL4UDIM\0";
/// G7 GPU-ray flag. After FL4UDIM. Newest section.
pub(crate) const BAKE_GPU_RAY_MAGIC: &[u8; 8] = b"FL4GRAY\0";

/// G4 `BakeTail` first; G1 files stored a bare `Vec<BakeSettingsRecord>`.
fn read_bake_tail(
    cursor: &mut std::io::Cursor<&[u8]>,
    data: &[u8],
) -> Result<(Vec<BakeSettingsRecord>, Vec<BakedImageRecord>), String> {
    let pos = cursor.position();
    let rest = &data[pos as usize..];
    let mut probe = std::io::Cursor::new(rest);
    if let Ok(tail) = bincode::deserialize_from::<_, BakeTail>(&mut probe) {
        cursor.set_position(pos + probe.position());
        return Ok((tail.settings, tail.images));
    }
    cursor.set_position(pos);
    let settings = bincode::deserialize_from(cursor)
        .map_err(|e| format!("Deserialize bake_settings error: {}", e))?;
    Ok((settings, Vec::new()))
}

/// Per-poly-modifier bevel VARIABLE WIDTH (`PolyModifier.bevel_var_width` / `bevel_width_min`, both
/// `#[serde(skip)]`). Same 8-byte-magic + bincode-value shape as its siblings, written **LAST (the
/// newest section, after `FL4PRMSH`)** so an older AZRA reaches every section it knows before
/// stopping at this unknown magic. Absent whenever no modifier turned the toggle on → files stay
/// byte-identical to pre-FL4PBVAR saves, and an older build loses only the variation (it reads the
/// stack as a plain uniform-width bevel at `bevel_width`, which is what it did before this feature).
///
/// A NEW section rather than two fields appended to `PolyModFlagRecord` / `PolyRemeshRecord`, for the
/// reason `POLY_SMOOTH_MAGIC` and `POLY_REMESH_MAGIC` above both record: a tail record is itself
/// positional bincode, and `load_binary` propagates a section decode error with `?`, so widening one
/// does not degrade to "lose that section" — it fails the ENTIRE load for any `.fl4` already
/// carrying that section from an older build.
pub(crate) const POLY_BEVEL_VAR_MAGIC: &[u8; 8] = b"FL4PBVAR";

/// Per-poly-body **Smart Select `op_created` tags** (`PolyOpTagRecord` — node_id + half-edge ids).
/// Same 8-byte-magic + bincode-value shape as its siblings, written **LAST (the newest section,
/// after `FL4PBVAR`)** so an older AZRA reaches every section it knows before stopping at this
/// unknown magic. Absent whenever no body carries a tag → files stay byte-identical to
/// pre-FL4PTAGS saves, and an older build loses only the tags (the mesh and every authored layer
/// ride the in-blob `poly_layers`, untouched).
///
/// **This closes a documented data-loss gap, it is not new plumbing**
/// (`docs/planning/poly/SIDECAR_SURVIVES_BOOLEAN.md` §OPEN): `op_created` was SESSION-ONLY, so a
/// reopened file handed Smart Select nothing to skip and every operation-created edge beveled
/// again. Two earlier attempts to persist it failed — one added a field to `PolyLayerRecord`, one
/// added a trailing `#[serde(default)]` `Vec` to the blob — and BOTH corrupted every existing
/// `.fl4`, because bincode is positional and `serde(default)` cannot rescue bytes that were never
/// written. That doc concluded the only remaining options were a version bump, re-deriving the
/// tags from geometry, or giving up; it was written before the tail-section pattern existed. A
/// tail section costs ZERO blob bytes, which is the whole reason this one is safe. Do NOT
/// "simplify" it back into `PolyLayerRecord`.
///
/// A NEW section rather than a field appended to any existing record, for the reason
/// `POLY_SMOOTH_MAGIC` records: `load_binary` propagates a section decode error with `?`, so
/// widening an existing record fails the ENTIRE load for any `.fl4` already carrying that section.
pub(crate) const POLY_OP_TAGS_MAGIC: &[u8; 8] = b"FL4PTAGS";

/// Per-poly-modifier BEND params (`PolyModifier.bend_angle_deg` / `_axis` / `_dir_deg` /
/// `_limit_lo` / `_limit_hi` / `_center`, all six `#[serde(skip)]`). Same 8-byte-magic +
/// bincode-value shape as its siblings, written **LAST (the newest section, after `FL4PTAGS`)** so
/// an older AZRA reaches every section it knows before stopping at this unknown magic. Absent
/// whenever no Bend modifier has a non-default param → files stay byte-identical to pre-FL4PBEND
/// saves.
///
/// A NEW section rather than fields appended to any existing record, for the reason
/// `POLY_SMOOTH_MAGIC` above records: a tail record is itself positional bincode, and `load_binary`
/// propagates a section decode error with `?`, so widening one does not degrade to "lose that
/// section" — it fails the ENTIRE load for any `.fl4` already carrying it from an older build.
///
/// Note what an older build does with a file containing a Bend: `PolyModifierType::Bend` is the
/// LAST enum variant, so its bincode tag is one past anything an older build knows and that build
/// fails the load outright rather than silently mis-reading the stack. That is the append-only
/// enum rule working as intended — the tail section only protects the PARAMS, never the variant.
pub(crate) const POLY_BEND_MAGIC: &[u8; 8] = b"FL4PBEND";

/// Per-poly-modifier PolySculpt deltas (`sculpt_deltas` / `sculpt_delta_gen`, both
/// `#[serde(skip)]`). Own section AFTER `FL4PBEND` — never widen Bend. Absent when unused.
pub(crate) const POLY_SCULPT_MAGIC: &[u8; 8] = b"FL4SCLP\0";

/// `PivotConfig.local` per node (`#[serde(skip)]` — the 2026-08-18 in-blob
/// `serde(default)` bool corrupted every older .fl4: bincode is positional, `default`
/// never fires on read, and the decoder consumed a neighbouring byte as the bool).
/// This tail is the fix's carrier. Own section AFTER `FL4SCLP`, newest-last. Absent
/// when no Custom pivot is node-local (older builds lose only pivot locality).
pub(crate) const PIVOT_LOCAL_MAGIC: &[u8; 8] = b"FL4PIVT\0";

pub(crate) fn save_binary(path: &Path, project: &ProjectFile) -> Result<(), String> {
    // 8MB stack — bincode's recursive serializer overflows the default 1MB stack
    // on scenes with many large SdfPrimitive structs (280 floats each).
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .name("save-binary".into())
            .stack_size(8 * 1024 * 1024)
            .spawn_scoped(s, || {
                let mut data = bincode::serialize(project).map_err(|e| format!("Serialize error: {}", e))?;
                // Append the child_order side-channel ONLY if at least one body has a non-default order
                // (keeps files identical to v7 when the feature is unused).
                // Self-delimiting multi-section FL4 tail. Each section = 8-byte MAGIC + a bincode value,
                // written ONLY when non-empty (files stay byte-identical when a feature is unused). The
                // loader reads each section with deserialize_from so the cursor lands exactly at the next
                // magic (or EOF) — that is what lets multiple sections coexist without a field trap.
                if !project.child_orders.is_empty() {
                    let tail = bincode::serialize(&project.child_orders)
                        .map_err(|e| format!("Serialize child_order error: {}", e))?;
                    data.extend_from_slice(CHILD_ORDER_MAGIC);
                    data.extend_from_slice(&tail);
                }
                if !project.material_maps.is_empty() {
                    let tail = bincode::serialize(&project.material_maps)
                        .map_err(|e| format!("Serialize material_maps error: {}", e))?;
                    data.extend_from_slice(MATERIAL_MAP_MAGIC);
                    data.extend_from_slice(&tail);
                }
                if !project.material_channels.is_empty() {
                    let tail = bincode::serialize(&project.material_channels)
                        .map_err(|e| format!("Serialize material_channels error: {}", e))?;
                    data.extend_from_slice(MATERIAL_CHANNEL_MAGIC);
                    data.extend_from_slice(&tail);
                }
                // Append-newest-LAST (forward-compat: a consume-until-first-unknown older reader
                // must reach every section IT knows before hitting an unknown magic). ENV_MAGIC
                // predates LIGHTVOL_MAGIC, so env is written BEFORE light_volumetrics — otherwise an
                // env-aware/ltvol-unaware build stops at LIGHTVOL and silently drops the HDRI env.
                if let Some(env) = &project.env {
                    let tail = bincode::serialize(env)
                        .map_err(|e| format!("Serialize env record error: {}", e))?;
                    data.extend_from_slice(ENV_MAGIC);
                    data.extend_from_slice(&tail);
                }
                if !project.light_volumetrics.is_empty() {
                    let tail = bincode::serialize(&project.light_volumetrics)
                        .map_err(|e| format!("Serialize light_volumetrics error: {}", e))?;
                    data.extend_from_slice(LIGHTVOL_MAGIC);
                    data.extend_from_slice(&tail);
                }
                // Append-newest-LAST (relative order among known sections): FL4LTRTS after FL4LTVOL
                // so a volumetric-aware/RT-unaware reader reaches FL4LTVOL before this unknown magic.
                // Only lights with AUTHORED RT params emit records (byte-compat).
                if !project.light_rt.is_empty() {
                    let tail = bincode::serialize(&project.light_rt)
                        .map_err(|e| format!("Serialize light_rt error: {}", e))?;
                    data.extend_from_slice(LIGHTRT_MAGIC);
                    data.extend_from_slice(&tail);
                }
                // Append-newest-LAST: FL4NODES is the NEWEST section — written AFTER FL4LTRTS so an
                // RT-aware / node-graph-unaware reader reaches FL4LTRTS before this unknown magic.
                // Only scenes with at least one procedural graph emit the section (byte-compat).
                if !project.node_graphs.is_empty() {
                    let tail = bincode::serialize(&project.node_graphs)
                        .map_err(|e| format!("Serialize node_graphs error: {}", e))?;
                    data.extend_from_slice(NODE_GRAPH_MAGIC);
                    data.extend_from_slice(&tail);
                }
                // Append-newest-LAST: FL4MPNLS is the NEWEST section — written AFTER FL4NODES so a
                // node-graph-aware / panel-unaware reader reaches FL4NODES before this unknown magic.
                // Only scenes with at least one authored panel doc emit the section (byte-compat).
                if !project.material_panel_docs.is_empty() {
                    let tail = bincode::serialize(&project.material_panel_docs)
                        .map_err(|e| format!("Serialize material_panel_docs error: {}", e))?;
                    data.extend_from_slice(MATERIAL_PANEL_MAGIC);
                    data.extend_from_slice(&tail);
                }
                // Append-newest-LAST: FL4PNORM is written AFTER FL4MPNLS so a panel-aware /
                // normals-unaware reader reaches FL4MPNLS before this unknown magic.
                // Only bodies with an authored Auto Smooth angle emit records (byte-compat).
                if !project.poly_normals.is_empty() {
                    let tail = bincode::serialize(&project.poly_normals)
                        .map_err(|e| format!("Serialize poly_normals error: {}", e))?;
                    data.extend_from_slice(POLY_NORMAL_MAGIC);
                    data.extend_from_slice(&tail);
                }
                // Append-newest-LAST: FL4PPRM is the NEWEST section (landed with parametric
                // primitives, after FL4PNORM). Only scenes with at least one parametric poly
                // primitive emit it (byte-compat with every earlier save).
                if !project.poly_params.is_empty() {
                    let tail = bincode::serialize(&project.poly_params)
                        .map_err(|e| format!("Serialize poly_params error: {}", e))?;
                    data.extend_from_slice(POLY_PARAMS_MAGIC);
                    data.extend_from_slice(&tail);
                }
                // Append-newest-LAST: FL4PMODF is the NEWEST section (after FL4PPRM). Only
                // modifiers with an authored non-default flag emit records (byte-compat).
                if !project.poly_mod_flags.is_empty() {
                    let tail = bincode::serialize(&project.poly_mod_flags)
                        .map_err(|e| format!("Serialize poly_mod_flags error: {}", e))?;
                    data.extend_from_slice(POLY_MOD_FLAGS_MAGIC);
                    data.extend_from_slice(&tail);
                }
                // Append-newest-LAST: FL4PSMTH is the NEWEST section (after FL4PMODF). Only
                // Smooth modifiers with a channel AND a non-default falloff emit records.
                if !project.poly_smooth.is_empty() {
                    let tail = bincode::serialize(&project.poly_smooth)
                        .map_err(|e| format!("Serialize poly_smooth error: {}", e))?;
                    data.extend_from_slice(POLY_SMOOTH_MAGIC);
                    data.extend_from_slice(&tail);
                }
                // Append-newest-LAST: FL4PRMSH is the NEWEST section (after FL4PSMTH). Only
                // modifiers with an authored non-default Voxel Remesh param emit records, so a
                // scene that never touched a Remesh slider stays byte-identical to before.
                if !project.poly_remesh.is_empty() {
                    let tail = bincode::serialize(&project.poly_remesh)
                        .map_err(|e| format!("Serialize poly_remesh error: {}", e))?;
                    data.extend_from_slice(POLY_REMESH_MAGIC);
                    data.extend_from_slice(&tail);
                }
                if !project.poly_remesh_collapse.is_empty() {
                    let tail = bincode::serialize(&project.poly_remesh_collapse)
                        .map_err(|e| format!("Serialize poly_remesh_collapse error: {}", e))?;
                    data.extend_from_slice(POLY_REMESH_COLLAPSE_MAGIC);
                    data.extend_from_slice(&tail);
                }
                if !project.poly_remesh_skip_relax.is_empty() {
                    let tail = bincode::serialize(&project.poly_remesh_skip_relax)
                        .map_err(|e| format!("Serialize poly_remesh_skip_relax error: {}", e))?;
                    data.extend_from_slice(POLY_REMESH_SKIP_RELAX_MAGIC);
                    data.extend_from_slice(&tail);
                }
                if !project.poly_remesh_skip_seal.is_empty() {
                    let tail = bincode::serialize(&project.poly_remesh_skip_seal)
                        .map_err(|e| format!("Serialize poly_remesh_skip_seal error: {}", e))?;
                    data.extend_from_slice(POLY_REMESH_SKIP_SEAL_MAGIC);
                    data.extend_from_slice(&tail);
                }
                if !project.poly_remesh_drop_res.is_empty() {
                    let tail = bincode::serialize(&project.poly_remesh_drop_res)
                        .map_err(|e| format!("Serialize poly_remesh_drop_res error: {}", e))?;
                    data.extend_from_slice(POLY_REMESH_DROP_RES_MAGIC);
                    data.extend_from_slice(&tail);
                }
                if !project.bake_settings.is_empty() || !project.bake_images.is_empty() {
                    let tail = bincode::serialize(&BakeTail {
                        settings: project.bake_settings.clone(),
                        images: project.bake_images.clone(),
                    })
                    .map_err(|e| format!("Serialize bake_settings error: {}", e))?;
                    data.extend_from_slice(BAKE_SETTINGS_MAGIC);
                    data.extend_from_slice(&tail);
                }
                if !project.bake_bit16.is_empty() {
                    let tail = bincode::serialize(&project.bake_bit16)
                        .map_err(|e| format!("Serialize bake_bit16 error: {}", e))?;
                    data.extend_from_slice(BAKE_BIT16_MAGIC);
                    data.extend_from_slice(&tail);
                }
                if !project.bake_cage.is_empty() {
                    let tail = bincode::serialize(&project.bake_cage)
                        .map_err(|e| format!("Serialize bake_cage error: {}", e))?;
                    data.extend_from_slice(BAKE_CAGE_MAGIC);
                    data.extend_from_slice(&tail);
                }
                if !project.bake_udim.flags.is_empty() || !project.bake_udim.tiles.is_empty() {
                    let tail = bincode::serialize(&project.bake_udim)
                        .map_err(|e| format!("Serialize bake_udim error: {}", e))?;
                    data.extend_from_slice(BAKE_UDIM_MAGIC);
                    data.extend_from_slice(&tail);
                }
                if !project.bake_gpu_rays.is_empty() {
                    let tail = bincode::serialize(&project.bake_gpu_rays)
                        .map_err(|e| format!("Serialize bake_gpu_rays error: {}", e))?;
                    data.extend_from_slice(BAKE_GPU_RAY_MAGIC);
                    data.extend_from_slice(&tail);
                }
                if !project.poly_bevel_var.is_empty() {
                    let tail = bincode::serialize(&project.poly_bevel_var)
                        .map_err(|e| format!("Serialize poly_bevel_var error: {}", e))?;
                    data.extend_from_slice(POLY_BEVEL_VAR_MAGIC);
                    data.extend_from_slice(&tail);
                }
                if !project.poly_op_tags.is_empty() {
                    let tail = bincode::serialize(&project.poly_op_tags)
                        .map_err(|e| format!("Serialize poly_op_tags error: {}", e))?;
                    data.extend_from_slice(POLY_OP_TAGS_MAGIC);
                    data.extend_from_slice(&tail);
                }
                if !project.poly_bend.is_empty() {
                    let tail = bincode::serialize(&project.poly_bend)
                        .map_err(|e| format!("Serialize poly_bend error: {}", e))?;
                    data.extend_from_slice(POLY_BEND_MAGIC);
                    data.extend_from_slice(&tail);
                }
                if !project.poly_sculpt.is_empty() {
                    let tail = bincode::serialize(&project.poly_sculpt)
                        .map_err(|e| format!("Serialize poly_sculpt error: {}", e))?;
                    data.extend_from_slice(POLY_SCULPT_MAGIC);
                    data.extend_from_slice(&tail);
                }
                if !project.pivot_local_nodes.is_empty() {
                    let tail = bincode::serialize(&project.pivot_local_nodes)
                        .map_err(|e| format!("Serialize pivot_local_nodes error: {}", e))?;
                    data.extend_from_slice(PIVOT_LOCAL_MAGIC);
                    data.extend_from_slice(&tail);
                }
                atomic_write(path, &data)
            })
            .map_err(|e| format!("Thread spawn error: {}", e))?
            .join()
            .map_err(|payload| {
                format!(
                    "Serialize thread panicked: {}",
                    panic_payload_message(&*payload)
                )
            })?
    })
}

pub(crate) fn load_binary(path: &Path) -> Result<ProjectFile, String> {
    let data = std::fs::read(path).map_err(|e| format!("Read error: {}", e))?;
    // 8MB stack — same overflow risk as serialize (see load_library_entry pattern).
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .name("load-binary".into())
            .stack_size(8 * 1024 * 1024)
            .spawn_scoped(s, || {
                // Deserialize the ProjectFile blob from a Cursor so we learn EXACTLY where the blob
                // ends (verified: bincode::deserialize_from consumes the blob and no more → cursor
                // position == blob length). Anything after that is the optional FL4ORDER side-channel.
                let mut cursor = std::io::Cursor::new(&data[..]);
                let mut project: ProjectFile = bincode::deserialize_from(&mut cursor)
                    .map_err(|e| format!("Deserialize error: {}", e))?;
                // Read the self-delimiting FL4 tail sections. Each is an 8-byte magic followed by one
                // bincode value read via deserialize_from (position-tracked), so after each section the
                // cursor sits exactly at the next magic (or EOF). Sniff in a loop; an unknown byte
                // pattern or EOF ends the tail. Each iteration checks all magics (order-independent), so
                // a genuine pre-#2 file (child_orders-only, or bare) reads identically to before.
                loop {
                    let pos = cursor.position() as usize;
                    if data.len() >= pos + CHILD_ORDER_MAGIC.len()
                        && &data[pos..pos + CHILD_ORDER_MAGIC.len()] == CHILD_ORDER_MAGIC
                    {
                        cursor.set_position((pos + CHILD_ORDER_MAGIC.len()) as u64);
                        project.child_orders = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize child_order error: {}", e))?;
                    } else if data.len() >= pos + MATERIAL_MAP_MAGIC.len()
                        && &data[pos..pos + MATERIAL_MAP_MAGIC.len()] == MATERIAL_MAP_MAGIC
                    {
                        cursor.set_position((pos + MATERIAL_MAP_MAGIC.len()) as u64);
                        project.material_maps = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize material_maps error: {}", e))?;
                    } else if data.len() >= pos + MATERIAL_CHANNEL_MAGIC.len()
                        && &data[pos..pos + MATERIAL_CHANNEL_MAGIC.len()] == MATERIAL_CHANNEL_MAGIC
                    {
                        cursor.set_position((pos + MATERIAL_CHANNEL_MAGIC.len()) as u64);
                        project.material_channels = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize material_channels error: {}", e))?;
                    } else if data.len() >= pos + ENV_MAGIC.len()
                        && &data[pos..pos + ENV_MAGIC.len()] == ENV_MAGIC
                    {
                        cursor.set_position((pos + ENV_MAGIC.len()) as u64);
                        project.env = Some(bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize env record error: {}", e))?);
                    } else if data.len() >= pos + LIGHTVOL_MAGIC.len()
                        && &data[pos..pos + LIGHTVOL_MAGIC.len()] == LIGHTVOL_MAGIC
                    {
                        cursor.set_position((pos + LIGHTVOL_MAGIC.len()) as u64);
                        project.light_volumetrics = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize light_volumetrics error: {}", e))?;
                    } else if data.len() >= pos + LIGHTRT_MAGIC.len()
                        && &data[pos..pos + LIGHTRT_MAGIC.len()] == LIGHTRT_MAGIC
                    {
                        cursor.set_position((pos + LIGHTRT_MAGIC.len()) as u64);
                        project.light_rt = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize light_rt error: {}", e))?;
                    } else if data.len() >= pos + NODE_GRAPH_MAGIC.len()
                        && &data[pos..pos + NODE_GRAPH_MAGIC.len()] == NODE_GRAPH_MAGIC
                    {
                        cursor.set_position((pos + NODE_GRAPH_MAGIC.len()) as u64);
                        project.node_graphs = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize node_graphs error: {}", e))?;
                    } else if data.len() >= pos + MATERIAL_PANEL_MAGIC.len()
                        && &data[pos..pos + MATERIAL_PANEL_MAGIC.len()] == MATERIAL_PANEL_MAGIC
                    {
                        cursor.set_position((pos + MATERIAL_PANEL_MAGIC.len()) as u64);
                        project.material_panel_docs = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize material_panel_docs error: {}", e))?;
                    } else if data.len() >= pos + POLY_NORMAL_MAGIC.len()
                        && &data[pos..pos + POLY_NORMAL_MAGIC.len()] == POLY_NORMAL_MAGIC
                    {
                        cursor.set_position((pos + POLY_NORMAL_MAGIC.len()) as u64);
                        project.poly_normals = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize poly_normals error: {}", e))?;
                    } else if data.len() >= pos + POLY_PARAMS_MAGIC.len()
                        && &data[pos..pos + POLY_PARAMS_MAGIC.len()] == POLY_PARAMS_MAGIC
                    {
                        cursor.set_position((pos + POLY_PARAMS_MAGIC.len()) as u64);
                        project.poly_params = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize poly_params error: {}", e))?;
                    } else if data.len() >= pos + POLY_MOD_FLAGS_MAGIC.len()
                        && &data[pos..pos + POLY_MOD_FLAGS_MAGIC.len()] == POLY_MOD_FLAGS_MAGIC
                    {
                        cursor.set_position((pos + POLY_MOD_FLAGS_MAGIC.len()) as u64);
                        project.poly_mod_flags = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize poly_mod_flags error: {}", e))?;
                    } else if data.len() >= pos + POLY_SMOOTH_MAGIC.len()
                        && &data[pos..pos + POLY_SMOOTH_MAGIC.len()] == POLY_SMOOTH_MAGIC
                    {
                        cursor.set_position((pos + POLY_SMOOTH_MAGIC.len()) as u64);
                        project.poly_smooth = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize poly_smooth error: {}", e))?;
                    } else if data.len() >= pos + POLY_REMESH_MAGIC.len()
                        && &data[pos..pos + POLY_REMESH_MAGIC.len()] == POLY_REMESH_MAGIC
                    {
                        // Dual-decode: five-field records (with union_islands) first; on failure
                        // re-read as the pre-toggle four-field layout (union_islands = false).
                        let section_start = (pos + POLY_REMESH_MAGIC.len()) as u64;
                        cursor.set_position(section_start);
                        match bincode::deserialize_from::<_, Vec<crate::project::PolyRemeshRecord>>(
                            &mut cursor,
                        ) {
                            Ok(v) => project.poly_remesh = v,
                            Err(_) => {
                                cursor.set_position(section_start);
                                let old: Vec<crate::project::PolyRemeshRecordV0> =
                                    bincode::deserialize_from(&mut cursor).map_err(|e| {
                                        format!("Deserialize poly_remesh error: {}", e)
                                    })?;
                                project.poly_remesh =
                                    old.into_iter().map(crate::project::PolyRemeshRecord::from).collect();
                            }
                        }
                    } else if data.len() >= pos + POLY_REMESH_COLLAPSE_MAGIC.len()
                        && &data[pos..pos + POLY_REMESH_COLLAPSE_MAGIC.len()]
                            == POLY_REMESH_COLLAPSE_MAGIC
                    {
                        cursor.set_position((pos + POLY_REMESH_COLLAPSE_MAGIC.len()) as u64);
                        project.poly_remesh_collapse = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize poly_remesh_collapse error: {}", e))?;
                    } else if data.len() >= pos + POLY_REMESH_SKIP_RELAX_MAGIC.len()
                        && &data[pos..pos + POLY_REMESH_SKIP_RELAX_MAGIC.len()]
                            == POLY_REMESH_SKIP_RELAX_MAGIC
                    {
                        cursor.set_position((pos + POLY_REMESH_SKIP_RELAX_MAGIC.len()) as u64);
                        project.poly_remesh_skip_relax = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize poly_remesh_skip_relax error: {}", e))?;
                    } else if data.len() >= pos + POLY_REMESH_SKIP_SEAL_MAGIC.len()
                        && &data[pos..pos + POLY_REMESH_SKIP_SEAL_MAGIC.len()]
                            == POLY_REMESH_SKIP_SEAL_MAGIC
                    {
                        cursor.set_position((pos + POLY_REMESH_SKIP_SEAL_MAGIC.len()) as u64);
                        project.poly_remesh_skip_seal = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize poly_remesh_skip_seal error: {}", e))?;
                    } else if data.len() >= pos + POLY_REMESH_DROP_RES_MAGIC.len()
                        && &data[pos..pos + POLY_REMESH_DROP_RES_MAGIC.len()]
                            == POLY_REMESH_DROP_RES_MAGIC
                    {
                        cursor.set_position((pos + POLY_REMESH_DROP_RES_MAGIC.len()) as u64);
                        project.poly_remesh_drop_res = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize poly_remesh_drop_res error: {}", e))?;
                    } else if data.len() >= pos + BAKE_SETTINGS_MAGIC.len()
                        && &data[pos..pos + BAKE_SETTINGS_MAGIC.len()] == BAKE_SETTINGS_MAGIC
                    {
                        cursor.set_position((pos + BAKE_SETTINGS_MAGIC.len()) as u64);
                        let (settings, images) = read_bake_tail(&mut cursor, &data)?;
                        project.bake_settings = settings;
                        project.bake_images = images;
                    } else if data.len() >= pos + BAKE_BIT16_MAGIC.len()
                        && &data[pos..pos + BAKE_BIT16_MAGIC.len()] == BAKE_BIT16_MAGIC
                    {
                        cursor.set_position((pos + BAKE_BIT16_MAGIC.len()) as u64);
                        project.bake_bit16 = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize bake_bit16 error: {}", e))?;
                    } else if data.len() >= pos + BAKE_CAGE_MAGIC.len()
                        && &data[pos..pos + BAKE_CAGE_MAGIC.len()] == BAKE_CAGE_MAGIC
                    {
                        cursor.set_position((pos + BAKE_CAGE_MAGIC.len()) as u64);
                        project.bake_cage = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize bake_cage error: {}", e))?;
                    } else if data.len() >= pos + BAKE_UDIM_MAGIC.len()
                        && &data[pos..pos + BAKE_UDIM_MAGIC.len()] == BAKE_UDIM_MAGIC
                    {
                        cursor.set_position((pos + BAKE_UDIM_MAGIC.len()) as u64);
                        project.bake_udim = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize bake_udim error: {}", e))?;
                    } else if data.len() >= pos + BAKE_GPU_RAY_MAGIC.len()
                        && &data[pos..pos + BAKE_GPU_RAY_MAGIC.len()] == BAKE_GPU_RAY_MAGIC
                    {
                        cursor.set_position((pos + BAKE_GPU_RAY_MAGIC.len()) as u64);
                        project.bake_gpu_rays = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize bake_gpu_rays error: {}", e))?;
                    } else if data.len() >= pos + POLY_BEVEL_VAR_MAGIC.len()
                        && &data[pos..pos + POLY_BEVEL_VAR_MAGIC.len()] == POLY_BEVEL_VAR_MAGIC
                    {
                        cursor.set_position((pos + POLY_BEVEL_VAR_MAGIC.len()) as u64);
                        project.poly_bevel_var = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize poly_bevel_var error: {}", e))?;
                    } else if data.len() >= pos + POLY_OP_TAGS_MAGIC.len()
                        && &data[pos..pos + POLY_OP_TAGS_MAGIC.len()] == POLY_OP_TAGS_MAGIC
                    {
                        cursor.set_position((pos + POLY_OP_TAGS_MAGIC.len()) as u64);
                        project.poly_op_tags = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize poly_op_tags error: {}", e))?;
                    } else if data.len() >= pos + POLY_BEND_MAGIC.len()
                        && &data[pos..pos + POLY_BEND_MAGIC.len()] == POLY_BEND_MAGIC
                    {
                        cursor.set_position((pos + POLY_BEND_MAGIC.len()) as u64);
                        project.poly_bend = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize poly_bend error: {}", e))?;
                    } else if data.len() >= pos + POLY_SCULPT_MAGIC.len()
                        && &data[pos..pos + POLY_SCULPT_MAGIC.len()] == POLY_SCULPT_MAGIC
                    {
                        cursor.set_position((pos + POLY_SCULPT_MAGIC.len()) as u64);
                        project.poly_sculpt = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize poly_sculpt error: {}", e))?;
                    } else if data.len() >= pos + PIVOT_LOCAL_MAGIC.len()
                        && &data[pos..pos + PIVOT_LOCAL_MAGIC.len()] == PIVOT_LOCAL_MAGIC
                    {
                        cursor.set_position((pos + PIVOT_LOCAL_MAGIC.len()) as u64);
                        project.pivot_local_nodes = bincode::deserialize_from(&mut cursor)
                            .map_err(|e| format!("Deserialize pivot_local_nodes error: {}", e))?;
                    } else {
                        break;
                    }
                }
                Ok(project)
            })
            .map_err(|e| format!("Thread spawn error: {}", e))?
            .join()
            .map_err(|payload| {
                format!(
                    "Deserialize thread panicked: {}",
                    panic_payload_message(&*payload)
                )
            })?
    })
}

pub(crate) fn save_json(path: &Path, project: &ProjectFile) -> Result<(), String> {
    let json = serde_json::to_string_pretty(project).map_err(|e| format!("JSON error: {}", e))?;
    atomic_write(path, json.as_bytes())
}

pub(crate) fn load_json(path: &Path) -> Result<ProjectFile, String> {
    let data = std::fs::read_to_string(path).map_err(|e| format!("Read error: {}", e))?;
    serde_json::from_str(&data).map_err(|e| format!("JSON parse error: {}", e))
}

#[cfg(test)]
mod atomic_write_tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "azra-atomic-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn replace_is_complete_and_leaves_no_temp() {
        let dir = scratch();
        let path = dir.join("default_scene.fl4");
        atomic_write(&path, b"ORIGINAL").unwrap();
        atomic_write(&path, b"REPLACED-BYTES").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"REPLACED-BYTES");
        assert!(!sibling_tmp(&path).exists(), "temp must not survive a successful rename");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_write_does_not_touch_existing_bytes() {
        let dir = scratch();
        let path = dir.join("default_scene.fl4");
        atomic_write(&path, b"ORIGINAL-BYTES").unwrap();

        let bad = dir.join("missing").join("default_scene.fl4");
        let err = atomic_write(&bad, b"SHOULD-NOT-LAND").unwrap_err();
        assert!(
            err.contains("Create ") && (err.contains("os error") || err.contains("No such") || err.contains("directory")),
            "failure must name the OS error, got {err}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"ORIGINAL-BYTES");
        assert!(!bad.exists());
        assert!(!sibling_tmp(&bad).exists(), "failed write must remove its temp");

        // A directory sitting at the destination: rename fails, and the directory
        // is not replaced by a truncated file.
        let slot = dir.join("slot");
        std::fs::create_dir(&slot).unwrap();
        let err = atomic_write(&slot, b"NEWDATA").unwrap_err();
        assert!(err.contains("Rename "), "{err}");
        assert!(slot.is_dir());
        assert!(!sibling_tmp(&slot).exists());

        let blocker = dir.join("not-a-directory");
        std::fs::write(&blocker, b"x").unwrap();
        let nested = blocker.join("default_scene.fl4");
        let err = crate::project::ensure_default_scene_dir(&nested).unwrap_err();
        assert!(err.contains("Could not create"), "{err}");
        assert!(err.contains("not-a-directory"), "{err}");
        assert_eq!(std::fs::read(&blocker).unwrap(), b"x");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
