// app_types/src/keymap/event.rs — the event vocabulary (INPUT_ARCHITECTURE_PLAN §3.2).
//
// S1: TYPES ONLY. Widened ONCE so `val` never forces a second format migration. `Val::Hold` is the
// fix for the F-pie degrade-to-QuickTap bug; `Val::DoubleTap` stops the GG edge-slide tax.

use super::ctx::Mods;

/// Logical key — the printed letter, not the physical position (owner decision). Round-trips to disk
/// via `keybindings::key_name`/`key_from_str` (S0).
pub type KeyId = egui::Key;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum MouseBtn { Left, Right, Middle }

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum WheelDir { Up, Down }

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Input { Key(KeyId), Mouse(MouseBtn), Wheel(WheelDir) }

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Val { Press, Release, Hold, Click, DoubleClick, DoubleTap, Drag }

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct EventSig { pub input: Input, pub val: Val, pub mods: Mods }

/// Level-sampled, NOT edge-triggered — the home for drag modifiers (§5).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct QualSig { pub mods: Mods, pub key: Option<KeyId> }

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Trigger { Event(EventSig), Qual(QualSig) }
