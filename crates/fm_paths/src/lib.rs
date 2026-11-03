// App identifier and per-user folders. A separate crate from fm_platform so the in-game plugins
// (ATS DLL) get only this and not game detection with its dependencies. The folders match the
// Tauri 2 path resolver for the same identifier (app_local_data_dir, app_log_dir), so the app and
// the plugins write to one place per user. Exception: Flatpak Steam on Linux gives the game its own
// XDG data folder (open; the app can send the path over IPC later). APP_ID must equal the Tauri "identifier".
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Bundle identifier of the desktop app (Tauri "identifier").
pub const APP_ID: &str = "com.parallel42.featuremachine";

/// Developer switch (debug builds only): an absolute folder that replaces the data folder, so a
/// test run of the app (e2e smoke) never reads or changes the member's features and settings.
/// Only a process that calls isolate_from_env uses it (the app). The in-game hosts and the
/// uninstaller never do, so they always use the member's folder, also in a debug build.
pub const DATA_DIR_ENV: &str = "FM_DATA_DIR";

/// The isolated data folder of this process: set once by isolate_from_env, else unset.
static ISOLATED: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Reads FM_DATA_DIR once for this process (debug builds; a release build ignores it). Ok(None):
/// not set, the member's folder. A relative value is an error: its meaning changes with the
/// current folder, and the caller must stop before anything (Tauri) creates the real folder.
pub fn isolate_from_env() -> Result<Option<PathBuf>, String> {
    let value = cfg!(debug_assertions).then(|| std::env::var_os(DATA_DIR_ENV)).flatten();
    let dir = checked(value.unwrap_or_default())?;
    Ok(ISOLATED.get_or_init(|| dir).clone())
}

/// An empty value counts as not set; a value that is not absolute is refused.
fn checked(value: OsString) -> Result<Option<PathBuf>, String> {
    let path = PathBuf::from(value);
    if path.as_os_str().is_empty() {
        Ok(None)
    } else if path.is_absolute() {
        Ok(Some(path))
    } else {
        Err(format!("{DATA_DIR_ENV} must be an absolute folder, not {}", path.display()))
    }
}

/// The FM_DATA_DIR folder when isolate_from_env took one in this process (the app gives it to
/// Tauri, and it attaches to no game while it is set: a game reads the member's folder).
pub fn data_dir_override() -> Option<PathBuf> {
    ISOLATED.get().cloned().flatten()
}

/// Per-user local data folder: %LOCALAPPDATA%, ~/.local/share or ~/Library/Application Support, plus APP_ID.
/// A process isolated by isolate_from_env uses its FM_DATA_DIR folder instead.
pub fn data_dir() -> Option<PathBuf> {
    data_dir_override().or_else(|| dirs::data_local_dir().map(|d| d.join(APP_ID)))
}

/// Installed features (feature packages plan): the store and active.json that in-game hosts read.
pub fn features_dir() -> Option<PathBuf> {
    data_dir().map(|d| d.join("features"))
}

/// Log folder. macOS uses ~/Library/Logs/<APP_ID> like Tauri; other OSes and FM_DATA_DIR use
/// <data_dir>/logs (Tauri's root override also puts its logs there).
pub fn logs_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") && data_dir_override().is_none() {
        dirs::home_dir().map(|h| h.join("Library/Logs").join(APP_ID))
    } else {
        data_dir().map(|d| d.join("logs"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // No test here calls isolate_from_env, so this process uses the member's folder even when
    // FM_DATA_DIR is set (the in-game hosts and the uninstaller are such processes).
    #[test]
    fn paths_end_with_app_id() {
        let data = data_dir().expect("data dir");
        assert!(data.ends_with(APP_ID));
        let logs = logs_dir().expect("logs dir");
        assert!(logs.components().any(|c| c.as_os_str() == APP_ID));
    }

    // The FM_DATA_DIR rules without a change to this process's environment (tests run in
    // parallel): empty is not set, absolute is used as is, relative is an error.
    #[test]
    fn override_value_rules() {
        let abs = std::env::temp_dir().join("fm-data");
        assert_eq!(checked(OsString::new()), Ok(None));
        assert_eq!(checked(abs.clone().into_os_string()), Ok(Some(abs)));
        assert!(checked("relative/fm-data".into()).is_err());
    }
}
