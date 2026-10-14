//! P5 — presets as DATA (MATERIAL_PANEL_SPEC.md §4-P5 as REVISED by §9, owner D1).
//!
//! A preset IS a panel doc (`MaterialPreset { name, doc }`): channel set + values
//! + shader type + texture space. NO pipeline permutations, no pre-warm, no
//! draw-call sorting (§9: materials are data, not pipelines; the cost readout
//! from P0 already reports the honest cost).
//!
//! **Image layers in presets:** layer stacks are stored STRUCTURALLY (blend,
//! opacity, placement, adjust) but with EMPTY image slots (`tex_id = 0`) —
//! texture-array slot ids are per-project and would dangle in any other project.
//! `sanitize_doc_for_preset` enforces this on save; applying such a preset gives
//! the user the stack shape with "Load…" ready per layer.
//!
//! **GraphDriven channels in presets:** a node graph is per-material and cannot
//! ride a preset; on save they are frozen to their current engine `Value`
//! (`sanitize_doc_for_preset`).
//!
//! Reuse audit (CLAUDE.md rule): searched for existing preset machinery — found
//! `PbrMaterial::preset()` / `preset_names()` (core_math/pbr_material.rs:326+),
//! REUSED as the value source for the Glass / Skin built-ins; user-blob JSON
//! persistence pattern found at `ui2/persist.rs` (ui2_layout.json — versioned
//! struct, `dirs::config_dir()/Azra/`, fail-soft load) and mirrored here. No
//! existing "material panel preset" type found — this module is new.

use serde::{Deserialize, Serialize};

use super::lower::{channel_value_of, lower_to_material, ALL_KINDS};
use super::model::*;
use core_math::pbr_material::PbrMaterial;
use core_math::primitives::MaterialId;

/// A named, self-contained panel doc. Serialized as JSON only (user blob) —
/// never bincode, never in the .fl4.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct MaterialPreset {
    pub name: String,
    pub doc: MaterialPanelDoc,
}

// ==================================================================
// Built-ins (data, derived from the engine's own value sources)
// ==================================================================

/// Helper: channels for `kinds`, each seeded with `mat`'s current value.
fn channels_from(mat: &PbrMaterial, kinds: &[ChannelKind]) -> Vec<Channel> {
    kinds
        .iter()
        .map(|&k| Channel::value(k, channel_value_of(k, mat)))
        .collect()
}

/// Diffuse Only — BaseColor only, from the default material.
pub(crate) fn preset_diffuse_only() -> MaterialPreset {
    let m = PbrMaterial::default();
    MaterialPreset {
        name: "Diffuse Only".into(),
        doc: MaterialPanelDoc {
            next_layer_id: 1,
            generator_mode: 0,
            shader_type: 0,
            texture_space: TextureSpace::default(),
            channels: channels_from(&m, &[ChannelKind::BaseColor]),
            bake_res: 256,
        },
    }
}

/// Standard PBR — the D1 MAIN channel set. IS `default_for_new` (one source).
pub(crate) fn preset_standard_pbr() -> MaterialPreset {
    MaterialPreset { name: "Standard PBR".into(), doc: MaterialPanelDoc::default_for_new() }
}

/// Full PBR — all 18 channels, values from the default material.
pub(crate) fn preset_full_pbr() -> MaterialPreset {
    let m = PbrMaterial::default();
    MaterialPreset {
        name: "Full PBR".into(),
        doc: MaterialPanelDoc {
            next_layer_id: 1,
            generator_mode: 0,
            shader_type: 0,
            texture_space: TextureSpace::default(),
            channels: channels_from(&m, &ALL_KINDS),
        bake_res: 256,
        },
    }
}

