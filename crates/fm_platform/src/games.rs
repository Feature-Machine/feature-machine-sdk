// Game detection: which supported games are installed, their version, and whether they run.
// One catalog row per game; each store module (Steam, Microsoft Store) turns its own data into
// the same GameInstall, so the app and the adapters never see store or OS details.
// A game is reported only on the OSes where it runs natively (plan decision "Desktop platforms").
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::process::{ProcInfo, ProcessList};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Game {
    Msfs2024,
    Ats,
}

impl Game {
    /// Every supported game, for contract tests with the app frontend.
    pub const ALL: [Game; 2] = [Game::Msfs2024, Game::Ats];

    /// The game name for messages outside the webview (uninstaller). The app UI has its own copy
    /// (games.ts GAME_NAMES).
    pub fn display_name(self) -> &'static str {
        match self {
            Game::Ats => "American Truck Simulator",
            Game::Msfs2024 => "Microsoft Flight Simulator 2024",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Steam,
    MicrosoftStore,
}

/// One install of one game. A PC can have several (a test PC has MSFS 2024 from Steam and Store).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GameInstall {
    pub game: Game,
    pub source: Source,
    pub install_dir: PathBuf,
    /// The game EXE that the process list is matched against.
    pub exe: PathBuf,
    /// EXE file version (Steam, Windows) or package version (Store). None where neither exists.
    pub version: Option<String>,
    pub steam_build_id: Option<u64>,
    /// False while Steam installs or must update the game.
    pub ready: bool,
    pub running_pid: Option<u32>,
}

/// Static facts about a supported game.
pub(crate) struct GameDef {
    pub game: Game,
    pub steam_app_id: u32,
    /// Microsoft Store package family name, if the game is sold there. Read only on Windows.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub store_family: Option<&'static str>,
    /// EXE path inside the install folder on this OS. None: the game does not run natively here.
    pub exe_rel: Option<&'static str>,
    /// Command line items that start the game EXE as a tool with no game session, for example
    /// MSFS that fspackagetool starts to build packages (seen 2026-10-17 on a test PC).
    pub tool_args: &'static [&'static str],
}

// ATS on macOS: SDK 1.15 documents no macOS plugin, so ATS is not reported there yet.
pub(crate) const CATALOG: &[GameDef] = &[
    GameDef {
        game: Game::Msfs2024,
        steam_app_id: 2537590,
        store_family: Some("Microsoft.Limitless_8wekyb3d8bbwe"),
        exe_rel: if cfg!(windows) { Some("FlightSimulator2024.exe") } else { None },
        tool_args: &["BuildAssetPackages"],
    },
    GameDef {
        game: Game::Ats,
        steam_app_id: 270880,
        store_family: None,
        exe_rel: if cfg!(windows) {
            Some("bin/win_x64/amtrucks.exe")
        } else if cfg!(target_os = "linux") {
            Some("bin/linux_x64/amtrucks")
        } else {
            None
        },
        tool_args: &[],
    },
];

/// Finds every supported game install on this PC and marks the running ones (one shot).
pub fn detect() -> Vec<GameInstall> {
    Detector::new().poll(Duration::ZERO)
}

/// Repeated detection for a watcher. Installs change rarely, so they are scanned again only when
/// older than the given age; the process list is kept between polls, so each poll asks the OS
/// only for new processes. Installs are sorted by game, then store, for a stable order.
pub struct Detector {
    procs: ProcessList,
    installs: Vec<GameInstall>,
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

    pub fn poll(&mut self, install_max_age: Duration) -> Vec<GameInstall> {
        if self.scanned.is_none_or(|t| t.elapsed() >= install_max_age) {
            let mut installs = crate::steam::installs();
            installs.extend(crate::msstore::installs());
            installs.sort_by_key(|i| (i.game as u8, i.source as u8));
            self.installs = installs;
            self.scanned = Some(Instant::now());
        }
        mark_running(&mut self.installs, &self.procs.refresh());
        self.installs.clone()
    }
}

/// Sets running_pid from a process snapshot. A process matches by its full EXE path. When the
/// OS hides the path (other user, protected or elevated process), a name match counts only if
/// exactly one install has that EXE name, so two MSFS installs are never both marked. The name
/// filter runs first, so the path check (file I/O) runs only for a few processes. A game EXE
/// that runs as a tool (GameDef::tool_args) is not a running game.
pub fn mark_running(installs: &mut [GameInstall], procs: &[ProcInfo]) {
    for i in 0..installs.len() {
        let exe = installs[i].exe.clone();
        let Some(name) = exe.file_name() else { continue };
        let tool_args = CATALOG.iter().find(|d| d.game == installs[i].game).map_or(&[][..], |d| d.tool_args);
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
            (candidates == 1).then_some(hidden.pid)
        });
        installs[i].running_pid = pid;
    }
}

/// True when any process has the EXE name of `game` (lifecycle hooks: LifecycleHost::game_running).
/// By name only, with no install path: a game from a folder that detection does not know, or a
/// tool start, still counts as running, so a hook never changes files while the game may save.
pub fn any_running(game: Game, procs: &[ProcInfo]) -> bool {
    let Some(name) = CATALOG.iter().find(|d| d.game == game).and_then(|d| d.exe_rel).and_then(|r| Path::new(r).file_name()) else { return false };
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
pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
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

    fn install(source: Source, exe: &str) -> GameInstall {
        let exe = PathBuf::from(exe);
        GameInstall {
            game: Game::Msfs2024,
            source,
            install_dir: exe.parent().unwrap().to_path_buf(),
            exe,
            version: None,
            steam_build_id: None,
            ready: true,
            running_pid: None,
        }
    }

    fn proc(pid: u32, name: &str, exe: Option<&str>) -> ProcInfo {
        ProcInfo { pid, name: OsString::from(name), exe: exe.map(PathBuf::from), cmd: vec![] }
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
        assert!(any_running(Game::Ats, &[proc(5, exe, None)]));
        assert!(any_running(Game::Ats, &[proc(5, "x", Some(if cfg!(windows) { "D:/other/bin/win_x64/amtrucks.exe" } else { "/other/amtrucks" }))]));
        assert!(!any_running(Game::Ats, &[proc(5, "eurotrucks2.exe", None)]));
        assert!(!any_running(Game::Ats, &[]));
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
        assert_eq!(v["game"], "msfs2024");
        assert_eq!(v["source"], "microsoft_store");
    }
}
