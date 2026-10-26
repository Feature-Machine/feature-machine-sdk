// App identifier and per-user folders. A separate crate from fm_platform so the in-game plugins
// (ATS DLL) get only this and not game detection with its dependencies. The folders match the
// Tauri 2 path resolver for the same identifier (app_local_data_dir, app_log_dir), so the app and
// the plugins write to one place per user. Exception: Flatpak Steam on Linux gives the game its own
// XDG data folder (open; the app can send the path over IPC later). APP_ID must equal the Tauri "identifier".
use std::path::PathBuf;

/// Bundle identifier of the desktop app (Tauri "identifier").
pub const APP_ID: &str = "com.parallel42.featuremachine";

/// Per-user local data folder: %LOCALAPPDATA%, ~/.local/share or ~/Library/Application Support, plus APP_ID.
pub fn data_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join(APP_ID))
}

/// Installed features (feature packages plan): the store and active.json that in-game hosts read.
pub fn features_dir() -> Option<PathBuf> {
    data_dir().map(|d| d.join("features"))
}

/// Log folder. macOS uses ~/Library/Logs/<APP_ID> like Tauri; other OSes use <data_dir>/logs.
pub fn logs_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        dirs::home_dir().map(|h| h.join("Library/Logs").join(APP_ID))
    } else {
        data_dir().map(|d| d.join("logs"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_end_with_app_id() {
        let data = data_dir().expect("data dir");
        assert!(data.ends_with(APP_ID));
        let logs = logs_dir().expect("logs dir");
        assert!(logs.components().any(|c| c.as_os_str() == APP_ID));
    }
}
