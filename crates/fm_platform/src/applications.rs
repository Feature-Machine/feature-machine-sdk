// Discovery separates stable installs from observed desktop and service processes. Providers
// supply OS facts; reviewed descriptors and integrations determine what this build can use.
use serde::{Deserialize, Serialize, Serializer};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::process::{ProcInfo, ProcessList};

pub use fm_application::{ApplicationId, ApplicationDefinition};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Steam,
    MicrosoftStore,
    WindowsUninstall,
}

impl Source {
    /// The serde name ("steam", "microsoft_store").
    pub fn id(self) -> &'static str {
        match self {
            Source::Steam => "steam",
            Source::MicrosoftStore => "microsoft_store",
            Source::WindowsUninstall => "windows_uninstall",
        }
    }
}

/// One application install. A PC can have several, such as Steam and Store installs of MSFS.
/// The wire form adds `id` (ApplicationInstall::id); see the Serialize impl below.
#[derive(Clone, Debug, PartialEq)]
pub struct ApplicationInstall {
    /// Legacy source field retained for existing struct literals; the wire name is application.
    pub game: ApplicationId,
    pub source: Source,
    pub install_dir: PathBuf,
    /// The application EXE that the process list is matched against.
    pub exe: PathBuf,
    /// EXE file version (Steam, Windows) or package version (Store). None where neither exists.
    pub version: Option<String>,
    pub steam_build_id: Option<u64>,
    /// False while Steam installs or must update the game.
    pub ready: bool,
    pub running_pid: Option<u32>,
    /// Stable provider identity for ordinary Windows registrations, independent of location.
    pub source_key: Option<String>,
    pub instances: Vec<ApplicationInstance>,
    pub service: Option<ApplicationService>,
    pub error: Option<String>,
}

/// One observed process. The start token changes on restart even when Windows reuses a PID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationInstance {
    pub kind: InstanceKind,
    pub pid: u32,
    #[serde(with = "instance_start")]
    pub start_id: u64,
    pub exe: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceKind { Desktop, Service }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationService { pub name: String, pub exe: PathBuf }

// Windows start tokens exceed JavaScript's integer range. Their wire form is decimal text,
// while OS and runtime comparisons retain the exact integer.
mod instance_start {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(value: &u64, s: S) -> Result<S::Ok, S::Error> { s.collect_str(value) }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        String::deserialize(d)?.parse().map_err(serde::de::Error::custom)
    }
}

impl ApplicationInstall {
    /// The stable id of this install: the key of its setup (per-install setup plan). It is
    /// "<game>-<source>-<8 hex>", the hash of a key that an update or a move keeps: the Steam app
    /// id (one Steam client puts an app in one library only, so a move to another library keeps
    /// every per-install file), or the Store package family (its folder name has the version).
    pub fn id(&self) -> String {
        let app = CATALOG.iter().find(|d| d.application == self.game).map(|d| d.steam_app_id);
        let key = match (self.source, app) {
            (Source::Steam, Some(app)) => format!("steam-app-{app}"),
            (Source::Steam, None) => path_key(&self.install_dir),
            (Source::MicrosoftStore, _) => store_family(&self.install_dir).unwrap_or_else(|| path_key(&self.install_dir)),
            (Source::WindowsUninstall, _) => self.source_key.clone().unwrap_or_else(|| "unregistered".into()),
        };
        format!("{}-{}-{:08x}", self.game.id(), self.source.id(), fnv1a(&key) as u32)
    }
}

/// The id is part of what the app sends (list_games, games-changed), so the UI and the app data
/// files use one key. Computed on each send: it can never differ from the folder.
impl Serialize for ApplicationInstall {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            id: String,
            #[serde(rename = "application")]
            game: ApplicationId,
            source: Source,
            install_dir: &'a Path,
            exe: &'a Path,
            version: &'a Option<String>,
            steam_build_id: Option<u64>,
            ready: bool,
            running_pid: Option<u32>,
            #[serde(skip_serializing_if = "Option::is_none")]
            source_key: &'a Option<String>,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            instances: &'a Vec<ApplicationInstance>,
            #[serde(skip_serializing_if = "Option::is_none")]
            service: &'a Option<ApplicationService>,
            #[serde(skip_serializing_if = "Option::is_none")]
            error: &'a Option<String>,
        }
        let g = self;
        Wire { id: g.id(), game: g.game, source: g.source, install_dir: &g.install_dir, exe: &g.exe, version: &g.version, steam_build_id: g.steam_build_id, ready: g.ready, running_pid: g.running_pid, source_key: &g.source_key, instances: &g.instances, service: &g.service, error: &g.error }.serialize(s)
    }
}