/// Glass — values from `PbrMaterial::preset("glass")` (roughness 0.02,
/// opacity 0.15, IOR 1.5, ShaderType::Glass). Channel set: the fields that
/// preset actually authors + the glass-model extras.
pub(crate) fn preset_glass() -> MaterialPreset {
    let m = PbrMaterial::preset("glass");
    MaterialPreset {
        name: "Glass".into(),
        doc: MaterialPanelDoc {
            next_layer_id: 1,
            generator_mode: 0,
            shader_type: m.shader_type.as_u32(),
            texture_space: TextureSpace::default(),
            channels: channels_from(
                &m,
                &[
                    ChannelKind::BaseColor,
                    ChannelKind::Roughness,
                    ChannelKind::Opacity,
                    ChannelKind::Ior,
                    ChannelKind::GlassTint,
                ],
            ),
        bake_res: 256,
        },
    }
}

/// Skin / SSS — values from `PbrMaterial::preset("subsurface")` (sss_radius
/// 0.3, warm sss_color, ShaderType::Subsurface).
pub(crate) fn preset_skin() -> MaterialPreset {
    let m = PbrMaterial::preset("subsurface");
    MaterialPreset {
        name: "Skin / SSS".into(),
        doc: MaterialPanelDoc {
            next_layer_id: 1,
            generator_mode: 0,
            shader_type: m.shader_type.as_u32(),
            texture_space: TextureSpace::default(),
            channels: channels_from(
                &m,
                &[
                    ChannelKind::BaseColor,
                    ChannelKind::Roughness,
                    ChannelKind::Normal,
                    ChannelKind::SssRadius,
                    ChannelKind::SssColor,
                ],
            ),
        bake_res: 256,
        },
    }
}

/// VFX — emissive / opacity-forward set (ShaderType::VFX).
pub(crate) fn preset_vfx() -> MaterialPreset {
    let mut m = PbrMaterial::default();
    m.shader_type = core_math::pbr_material::ShaderType::VFX;
    m.emission_intensity = 1.0;
    m.emission_color = glam::Vec3::ONE;
    MaterialPreset {
        name: "VFX".into(),
        doc: MaterialPanelDoc {
            next_layer_id: 1,
            generator_mode: 0,
            shader_type: m.shader_type.as_u32(),
            texture_space: TextureSpace::default(),
            channels: channels_from(
                &m,
                &[
                    ChannelKind::BaseColor,
                    ChannelKind::Opacity,
                    ChannelKind::Emission,
                    ChannelKind::Roughness,
                ],
            ),
        bake_res: 256,
        },
    }
}

/// Advanced — full lobe set (clearcoat / sheen / anisotropy / SSS extras).
pub(crate) fn preset_advanced() -> MaterialPreset {
    let mut m = PbrMaterial::default();
    m.shader_type = core_math::pbr_material::ShaderType::Advanced;
    MaterialPreset {
        name: "Advanced".into(),
        doc: MaterialPanelDoc {
            next_layer_id: 1,
            generator_mode: 0,
            shader_type: m.shader_type.as_u32(),
            texture_space: TextureSpace::default(),
            channels: channels_from(&m, &ALL_KINDS),
        bake_res: 256,
        },
    }
}

/// All built-ins, in menu order. SHADER TYPES for create (not quality tiers).
/// Built fresh (cheap Vec of small docs) — not deletable, not persisted.
pub(crate) fn builtin_presets() -> Vec<MaterialPreset> {
    vec![
        preset_diffuse_only(),
        preset_standard_pbr(),
        preset_full_pbr(),
        preset_glass(),
        preset_skin(),
        preset_vfx(),
        preset_advanced(),
    ]
}

// ==================================================================
// Destructive-apply gate (pure)
// ==================================================================

/// True when replacing `doc` with a preset destroys authored content the user
/// cannot get back with a slider: any `GraphDriven` channel (node subgraph) or
/// any layer holding a LOADED image (`tex_id != 0`). Plain values and empty
/// layer shells are freely replaceable.
pub(crate) fn apply_is_destructive(doc: &MaterialPanelDoc) -> bool {
    doc.channels.iter().any(|c| {
        matches!(c.state, ChannelState::GraphDriven { .. })
            || c.layers
                .iter()
                .any(|l| matches!(l.content, LayerContent::Image { tex_id, .. } if tex_id != 0))
    })
}

