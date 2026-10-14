//! Macro recording, playback, save/load — shared actions.

use crate::*;

impl AzraApp {
    // ---- Macro Recording ----
    pub(crate) fn toggle_macro_recording(&mut self) {
        if self.macro_recording {
            self.macro_recording = false;
            let count = self.macro_buffer.len();
            self.toast(&format!("Macro recorded ({} commands)", count));
        } else {
            self.macro_buffer.clear();
            self.macro_recording = true;
            self.toast("Recording macro...");
        }
    }

    pub(crate) fn play_macro(&mut self) {
        if self.macro_buffer.is_empty() {
            self.toast("No macro recorded");
            return;
        }
        let cmds = self.macro_buffer.clone();
        let count = cmds.len();
        let mut skipped = 0usize;
        for cmd in cmds {
            // Bounds-check commands that reference prim uids — silently skip deleted prims
            let valid = match &cmd {
                AppCommand::SelectSingle(uid) | AppCommand::SetVisible(uid, _)
                | AppCommand::SetLocked(uid, _) | AppCommand::SetBooleanOp(uid, _)
                | AppCommand::SetPosition(uid, _) | AppCommand::SetRotation(uid, _)
                | AppCommand::SetScale(uid, _) => self.prim_index_by_uid(*uid).is_some(),
                _ => true,
            };
            if valid {
                self.commands.push(cmd);
            } else {
                skipped += 1;
            }
        }
        if skipped > 0 {
            self.toast(&format!("Played macro ({} cmds, {} skipped: prim deleted)", count, skipped));
        } else {
            self.toast(&format!("Playing macro ({} commands)", count));
        }
    }

    pub(crate) fn save_macro_dialog(&mut self) {
        if self.macro_buffer.is_empty() {
            self.toast("No macro to save");
            return;
        }
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("AZRA Macro", &["flmacro"])
            .set_title("Save Macro")
            .save_file()
        {
            let json = Self::macro_to_json(&self.macro_buffer);
            match std::fs::write(&path, json) {
                Ok(_) => self.toast(&format!("Macro saved to {}", path.display())),
                Err(e) => self.toast(&format!("Save failed: {}", e)),
            }
        }
    }