/// A folder as id text: Windows ignores case and accepts both slashes (the same_path rule).
fn path_key(p: &Path) -> String {
    let t = p.to_string_lossy();
    if cfg!(windows) { t.replace('/', "\\").trim_end_matches('\\').to_lowercase() } else { t.trim_end_matches('/').to_string() }
}

/// The package family of a Store folder: "Microsoft.Limitless_1.9.21.0_x64__8wekyb3d8bbwe" gives
/// "microsoft.limitless_8wekyb3d8bbwe" (full name: name, version, architecture, resource id,
/// publisher id). None when the folder name has another form.
fn store_family(dir: &Path) -> Option<String> {
    let full = dir.file_name()?.to_str()?;
    let parts: Vec<&str> = full.split('_').collect();
    (parts.len() == 5 && !parts[0].is_empty() && !parts[4].is_empty()).then(|| format!("{}_{}", parts[0], parts[4]).to_lowercase())
}

/// FNV-1a, 64 bit: a short stable hash with no dependency (fm_feature_sdk root_id does the same).
fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// Static process facts projected from a reviewed application descriptor.
pub(crate) struct ProcessDefinition {
    pub application: ApplicationId,
    pub steam_app_id: u32,
    /// Microsoft Store package family name, if the game is sold there. Read only on Windows.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub store_family: Option<&'static str>,
    /// EXE path inside the install folder on this OS. None: the game does not run natively here.
    pub exe_rel: Option<&'static str>,
    /// Command line items that start the game EXE as a tool with no game session, for example
    /// MSFS that fspackagetool starts to build packages (seen 2026-10-17 on a test PC).
    pub tool_args: Vec<&'static str>,
}

// The same descriptor supplies discovery and UI metadata. OS-specific executable paths are
// projected here, so providers have no application switch when another reviewed row is added.
pub(crate) static CATALOG: std::sync::LazyLock<Vec<ProcessDefinition>> = std::sync::LazyLock::new(|| {
    fm_application::definitions().iter().filter_map(|d| d.process.as_ref().map(|p| ProcessDefinition {
        application: d.id, steam_app_id: p.steam_app_id, store_family: p.store_family.as_deref(),
        exe_rel: p.executables.get(std::env::consts::OS).map(String::as_str),
        tool_args: p.tool_args.iter().map(String::as_str).collect(),
    })).collect()
});

/// Finds every supported application install on this PC and marks the running ones (one shot).
pub fn detect() -> Vec<ApplicationInstall> {
    Detector::new().poll(Duration::ZERO)
}