/// The preset name the current doc exactly matches, if any ("—" otherwise in
/// the UI). Exact doc equality — any edit after applying reverts to "—".
pub(crate) fn matching_preset_name<'a>(
    doc: &MaterialPanelDoc,
    builtins: &'a [MaterialPreset],
    user: &'a [MaterialPreset],
) -> Option<&'a str> {
    builtins
        .iter()
        .chain(user.iter())
        .find(|p| p.doc == *doc)
        .map(|p| p.name.as_str())
}

// ==================================================================
// Save sanitization (pure)
// ==================================================================

/// Prepare the CURRENT material doc for storage as a preset:
/// - `GraphDriven` channels freeze to `Value` from the live material (graphs
///   are per-material; the value is the honest portable part);
/// - image layers keep their structure but drop project-local `tex_id`s to 0
///   (empty slot — see module header).
pub(crate) fn sanitize_doc_for_preset(doc: &MaterialPanelDoc, mat: &PbrMaterial) -> MaterialPanelDoc {
    let mut out = doc.clone();
    for ch in &mut out.channels {
        if matches!(ch.state, ChannelState::GraphDriven { .. }) {
            ch.state = ChannelState::Value(channel_value_of(ch.kind, mat));
            ch.layers.clear();
        }
        for l in &mut ch.layers {
            if let LayerContent::Image { tex_id, .. } = &mut l.content {
                *tex_id = 0;
            }
            if let Some(LayerContent::Image { tex_id, .. }) = l.mask.primary_content_mut() {
                *tex_id = 0;
            }
        }
    }
    out
}

// ==================================================================
// User preset persistence — %APPDATA%/Azra/material_presets.json
// (the ui2_layout.json pattern: dirs::config_dir()/Azra, versioned, fail-soft)
// ==================================================================

const PRESET_FILE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct PresetFile {
    v: u32,
    presets: Vec<MaterialPreset>,
}

fn presets_path() -> Option<std::path::PathBuf> {
    Some(dirs::config_dir()?.join("Azra").join("material_presets.json"))
}

/// Fail-soft load: missing file / bad JSON / future version → empty list, never
/// an error dialog (user blob, same policy as ui2_layout.json).
pub(crate) fn load_user_presets() -> Vec<MaterialPreset> {
    let Some(path) = presets_path() else { return Vec::new() };
    load_user_presets_from(&path)
}

pub(crate) fn load_user_presets_from(path: &std::path::Path) -> Vec<MaterialPreset> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    match serde_json::from_str::<PresetFile>(&text) {
        Ok(f) if f.v <= PRESET_FILE_VERSION => f.presets,
        Ok(f) => {
            eprintln!("[Presets] material_presets.json v{} is newer than v{PRESET_FILE_VERSION} — ignoring", f.v);
            Vec::new()
        }
        Err(e) => {
            eprintln!("[Presets] material_presets.json unreadable — starting empty ({e})");
            Vec::new()
        }
    }
}

pub(crate) fn save_user_presets(presets: &[MaterialPreset]) {
    let Some(path) = presets_path() else { return };
    save_user_presets_to(&path, presets);
}

pub(crate) fn save_user_presets_to(path: &std::path::Path, presets: &[MaterialPreset]) {
    let file = PresetFile { v: PRESET_FILE_VERSION, presets: presets.to_vec() };
    let Ok(json) = serde_json::to_string_pretty(&file) else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(path, json) {
        eprintln!("[Presets] failed to write {}: {e}", path.display());
    }
}

// ==================================================================
// App-side: apply / save / create-with-preset
// ==================================================================

impl crate::AzraApp {
    /// The user preset list, loaded lazily once per session (RUNTIME cache;
    /// the file on disk is the truth between sessions).
    pub(crate) fn mp_user_presets(&mut self) -> &[MaterialPreset] {
        if self.ui.mp_user_presets.is_none() {
            self.ui.mp_user_presets = Some(load_user_presets());
        }
        self.ui.mp_user_presets.as_deref().unwrap_or(&[])
    }

