// The list of features that a host loads (plan: "Lifecycle"). The app writes
// <data>/features/active.json after each install, update, enable or removal. The ATS plugin reads
// it during the session and follows it (FeatureSet::sync, feature session sync plan); the MSFS
// host reads it when the game starts. Game ids are the fm_platform::Game names, as text, so
// in-game hosts need no fm_platform (M2 rule).
use crate::{Capability, Multiplayer};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const FILE: &str = "active.json";

/// Exact map content for a complete session. A version update cannot silently replace its bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MapDependency { pub id: String, pub version: String, pub sha256: String }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActiveFeature {
    pub id: String,
    pub version: String,
    /// Full path of the feature library for this OS.
    pub library: PathBuf,
    /// "ats", "msfs2024".
    pub games: Vec<String>,
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub multiplayer: Multiplayer,
    /// First words of the engine commands that the feature may run (capability game_command).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub game_commands: Vec<String>,
    /// Hosts that the feature may reach with http_get (capability network, API 3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_hosts: Vec<String>,
    /// The feature's own data folder (data_dir below), for Host::data_dir. None: an older app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_maps: Vec<MapDependency>,
    /// The game files that the feature may read or change (capability user_files, files.rs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub user_files: Vec<crate::UserFile>,
}

/// The data folder of feature `id`: <features dir>/<id>/data, next to its versions/ folder. The
/// app gives it to the hosts (active.json) and to the lifecycle helper, so the game and the hooks
/// use one folder. A version update keeps it (prune touches versions/ only); removal deletes it.
pub fn data_dir(features_dir: &Path, id: &str) -> PathBuf {
    features_dir.join(id).join("data")
}

/// The active complete version of each feature in the install store, on or off: <features
/// dir>/<id>/state.json names it and versions/<version>/.fm-complete marks it complete (the rule
/// of fm_install::Install::active; a test in fm_install keeps both the same). In-game hosts do
/// not link fm_install. A host reads this at its start: the session versions (FeatureSet::pin).
pub fn installed_versions(features_dir: &Path) -> BTreeMap<String, String> {
    #[derive(Deserialize)]
    struct StoreState { active: String }
    let safe = |v: &str| !v.is_empty() && v.len() <= 64 && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
    let Ok(dir) = std::fs::read_dir(features_dir) else { return BTreeMap::new() };
    dir.flatten()
        .filter_map(|e| {
            let id = e.file_name().to_str()?.to_string();
            let state: StoreState = serde_json::from_slice(&std::fs::read(e.path().join("state.json")).ok()?).ok()?;
            let complete = safe(&state.active) && e.path().join("versions").join(&state.active).join(".fm-complete").is_file();
            complete.then_some((id, state.active))
        })
        .collect()
}

/// The state of one feature in a game session, as the host reports it to the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Running,
    /// Stopped by the member or at the end of the session.
    Stopped,
    /// Not loaded or stopped in a multiplayer session (review flag "blocked").
    Blocked,
    Failed,
}

/// The image passes of one feature, as the host sees them (shader pipeline plan, slice 10): the
/// worst of its passes. The app shows it in the Library Shaders list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PassState {
    /// A pass drew in the last check.
    Drawing,
    /// No pass drew: a shader builds, the scene is not found yet, or no frame came.
    Waiting,
    /// A depth pass waits for the game camera: the host has no live camera read (no camera map,
    /// or the map does not match this game build). The pass stays off, so its range is never wrong.
    NoCamera,
    /// The pass cannot draw (the shader was refused, the device was lost, or the format or API
    /// is not supported).
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeatureStatus {
    pub id: String,
    pub version: String,
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// None: the feature holds no image pass (or the host has none, as ATS).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass: Option<PassState>,
    /// The last start of this item: a counter of the host instance (`Hello.instance`). The app
    /// keeps page messages only of the newest activation, so a late message of a stopped start
    /// never shows (session sync plan 12.2). None: a host from before slice 7.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation: Option<u64>,
}

/// What a host reports after each change: its features and the multiplayer check.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HostReport {
    /// A multiplayer client was found in the game (TruckersMP for ATS).
    pub multiplayer: bool,
    /// The list could not be read (the reason); no feature runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub features: Vec<FeatureStatus>,
    /// The session versions (FeatureSet::pin, with the pins made later in the session): the
    /// app runs a feature's game exit hook on the version that the game ran (7f defect A).
    /// Empty: an older host, or no feature installed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub session_versions: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ActiveList {
    pub features: Vec<ActiveFeature>,
}

impl ActiveList {
    /// <features dir>/active.json, where the features dir is <app data>/features.
    pub fn path(features_dir: &Path) -> PathBuf {
        features_dir.join(FILE)
    }

    /// A missing file is an empty list (no feature installed); a damaged one is an error that
    /// the host logs, and it loads nothing.
    pub fn load(file: &Path) -> Result<ActiveList, String> {
        match std::fs::read(file) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", file.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ActiveList::default()),
            Err(e) => Err(format!("{}: {e}", file.display())),
        }
    }

    /// The features for one game.
    pub fn for_game<'a>(&'a self, game: &'a str) -> impl Iterator<Item = &'a ActiveFeature> {
        self.features.iter().filter(move |f| f.games.iter().any(|g| g == game))
    }
}