/// Repeated detection for a watcher. Installs change rarely, so they are scanned again only when
/// older than the given age; the process list is kept between polls, so each poll asks the OS
/// only for new processes. Installs are sorted by application, then source, for a stable order.
pub struct Detector {
    procs: ProcessList,
    installs: Vec<ApplicationInstall>,
    scanned: Option<Instant>,
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector {
    pub fn new() -> Self {
        Detector { procs: ProcessList::new(), installs: Vec::new(), scanned: None }
    }

    pub fn poll(&mut self, install_max_age: Duration) -> Vec<ApplicationInstall> {
        if self.scanned.is_none_or(|t| t.elapsed() >= install_max_age) {
            let mut installs = crate::steam::installs();
            installs.extend(crate::msstore::installs());
            installs.extend(crate::uninstall::installs());
            installs.sort_by_key(|i| (i.game, i.source as u8));
            self.installs = installs;
            self.scanned = Some(Instant::now());
        }
        mark_running(&mut self.installs, &self.procs.refresh());
        crate::uninstall::observe(&mut self.installs);
        self.installs.clone()
    }
}

/// Sets running_pid from a process snapshot. A process matches by its full EXE path. When the
/// OS hides the path (other user, protected or elevated process), a name match counts only if
/// exactly one install has that EXE name, so two MSFS installs are never both marked. The name
/// filter runs first, so the path check (file I/O) runs only for a few processes. An application EXE
/// that runs as a tool (ProcessDefinition::tool_args) is not a member session.
pub fn mark_running(installs: &mut [ApplicationInstall], procs: &[ProcInfo]) {
    for i in 0..installs.len() {
        installs[i].instances.clear();
        if installs[i].source == Source::WindowsUninstall && !installs[i].ready { installs[i].running_pid = None; continue; }
        let exe = installs[i].exe.clone();
        let Some(name) = exe.file_name() else { continue };
        let tool_args = CATALOG.iter().find(|d| d.application == installs[i].game).map_or(&[][..], |d| d.tool_args.as_slice());
        let is_tool = |p: &ProcInfo| p.cmd.iter().skip(1).any(|c| tool_args.iter().any(|a| c.to_string_lossy().eq_ignore_ascii_case(a)));
        // The EXE file name when known: Linux truncates process names to 15 characters.
        let named: Vec<&ProcInfo> = procs
            .iter()
            .filter(|p| same_name(p.exe.as_deref().and_then(Path::file_name).unwrap_or(&p.name), name) && !is_tool(p))
            .collect();
        let by_path = named.iter().find(|p| p.exe.as_deref().is_some_and(|e| same_path(e, &exe)));
        let pid = by_path.map(|p| p.pid).or_else(|| {
            let candidates = installs.iter().filter(|o| o.exe.file_name().is_some_and(|n| same_name(n, name))).count();
            let hidden = named.iter().find(|p| p.exe.is_none())?;
            (candidates == 1 && installs[i].source != Source::WindowsUninstall).then_some(hidden.pid)
        });
        installs[i].running_pid = pid;
        if let Some(process) = pid.and_then(|pid| procs.iter().find(|p| p.pid == pid)) {
            installs[i].instances.push(ApplicationInstance { kind: InstanceKind::Desktop, pid: process.pid, start_id: process.start_id, exe: exe.clone() });
        }
    }
}

/// True when any process has the EXE name of `game` (lifecycle hooks: LifecycleHost::game_running).
/// By name only, with no install path: a game from a folder that detection does not know, or a
/// tool start, still counts as running, so a hook never changes files while the game may save.
pub fn any_running(game: ApplicationId, procs: &[ProcInfo]) -> bool {
    let Some(name) = CATALOG.iter().find(|d| d.application == game).and_then(|d| d.exe_rel).and_then(|r| Path::new(r).file_name()) else { return false };
    procs.iter().any(|p| same_name(p.exe.as_deref().and_then(Path::file_name).unwrap_or(&p.name), name))
}

/// File name equality; Windows file names ignore case.
fn same_name(a: &OsStr, b: &OsStr) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
    } else {
        a == b
    }
}