    /// Apply a preset onto an existing material — ONE undo entry. Replaces the
    /// stored doc and lowers it (values + shader type + texture space) through
    /// the exact P2 commit path (`mp_commit_doc_edit`: transaction_snapshot_only,
    /// lower_to_material, Master-socket sync, rich-channel lowering, P2 dirty
    /// propagation). Caller is responsible for the destructive-apply gate
    /// (`apply_is_destructive` on the CURRENT doc → `mp_pending_preset` confirm).
    pub(crate) fn mp_apply_preset(&mut self, mid: MaterialId, preset: &MaterialPreset) {
        // Applying over GraphDriven content: the preset doc simply has no
        // GraphDriven channels, but the material's stale subgraph must not keep
        // driving sockets the panel now owns — prune every channel the doc
        // replaces (same surgery as revert; no-op when no graph exists).
        let had_graph_driven = self
            .ui
            .material_panel_docs
            .get(&mid.0)
            .map(|d| d.channels.iter().any(|c| matches!(c.state, ChannelState::GraphDriven { .. })))
            .unwrap_or(false);
        self.mp_commit_doc_edit(mid, preset.doc.clone(), "Apply material preset", false);
        if had_graph_driven {
            // The commit above snapshotted undo; do the graph surgery inside
            // the same frame so Ctrl+Z restores both together.
            for &kind in ALL_KINDS.iter() {
                self.mp_prune_channel_subgraph(mid, kind);
            }
            self.enqueue_material_node_graph_bake(mid);
        }
        self.toast(&format!("Preset '{}' applied", preset.name));
    }

    /// "Save as preset…" confirmed: sanitize the current doc (see
    /// `sanitize_doc_for_preset`), replace-or-append by name in the user list,
    /// persist to disk. Built-in names are shadowable in the user list but the
    /// built-ins themselves are never touched.
    pub(crate) fn mp_save_preset(&mut self, mid: MaterialId, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let Some(mat) = self.scene.scene_graph.material_by_id(mid).map(|(_, m)| m.clone())
        else {
            return;
        };
        let doc = self.mp_current_doc(mid, &mat);
        let preset = MaterialPreset {
            name: name.to_string(),
            doc: sanitize_doc_for_preset(&doc, &mat),
        };
        let _ = self.mp_user_presets(); // ensure loaded
        let list = self.ui.mp_user_presets.get_or_insert_with(Vec::new);
        match list.iter_mut().find(|p| p.name == preset.name) {
            Some(existing) => *existing = preset,
            None => list.push(preset),
        }
        save_user_presets(list);
        self.toast(&format!("Preset '{name}' saved"));
    }

    /// Delete a user preset (confirmed) and persist. Built-ins are not in this
    /// list and cannot reach here.
    pub(crate) fn mp_delete_user_preset(&mut self, index: usize) {
        let Some(list) = self.ui.mp_user_presets.as_mut() else { return };
        if index >= list.len() {
            return;
        }
        let name = list.remove(index).name;
        save_user_presets(list);
        self.toast(&format!("Preset '{name}' deleted"));
    }

    /// Ctrl+M popup "+ New Material" with a preset (owner D1): create the
    /// material FROM the preset's doc — the doc is stored, lowered onto the new
    /// `PbrMaterial`, and the material is assigned to the selection. One undo
    /// entry (same transaction shape as the popup's existing create path).
    pub(crate) fn create_material_with_preset(&mut self, name: &str, preset: &MaterialPreset) {
        let mut seed = PbrMaterial::default();
        lower_to_material(&preset.doc, &mut seed);
        let doc = preset.doc.clone();
        let name = name.to_string();
        self.transaction("Create Material", |s| {
            let mid = s.scene.scene_graph.add_material(&name, seed);
            s.ui
                .material_node_graphs
                .insert(mid.0, crate::shader_nodes::NodeGraph::default());
            s.ui.material_panel_docs.insert(mid.0, doc);
            if s.apply_material_to_selection(mid.0) {
                s.toast(&format!("{} created and assigned", name));
            } else {
                s.toast(&format!("{} created — select a prim/body first to assign", name));
            }
        });
    }
}

