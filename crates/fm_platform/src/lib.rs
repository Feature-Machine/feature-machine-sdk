// OS layer. Other crates ask this crate for OS locations and game installs, so they stay free
// of OS code. App folders live in fm_paths (re-exported here), so in-game plugins can use them
// without game detection.
pub use fm_paths::{data_dir, features_dir, logs_dir, APP_ID};

pub mod files;
pub mod feature_parts;
pub mod game_files;
pub mod inject;
pub mod memory;
pub mod install;
pub mod keyboard;
pub mod games;
mod msstore;
pub mod process;
mod steam;
mod version;

pub use games::{detect as detect_games, Detector, Game, GameInstall, Source};