/// Path equality that survives symlinks (Linux ~/.steam/steam) and case or slash differences
/// (Windows: libraryfolders.vdf stores "c:\\program files (x86)\\steam").
pub fn same_path(a: &Path, b: &Path) -> bool {
    if let (Ok(ca), Ok(cb)) = (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        if ca == cb {
            return true;
        }
    }
    if cfg!(windows) {
        let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
        norm(a) == norm(b)
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn install(source: Source, exe: &str) -> ApplicationInstall {
        let exe = PathBuf::from(exe);
        ApplicationInstall {
            game: ApplicationId::MSFS2024,
            source,
            install_dir: exe.parent().unwrap().to_path_buf(),
            exe,
            version: None,
            steam_build_id: None,
            ready: true,
            source_key: None, instances: vec![], service: None, error: None, running_pid: None,
        }
    }

    fn proc(pid: u32, name: &str, exe: Option<&str>) -> ProcInfo {
        ProcInfo { pid, start_id: u64::from(pid), name: OsString::from(name), exe: exe.map(PathBuf::from), cmd: vec![] }
    }

    // Per-install setup: the id keys the setup of an install, so a Store update (new folder with
    // the new version) keeps it, two installs never share it, and it is on the wire.
    #[test]
    fn install_id_is_stable_and_unique() {
        let store = |v: &str| install(Source::MicrosoftStore, &format!("C:/Program Files/WindowsApps/Microsoft.Limitless_{v}_x64__8wekyb3d8bbwe/FlightSimulator2024.exe"));
        let a = store("1.9.18.0").id();
        assert!(a.starts_with("msfs2024-microsoft_store-") && a.len() == "msfs2024-microsoft_store-".len() + 8, "{a}");
        assert_eq!(store("1.9.21.0").id(), a, "a Store update keeps the id");
        let steam = install(Source::Steam, "/games/steam/Limitless/FlightSimulator2024.exe");
        assert_eq!(steam.id(), "msfs2024-steam-019633d7");
        assert_eq!(ApplicationInstall { game: ApplicationId::ATS, ..steam.clone() }.id(), "ats-steam-335b6c27");
        assert!(steam.id().starts_with("msfs2024-steam-"));
        assert_eq!(steam.id(), install(Source::Steam, "/games/other/Limitless/FlightSimulator2024.exe").id(), "a move to another Steam library keeps the id");
        assert_ne!(steam.id(), a, "Steam and Store differ");
        assert_ne!(steam.id(), ApplicationInstall { game: ApplicationId::ATS, ..steam.clone() }.id(), "the game is in the id");
        let ok = |id: &str| id.len() <= 64 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
        assert!(ok(&a) && ok(&steam.id()), "the app's InstallId form");
        let wire = serde_json::to_value(&steam).unwrap();
        assert_eq!(wire["id"], steam.id());
        assert_eq!(wire["source"], "steam");
    }

    #[test]
    fn game_exe_started_as_tool_is_not_running() {
        let exe = "/games/steam/Limitless/FlightSimulator2024.exe";
        let mut v = vec![install(Source::Steam, exe)];
        let mut p = proc(9, "FlightSimulator2024.exe", Some(exe));
        p.cmd = ["FlightSimulator2024.exe", "-I", ";", "BuildAssetPackages", "x.xml", "-FastLaunch"].map(OsString::from).to_vec();
        mark_running(&mut v, &[p.clone()]);
        assert_eq!(v[0].running_pid, None, "fspackagetool build instance");
        p.cmd = vec![OsString::from("FlightSimulator2024.exe")];
        mark_running(&mut v, &[p]);
        assert_eq!(v[0].running_pid, Some(9), "normal start");
    }

    /// The hook check counts any process with the game's EXE name, with or without a known path.
    #[test]
    fn any_running_matches_the_exe_name() {
        let exe = if cfg!(windows) { "AMTRUCKS.EXE" } else { "amtrucks" };
        assert!(any_running(ApplicationId::ATS, &[proc(5, exe, None)]));
        assert!(any_running(ApplicationId::ATS, &[proc(5, "x", Some(if cfg!(windows) { "D:/other/bin/win_x64/amtrucks.exe" } else { "/other/amtrucks" }))]));
        assert!(!any_running(ApplicationId::ATS, &[proc(5, "eurotrucks2.exe", None)]));
        assert!(!any_running(ApplicationId::ATS, &[]));
    }

    #[test]
    fn running_matches_full_path_only() {
        let mut v = vec![
            install(Source::Steam, "/games/steam/Limitless/FlightSimulator2024.exe"),
            install(Source::MicrosoftStore, "/games/store/FlightSimulator2024.exe"),
        ];
        let procs = [proc(7, "FlightSimulator2024.exe", Some("/games/store/FlightSimulator2024.exe"))];
        mark_running(&mut v, &procs);
        assert_eq!(v[0].running_pid, None);
        assert_eq!(v[1].running_pid, Some(7));
    }

    #[test]
    fn hidden_path_matches_by_name_only_when_unambiguous() {
        let procs = [proc(9, "FlightSimulator2024.exe", None)];
        let mut two = vec![
            install(Source::Steam, "/a/FlightSimulator2024.exe"),
            install(Source::MicrosoftStore, "/b/FlightSimulator2024.exe"),
        ];
        mark_running(&mut two, &procs);
        assert!(two.iter().all(|i| i.running_pid.is_none()));
        let mut one = vec![install(Source::Steam, "/a/FlightSimulator2024.exe")];
        mark_running(&mut one, &procs);
        assert_eq!(one[0].running_pid, Some(9));
    }

    #[cfg(windows)]
    #[test]
    fn windows_hidden_name_ignores_case() {
        let mut one = vec![install(Source::Steam, "/a/FlightSimulator2024.exe")];
        mark_running(&mut one, &[proc(5, "flightsimulator2024.EXE", None)]);
        assert_eq!(one[0].running_pid, Some(5));
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_ignore_case_and_slashes() {
        assert!(same_path(
            Path::new("c:\\program files (x86)\\steam/steamapps/common/x.exe"),
            Path::new("C:\\Program Files (x86)\\Steam\\steamapps\\common\\X.exe"),
        ));
    }

    #[test]
    fn install_serializes_for_the_app() {
        let v = serde_json::to_value(install(Source::MicrosoftStore, "/b/FlightSimulator2024.exe")).unwrap();
        assert_eq!(v["application"], "msfs2024");
        assert_eq!(v["source"], "microsoft_store");
    }
}