// ==================================================================
// Tests
// ==================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(p: &MaterialPreset) -> Vec<ChannelKind> {
        p.doc.channels.iter().map(|c| c.kind).collect()
    }

    /// Built-ins produce the DOCUMENTED channel sets.
    #[test]
    fn builtin_channel_sets() {
        assert_eq!(kinds(&preset_diffuse_only()), vec![ChannelKind::BaseColor]);
        assert_eq!(
            kinds(&preset_standard_pbr()),
            vec![
                ChannelKind::BaseColor,
                ChannelKind::Roughness,
                ChannelKind::Metallic,
                ChannelKind::Normal,
                ChannelKind::Displacement,
            ],
            "Standard PBR must equal default_for_new (D1 main set)"
        );
        assert_eq!(preset_full_pbr().doc.channels.len(), 18);
        let glass = preset_glass();
        assert_eq!(glass.doc.shader_type, core_math::pbr_material::ShaderType::Glass.as_u32());
        assert!(kinds(&glass).contains(&ChannelKind::Ior));
        assert!(kinds(&glass).contains(&ChannelKind::GlassTint));
        let skin = preset_skin();
        assert_eq!(skin.doc.shader_type, core_math::pbr_material::ShaderType::Subsurface.as_u32());
        assert!(kinds(&skin).contains(&ChannelKind::SssRadius));
        assert!(kinds(&skin).contains(&ChannelKind::SssColor));
        let vfx = preset_vfx();
        assert_eq!(vfx.doc.shader_type, core_math::pbr_material::ShaderType::VFX.as_u32());
        assert!(kinds(&vfx).contains(&ChannelKind::Emission));
        assert_eq!(
            preset_advanced().doc.shader_type,
            core_math::pbr_material::ShaderType::Advanced.as_u32()
        );
        // Displacement defaults to 16-bit (owner: height maps need the headroom).
        let std = preset_standard_pbr();
        let disp = std
            .doc
            .channel(ChannelKind::Displacement)
            .expect("displacement in standard PBR");
        assert_eq!(disp.bit_depth, ChannelBitDepth::Bit16);
    }

    /// Values come from the PbrMaterial::preset family, not invented.
    #[test]
    fn glass_values_match_engine_preset() {
        let engine = PbrMaterial::preset("glass");
        let p = preset_glass();
        assert_eq!(
            p.doc.channel(ChannelKind::Roughness).map(|c| c.state),
            Some(ChannelState::Value(ChannelValue::Scalar(engine.roughness)))
        );
        assert_eq!(
            p.doc.channel(ChannelKind::Ior).map(|c| c.state),
            Some(ChannelState::Value(ChannelValue::Scalar(engine.ior)))
        );
        assert_eq!(
            p.doc.channel(ChannelKind::Opacity).map(|c| c.state),
            Some(ChannelState::Value(ChannelValue::Scalar(engine.opacity)))
        );
    }

    /// Preset → lower → read back: applying the preset doc onto a material
    /// yields the preset's values (apply/derive round-trip).
    #[test]
    fn apply_lower_roundtrip() {
        for p in builtin_presets() {
            let mut mat = PbrMaterial::default();
            lower_to_material(&p.doc, &mut mat);
            assert_eq!(mat.shader_type.as_u32(), p.doc.shader_type, "{}", p.name);
            for ch in &p.doc.channels {
                // Map-only channels (Bump/Displacement) have no scalar readback.
                if matches!(ch.kind, ChannelKind::Bump | ChannelKind::Displacement) {
                    continue;
                }
                if let ChannelState::Value(v) = ch.state {
                    assert_eq!(
                        channel_value_of(ch.kind, &mat),
                        v,
                        "{}: {:?} did not round-trip",
                        p.name,
                        ch.kind
                    );
                }
            }
        }
    }

    /// User preset JSON file round-trip (temp dir).
    #[test]
    fn user_preset_file_roundtrip() {
        let dir = std::env::temp_dir().join("azra_preset_test");
        let path = dir.join("material_presets.json");
        let _ = std::fs::remove_file(&path);
        let presets = vec![
            MaterialPreset { name: "Empty".into(), doc: MaterialPanelDoc::default() },
            preset_glass(),
        ];
        save_user_presets_to(&path, &presets);
        let back = load_user_presets_from(&path);
        assert_eq!(back, presets);
        // Fail-soft: garbage file loads as empty.
        std::fs::write(&path, "{not json").unwrap();
        assert!(load_user_presets_from(&path).is_empty());
        // Missing file loads as empty.
        let _ = std::fs::remove_file(&path);
        assert!(load_user_presets_from(&path).is_empty());
    }

    /// Destructive-apply gate: GraphDriven or loaded image → confirm required.
    #[test]
    fn destructive_gate() {
        let mut doc = MaterialPanelDoc::default_for_new();
        assert!(!apply_is_destructive(&doc), "plain values are not destructive");
        // GraphDriven channel → destructive.
        doc.channels[0].state = ChannelState::GraphDriven { node: 7 };
        assert!(apply_is_destructive(&doc));
        // Loaded image layer → destructive; empty slot (tex_id 0) is not.
        let mut doc2 = MaterialPanelDoc::default_for_new();
        doc2.channels[0].state = ChannelState::Layered;
        doc2.channels[0].layers = vec![Layer {
            id: 0,
            drives: Vec::new(),
            content: LayerContent::Image { tex_id: 0, strength: 1.0 },
            mask: MaskStack::default(),
            blend: BlendMode::Normal,
            opacity: 1.0,
            visible: true,
            placement: Placement::default(),
            adjust: Adjust::default(),
        }];
        assert!(!apply_is_destructive(&doc2), "empty image slot is replaceable");
        if let LayerContent::Image { tex_id, .. } = &mut doc2.channels[0].layers[0].content {
            *tex_id = 3;
        }
        assert!(apply_is_destructive(&doc2), "loaded image must gate");
    }

    /// Save sanitization: GraphDriven freezes to Value; image slots empty out.
    #[test]
    fn sanitize_for_preset() {
        let mat = PbrMaterial::preset("glass");
        let mut doc = MaterialPanelDoc::default_for_new();
        doc.channels[1].state = ChannelState::GraphDriven { node: 3 }; // Roughness
        doc.channels[0].state = ChannelState::Layered;
        doc.channels[0].layers = vec![Layer {
            id: 0,
            drives: Vec::new(),
            content: LayerContent::Image { tex_id: 9, strength: 0.8 },
            mask: MaskStack::default(),
            blend: BlendMode::Multiply,
            opacity: 0.5,
            visible: true,
            placement: Placement::default(),
            adjust: Adjust::default(),
        }];
        let s = sanitize_doc_for_preset(&doc, &mat);
        assert_eq!(
            s.channels[1].state,
            ChannelState::Value(ChannelValue::Scalar(mat.roughness)),
            "GraphDriven freezes to the live value"
        );
        match s.channels[0].layers[0].content {
            LayerContent::Image { tex_id, strength } => {
                assert_eq!(tex_id, 0, "image slots stored EMPTY (per-project ids)");
                assert_eq!(strength, 0.8, "structure survives");
            }
            _ => panic!("layer content changed shape"),
        }
        assert_eq!(s.channels[0].layers[0].blend, BlendMode::Multiply);
        assert!(!apply_is_destructive(&s), "a sanitized preset is never gate-worthy");
    }

    /// Doc↔preset name matching (the header combo's current label).
    #[test]
    fn matching_name() {
        let builtins = builtin_presets();
        let user = vec![MaterialPreset { name: "Empty".into(), doc: MaterialPanelDoc::default() }];
        assert_eq!(
            matching_preset_name(&MaterialPanelDoc::default_for_new(), &builtins, &user),
            Some("Standard PBR")
        );
        assert_eq!(
            matching_preset_name(&MaterialPanelDoc::default(), &builtins, &user),
            Some("Empty")
        );
        let mut edited = MaterialPanelDoc::default_for_new();
        edited.channels[1].state = ChannelState::Value(ChannelValue::Scalar(0.123));
        assert_eq!(matching_preset_name(&edited, &builtins, &user), None);
    }
}
