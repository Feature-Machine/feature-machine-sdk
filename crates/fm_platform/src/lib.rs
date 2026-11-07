// OS layer. Other crates ask this crate for OS locations and game installs, so they stay free
// of OS code. App folders live in fm_paths (re-exported here), so in-game plugins can use them
// without game detection.
pub use fm_paths::{data_dir, data_dir_override, features_dir, isolate_from_env, logs_dir, APP_ID, DATA_DIR_ENV};

pub mod files;
pub mod feature_parts;
pub mod application_files;
// Existing record paths and source callers keep the legacy name during migration.
pub use application_files as game_files;
pub mod inject;
pub mod memory;
pub mod install;
pub mod keyboard;
pub mod notify;
pub mod applications;
mod msstore;
pub mod process;
pub mod process_tree;
pub mod stdio;
pub mod uninstall;
mod steam;
mod version;

pub use applications::{detect as detect_applications, Detector, ApplicationId, ApplicationInstall, ApplicationInstance, ApplicationService, InstanceKind, Source};
pub use fm_application::ApplicationDefinition;

// Existing SDK and tool callers retain their source names while neutral callers migrate.
pub use applications::{detect as detect_games, ApplicationId as Game, ApplicationInstall as GameInstall};
pub mod games {
    pub use crate::applications::*;
    pub use fm_application::ApplicationId as Game;
    pub use crate::applications::ApplicationInstall as GameInstall;
}
