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

/// A map that a feature needs from a module (module layer plan 8.1): an interface (a camera
/// contract name), a semver range of that interface, and the map id. The feature names no
/// module; the app's lock selects the provider and the host reads the map from it only. No
/// deny_unknown_fields: a later field must not make older apps refuse a whole catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement { pub interface: String, pub versions: String, pub map: String }

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
    /// Maps from modules (Requirement). The host takes them from the session lock only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<Requirement>,
    /// The feature's settings file (crate::settings): a path, not the values, so a restart that
    /// clones this record still reads the current values. None: no settings, or an older app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<PathBuf>,
}

impl ActiveFeature {
    /// True when the feature reads maps: pinned in its package or required from a module.
    pub fn uses_maps(&self) -> bool { !self.memory_maps.is_empty() || !self.requires.is_empty() }
    /// The ids of every map that the feature may use with `mem:` keys: its own pins and the maps
    /// that it requires from modules (ground wetness plan: a writer has only module maps).
    pub fn map_ids(&self) -> impl Iterator<Item = &str> {
        self.memory_maps.iter().map(|m| m.id.as_str()).chain(self.requires.iter().map(|r| r.map.as_str()))
    }
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

/// One start of a feature in one host (feature surfaces plan, section 4): the host instance
/// (Hello.instance) and the activation (FeatureStatus.activation). A widget action names the
/// start that its widget showed, so a restart while the action waits cannot give it to the next
/// start; the host checks it just before the dispatch (FeatureSet::app_message).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageTarget {
    pub instance: u64,
    pub activation: u64,
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
    /// The settings revision that this activation runs (feature surfaces plan, section 3): from
    /// the file at its start, then from each update that it got. The app shows "Applied" only
    /// when it is the current revision. None: not a version 6 feature that runs, or an older host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings_revision: Option<u64>,
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
    /// The user folder of the game when it is not the default (an ATS -homedir start). The app
    /// keeps it in the session record and gives it to the exit hook, which cannot see the game
    /// arguments. None: the default folder, no user folder, or an older host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_root: Option<String>,
}

/// When a game policy applies (game policies plan; fm_install::policy uses the same type). A
/// multiplayer policy names the clients, in lower case, that the host detects ("truckersmp").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum When {
    Always,
    Multiplayer(Vec<String>),
}

/// What a policy does to a game that runs (decision D4): stop the feature at once, or let the
/// running start go on and refuse each new start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Apply {
    #[default]
    Now,
    NextStart,
}

/// One game policy that stops one feature in one game. The app writes it from the policy verdict
/// (fm_install::policy) and the host applies it (FeatureSet::sync, set_multiplayer). The list
/// carries it, not the feature record, because a running session keeps its own record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyStop {
    pub feature: String,
    /// The version that the policy hits (a session keeps its own version, which can have other
    /// powers than the installed one). None: every version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub game: String,
    pub when: When,
    #[serde(default)]
    pub apply: Apply,
    /// The policy id and its reason, for the host log and the feature state.
    pub policy: String,
    pub reason: String,
}

impl PolicyStop {
    /// True when the policy is for this record of a feature (its id, and its version if set).
    pub fn is_for(&self, f: &ActiveFeature) -> bool {
        self.feature == f.id && self.version.as_ref().is_none_or(|v| *v == f.version)
    }

