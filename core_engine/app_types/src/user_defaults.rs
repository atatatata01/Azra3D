// user_defaults.rs — per-user "Set as default" store for toggles and settings.
//
// ONE file (`~/.azra/user_defaults.json`, flat `{ "key": value }`), keyed by stable dotted
// ids ("viewport.show_light_gizmos", "poly.extrude_along_normals_grouped"). Absent key =
// factory default. Two consumption patterns:
//   • app-state fields (viewport / render flags) → applied once at startup (`app_new.rs`);
//   • tool options read when the tool starts → `user_defaults::get_bool(key).unwrap_or(factory)`.
// UI side: `ui_menus::DefaultMenu` adds the right-click "Set as default / Reset default" menu.
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use serde_json::Value;
use crate::keybindings::KeyBindings;

const FILE: &str = "user_defaults.json";

fn store() -> &'static Mutex<BTreeMap<String, Value>> {
    static STORE: OnceLock<Mutex<BTreeMap<String, Value>>> = OnceLock::new();
    STORE.get_or_init(|| {
        let map = KeyBindings::config_dir()
            .and_then(|d| std::fs::read_to_string(d.join(FILE)).ok())
            .and_then(|s| serde_json::from_str::<BTreeMap<String, Value>>(&s).ok())
            .unwrap_or_default();
        Mutex::new(map)
    })
}

fn flush(map: &BTreeMap<String, Value>) -> bool {
    let Some(dir) = KeyBindings::config_dir() else { return false; };
    std::fs::write(dir.join(FILE), serde_json::to_string_pretty(map).unwrap_or_default()).is_ok()
}

pub fn get(key: &str) -> Option<Value> {
    store().lock().ok()?.get(key).cloned()
}

pub fn get_bool(key: &str) -> Option<bool> {
    get(key)?.as_bool()
}

pub fn get_color3(key: &str) -> Option<[f32; 3]> {
    let v = get(key)?;
    let a = v.as_array()?;
    if a.len() != 3 { return None; }
    let mut c = [0.0f32; 3];
    for (o, x) in c.iter_mut().zip(a) { *o = (x.as_f64()? as f32).clamp(0.0, 1.0); }
    Some(c)
}

/// Save `value` as the default for `key`. Returns false if the file could not be written.
pub fn set(key: &str, value: Value) -> bool {
    let Ok(mut m) = store().lock() else { return false; };
    m.insert(key.to_string(), value);
    flush(&m)
}

/// Forget the saved default for `key` (back to the factory value next session).
pub fn clear(key: &str) -> bool {
    let Ok(mut m) = store().lock() else { return false; };
    if m.remove(key).is_none() { return true; }
    flush(&m)
}

/// Visit every saved (key, value) — used by the startup apply pass.
pub fn for_each(mut f: impl FnMut(&str, &Value)) {
    if let Ok(m) = store().lock() {
        for (k, v) in m.iter() { f(k, v); }
    }
}
