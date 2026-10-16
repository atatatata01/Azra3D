// project_state.rs — Project file state extracted from FourthLayerApp.
// Contains project path, save state, auto-save timer, and recent files.

/// Project save/load state.
pub(crate) struct ProjectState {
    pub(crate) project_path: Option<std::path::PathBuf>,
    pub(crate) scene_modified: bool,
    /// When set, overwrite-save (Ctrl+S) is blocked — file was saved by a newer version.
    pub(crate) save_blocked_reason: Option<String>,
    pub(crate) last_auto_save: std::time::Instant,
    /// Error text of the auto-save failure already shown to the user.
    /// `None` after a successful write (or before any failure). Equal to the
    /// latest error while that same failure keeps repeating, so the toast
    /// fires once; every attempt is still logged.
    pub(crate) auto_save_error: Option<String>,
    pub(crate) recent_files: Vec<std::path::PathBuf>,
    /// CR-70b: Background file-open dialog — prevents blocking the render loop (moved from FourthLayerApp — Phase 1).
    pub(crate) pending_file_open: Option<std::sync::mpsc::Receiver<Option<std::path::PathBuf>>>,
    /// Last title actually sent to the OS window. `ViewportCommand::Title` may only be
    /// sent when the title CHANGES: egui's `send_viewport_cmd` requests an immediate
    /// repaint, so an unconditional per-frame send turns the whole app into a
    /// full-rate busy loop (the ~300 fps "idle floor", owner "MatCap slow" 2026-08-07).
    pub(crate) last_window_title: String,
}

impl ProjectState {
    pub(crate) fn new(recent_files: Vec<std::path::PathBuf>) -> Self {
        Self {
            project_path: None,
            scene_modified: false,
            save_blocked_reason: None,
            last_auto_save: std::time::Instant::now(),
            auto_save_error: None,
            recent_files,
            pending_file_open: None,
            last_window_title: String::new(),
        }
    }
}