    /// True when the policy applies with this multiplayer client (None: single player). Client
    /// names compare with no case: the host reports "TruckersMP", policies write "truckersmp".
    pub fn applies(&self, client: Option<&str>) -> bool {
        match &self.when {
            When::Always => true,
            When::Multiplayer(clients) => client.is_some_and(|c| clients.iter().any(|x| x.eq_ignore_ascii_case(c))),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ActiveList {
    pub features: Vec<ActiveFeature>,
    /// Per-install setup (app plan 2026-11-03): each game install with the features that are on
    /// there. None: an app before per-install setup, and every listed feature runs in every
    /// install of its games.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installs: Option<Vec<ActiveInstall>>,
    /// Game policies that stop listed features (PolicyStop). Hosts before policies ignore it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stops: Vec<PolicyStop>,
}

/// One game install in active.json: its id (the app's GameInstall::id), game, folder and the ids
/// of the features that run in it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActiveInstall {
    pub id: String,
    pub game: String,
    pub dir: PathBuf,
    pub features: Vec<String>,
    /// The settings file of each feature in this install (settings::install_path). A feature
    /// with no entry reads the file of ActiveFeature.settings.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub settings: std::collections::BTreeMap<String, PathBuf>,
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

    /// The list for the game process of `game` whose program is `exe`: only the features of the
    /// install that has `exe` in its folder. A list with no installs is kept whole; an exe that is
    /// in no listed install gets no feature (a new install starts empty, 2026-11-03).
    pub fn for_exe(self, game: &str, exe: &Path) -> ActiveList {
        let Some(installs) = &self.installs else { return self };
        let Some(mine) = installs.iter().find(|i| i.game == game && in_folder(exe, &i.dir)) else {
            return ActiveList { features: vec![], ..self };
        };
        // Each kept feature reads the settings file of this install.
        let features = self
            .features
            .into_iter()
            .filter(|f| mine.features.contains(&f.id))
            .map(|f| ActiveFeature { settings: mine.settings.get(&f.id).cloned().or(f.settings), ..f })
            .collect();
        ActiveList { features, installs: self.installs.clone(), stops: self.stops.clone() }
    }

    /// External helpers have no target executable folder. One explicit install must match
    /// both identity and application; old global lists cannot authorize an external host.
    pub fn for_install(self, application: &str, install: &str) -> Result<ActiveList, String> {
        let installs = self.installs.as_ref().ok_or("the external host needs a per-install list")?;
        let matches: Vec<_> = installs.iter().filter(|i| i.id == install).collect();
        let [mine] = matches.as_slice() else { return Err("the install grant is missing or ambiguous".into()) };
        if mine.game != application { return Err("the install grant names another application".into()); }
        let features = self.features.iter().filter(|f| mine.features.contains(&f.id) && f.games.iter().any(|a| a == application))
            .map(|f| ActiveFeature { settings: mine.settings.get(&f.id).cloned().or(f.settings.clone()), ..f.clone() }).collect();
        Ok(ActiveList { features, ..self })
    }

    /// The features for one game.
    pub fn for_game<'a>(&'a self, game: &'a str) -> impl Iterator<Item = &'a ActiveFeature> {
        self.features.iter().filter(move |f| f.games.iter().any(|g| g == game))
    }
}

/// True when `exe` is inside `dir`. Both are canonical when they can be (links, "..", the long
/// form); Windows paths compare with no case and either slash, like the app's install match.
fn in_folder(exe: &Path, dir: &Path) -> bool {
    let text = |p: PathBuf| {
        let t = p.to_string_lossy().replace('/', std::path::MAIN_SEPARATOR_STR);
        let t = t.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(t);
        if cfg!(windows) { t.to_lowercase() } else { t }
    };
    let (e, d) = (text(canon(exe)), text(canon(dir)));
    let d = d.trim_end_matches(std::path::MAIN_SEPARATOR);
    e.len() > d.len() && e.starts_with(d) && e[d.len()..].starts_with(std::path::MAIN_SEPARATOR)
}

/// The canonical form of the nearest part of `p` that exists, plus the rest of `p`. A path that
/// does not exist (yet) then has the same form as an existing folder above it: with only a
/// fallback to the raw path, an 8.3 short name (C:\Users\RUNNER~1, the GitHub runner) never
/// matched the long name of the canonical folder (SDK 0.3.0 CI). A ".." in the rest stays raw.
fn canon(p: &Path) -> PathBuf {
    let mut rest = vec![];
    let mut cur = p;
    loop {
        if let Ok(found) = std::fs::canonicalize(cur) {
            return rest.iter().rev().fold(found, |acc, name| acc.join(name));
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => { rest.push(name.to_os_string()); cur = parent; }
            _ => return p.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feature(id: &str) -> ActiveFeature {
        ActiveFeature { id: id.into(), version: "1.0.0".into(), library: "x".into(), games: vec!["msfs2024".into()], capabilities: vec![], multiplayer: Multiplayer::Allowed, game_commands: vec![], network_hosts: vec![], memory_maps: vec![], data_dir: None, user_files: vec![], requires: vec![], settings: None }
    }

    // External hosts select one exact install, independent of their own executable folder.
    // An absent, duplicate or wrong-application grant must never inherit the global list.
    #[test]
    fn an_external_host_requires_one_matching_install() {
        let install = |id: &str, game: &str, ids: &[&str]| ActiveInstall { id: id.into(), game: game.into(), dir: "/vendor".into(), features: ids.iter().map(|s| s.to_string()).collect(), settings: Default::default() };
        let mut a = install("a", "msfs2024", &["one"]);
        a.settings.insert("one".into(), "/data/one/installs/a/settings.json".into());
        let list = ActiveList { features: vec![feature("one"), feature("two")], installs: Some(vec![a.clone(), install("b", "msfs2024", &["two"])]), ..Default::default() };
        let selected = list.clone().for_install("msfs2024", "a").unwrap();
        assert_eq!(selected.features.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), ["one"]);
        assert_eq!(selected.features[0].settings, a.settings.get("one").cloned());
        assert!(list.clone().for_install("ats", "a").is_err());
        assert!(list.clone().for_install("msfs2024", "missing").is_err());
        assert!(ActiveList { installs: None, ..list.clone() }.for_install("msfs2024", "a").is_err());
        assert!(ActiveList { installs: Some(vec![a.clone(), a]), ..list }.for_install("msfs2024", "a").is_err());
    }

    // Per-install setup: a host keeps only the features of the install that its exe is in; a
    // list from an older app (no installs) stays whole; an exe in no install gets nothing.
    #[test]
    fn a_host_keeps_the_features_of_its_own_install() {
        let t = tempfile::tempdir().unwrap();
        let (steam, xbox) = (t.path().join("Steam").join("Limitless"), t.path().join("WindowsApps").join("Microsoft.Limitless_1.9.21.0_x64__8wekyb3d8bbwe"));
        std::fs::create_dir_all(&steam).unwrap();
        let install = |id: &str, dir: &Path, f: &[&str]| ActiveInstall { id: id.into(), game: "msfs2024".into(), dir: dir.into(), features: f.iter().map(|s| s.to_string()).collect(), settings: Default::default() };
        let mut s = install("s", &steam, &["trip", "probe"]);
        s.settings.insert("trip".into(), PathBuf::from("trip/installs/s/settings.json"));
        let list = ActiveList { features: vec![ActiveFeature { settings: Some("trip/settings.json".into()), ..feature("trip") }, feature("probe")], installs: Some(vec![s, install("x", &xbox, &["probe", "trip"])]), stops: vec![] };
        let ids = |l: ActiveList| l.features.into_iter().map(|f| f.id).collect::<Vec<_>>();
        assert_eq!(ids(list.clone().for_exe("msfs2024", &steam.join("FlightSimulator2024.exe"))), ["trip", "probe"]);
        // The settings file of the install; an install with no entry keeps the feature's file.
        let settings = |l: ActiveList| l.features.into_iter().find(|f| f.id == "trip").and_then(|f| f.settings);
        assert_eq!(settings(list.clone().for_exe("msfs2024", &steam.join("x.exe"))), Some(PathBuf::from("trip/installs/s/settings.json")));
        assert_eq!(settings(list.clone().for_exe("msfs2024", &xbox.join("x.exe"))), Some(PathBuf::from("trip/settings.json")));
        let list = ActiveList { installs: list.installs.map(|mut v| { v[1].features = vec!["probe".into()]; v }), ..list };
        assert_eq!(ids(list.clone().for_exe("msfs2024", &xbox.join("FlightSimulator2024.exe"))), ["probe"], "a folder that is not there compares as text");
        assert!(ids(list.clone().for_exe("msfs2024", &t.path().join("Other").join("FlightSimulator2024.exe"))).is_empty());
        assert!(ids(list.clone().for_exe("ats", &steam.join("x.exe"))).is_empty(), "another game");
        let sibling = t.path().join("Steam").join("Limitless2").join("FlightSimulator2024.exe");
        assert!(ids(list.clone().for_exe("msfs2024", &sibling)).is_empty(), "a folder name that only starts the same");
        if cfg!(windows) {
            let upper = PathBuf::from(steam.to_string_lossy().to_uppercase().replace('\\', "/")).join("FlightSimulator2024.exe");
            assert_eq!(ids(list.clone().for_exe("msfs2024", &upper)).len(), 2, "case and slashes");
        }
        let old = ActiveList { installs: None, ..list.clone() };
        assert_eq!(ids(old.for_exe("msfs2024", &sibling)).len(), 2, "an older app's list stays whole");
        let json = serde_json::to_value(&ActiveList { installs: None, ..list.clone() }).unwrap();
        assert!(json.get("installs").is_none(), "no installs key for no installs");
    }
}