    pub(crate) fn load_macro_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("AZRA Macro", &["flmacro"])
            .set_title("Load Macro")
            .pick_file()
        {
            match std::fs::read_to_string(&path) {
                Ok(json) => {
                    match Self::macro_from_json(&json) {
                        Some(cmds) => {
                            let count = cmds.len();
                            self.macro_buffer = cmds;
                            self.toast(&format!("Loaded macro ({} commands)", count));
                        }
                        None => self.toast("Failed to parse macro file"),
                    }
                }
                Err(e) => self.toast(&format!("Load failed: {}", e)),
            }
        }
    }

    pub(crate) fn macro_to_json(cmds: &[AppCommand]) -> String {
        let entries: Vec<String> = cmds.iter().map(|cmd| {
            match cmd {
                AppCommand::DeleteSelected => r#"{"cmd":"DeleteSelected"}"#.into(),
                AppCommand::DuplicateSelected => r#"{"cmd":"DuplicateSelected"}"#.into(),
                AppCommand::SelectSingle(uid) => format!(r#"{{"cmd":"SelectSingle","uid":{}}}"#, uid),
                AppCommand::DeselectAll => r#"{"cmd":"DeselectAll"}"#.into(),
                AppCommand::SelectAll => r#"{"cmd":"SelectAll"}"#.into(),
                AppCommand::SetBooleanOp(uid, op) => {
                    let op_s = if matches!(op, BooleanOp::Intersect) {
                        r#""Intersect""#.into()
                    } else {
                        let name = if op.is_additive() { "Union" } else { "Subtract" };
                        format!(r#""{}",\"k\":{:.4}"#, name, op.blend_k())
                    };
                    format!(r#"{{"cmd":"SetBooleanOp","uid":{},"op":{}}}"#, uid, op_s)
                }
                AppCommand::SetVisible(uid, v) => format!(r#"{{"cmd":"SetVisible","uid":{},"vis":{}}}"#, uid, v),
                AppCommand::SetLocked(uid, l) => format!(r#"{{"cmd":"SetLocked","uid":{},"locked":{}}}"#, uid, l),
                AppCommand::SetCreateOp(op) => {
                    let s = match op { CreateOp::Union=>"Union", CreateOp::Subtract=>"Subtract", CreateOp::Intersect=>"Intersect", CreateOp::Slice=>"Slice" };
                    format!(r#"{{"cmd":"SetCreateOp","op":"{}"}}"#, s)
                }
                AppCommand::SetPosition(uid, p) => format!(r#"{{"cmd":"SetPosition","uid":{},"pos":[{:.4},{:.4},{:.4}]}}"#, uid, p.x, p.y, p.z),
                AppCommand::SetRotation(uid, r) => format!(r#"{{"cmd":"SetRotation","uid":{},"rot":[{:.4},{:.4},{:.4}]}}"#, uid, r.x, r.y, r.z),
                AppCommand::SetScale(uid, s) => format!(r#"{{"cmd":"SetScale","uid":{},"scale":[{:.4},{:.4},{:.4}]}}"#, uid, s.x, s.y, s.z),
                AppCommand::Toast(msg) => format!(r#"{{"cmd":"Toast","msg":"{}"}}"#, msg.replace('"', "\\\"")),
                _ => format!(r#"{{"cmd":"{}"}}"#, "Unknown"),
            }
        }).collect();
        format!("[\n  {}\n]", entries.join(",\n  "))
    }

    pub(crate) fn macro_from_json(json: &str) -> Option<Vec<AppCommand>> {
        // Simple JSON array parser — each entry has a "cmd" field
        let val: serde_json::Value = serde_json::from_str(json).ok()?;
        let arr = val.as_array()?;
        let mut cmds = Vec::new();
        for entry in arr {
            let cmd_name = entry.get("cmd")?.as_str()?;
            let cmd = match cmd_name {
                "DeleteSelected" => AppCommand::DeleteSelected,
                "DuplicateSelected" => AppCommand::DuplicateSelected,
                "SelectSingle" => AppCommand::SelectSingle(entry.get("uid")?.as_u64()? as u32),
                "DeselectAll" => AppCommand::DeselectAll,
                "SelectAll" => AppCommand::SelectAll,
                "SetVisible" => AppCommand::SetVisible(
                    entry.get("uid")?.as_u64()? as u32,
                    entry.get("vis")?.as_bool()?,
                ),
                "SetLocked" => AppCommand::SetLocked(
                    entry.get("uid")?.as_u64()? as u32,
                    entry.get("locked")?.as_bool()?,
                ),
                "SetCreateOp" => {
                    let op = match entry.get("op")?.as_str()? {
                        "Union" => CreateOp::Union,
                        "Subtract" => CreateOp::Subtract,
                        "Intersect" => CreateOp::Intersect,
                        "Slice" => CreateOp::Slice,
                        _ => return None,
                    };
                    AppCommand::SetCreateOp(op)
                }
                "SetPosition" => {
                    let p = entry.get("pos")?.as_array()?;
                    AppCommand::SetPosition(
                        entry.get("uid")?.as_u64()? as u32,
                        Vec3::new(p[0].as_f64()? as f32, p[1].as_f64()? as f32, p[2].as_f64()? as f32),
                    )
                }
                "SetRotation" => {
                    let r = entry.get("rot")?.as_array()?;
                    AppCommand::SetRotation(
                        entry.get("uid")?.as_u64()? as u32,
                        Vec3::new(r[0].as_f64()? as f32, r[1].as_f64()? as f32, r[2].as_f64()? as f32),
                    )
                }
                "SetScale" => {
                    let s = entry.get("scale")?.as_array()?;
                    AppCommand::SetScale(
                        entry.get("uid")?.as_u64()? as u32,
                        Vec3::new(s[0].as_f64()? as f32, s[1].as_f64()? as f32, s[2].as_f64()? as f32),
                    )
                }
                "Toast" => AppCommand::Toast(entry.get("msg")?.as_str()?.to_string()),
                _ => continue, // Skip unknown commands
            };
            cmds.push(cmd);
        }
        Some(cmds)
    }
}
