// Steam installs. steamlocate (the common crate for this) finds every Steam folder (registry on
// Windows; native, Flatpak and Snap folders on Linux) and reads libraryfolders.vdf and the app
// manifests, so we do not keep our own VDF parser. Each library is checked for each catalog game.
use std::path::{Path, PathBuf};
use steamlocate::{app::StateFlag, SteamDir};

use crate::games::{GameInstall, Source, CATALOG};

pub fn installs() -> Vec<GameInstall> {
    let mut out: Vec<GameInstall> = Vec::new();
    for dir in steamlocate::locate_all().unwrap_or_default() {
        for i in installs_in(&dir) {
            // Linux can expose one Steam folder under two paths (~/.steam/steam and ~/.local/share/Steam).
            if !out.iter().any(|o| crate::games::same_path(&o.install_dir, &i.install_dir)) {
                out.push(i);
            }
        }
    }
    out
}

/// Catalog games in one Steam folder. Public in the crate for tests with a fixture folder.
pub(crate) fn installs_in(steam: &SteamDir) -> Vec<GameInstall> {
    let mut out = Vec::new();
    for def in CATALOG {
        let Some(exe_rel) = def.exe_rel else { continue };
        let Ok(Some((app, lib))) = steam.find_app(def.steam_app_id) else { continue };
        // libraryfolders.vdf can store the path in lower case; the canonical on-disk form is
        // shown. It also resolves junctions and subst drives (accepted: the same game folder).
        // dunce avoids the \\?\ prefix of std canonicalize on Windows.
        let raw_dir = lib.resolve_app_dir(&app);
        let install_dir = dunce::canonicalize(&raw_dir).unwrap_or(raw_dir);
        let exe = join_rel(&install_dir, exe_rel);
        let flags: Vec<StateFlag> = app.state_flags.map(|f| f.flags().collect()).unwrap_or_default();
        let ready = flags.contains(&StateFlag::FullyInstalled) && !flags.contains(&StateFlag::UpdateRequired);
        out.push(GameInstall {
            game: def.game,
            source: Source::Steam,
            version: crate::version::file_version(&exe),
            install_dir,
            exe,
            steam_build_id: app.build_id,
            ready,
            running_pid: None,
        });
    }
    out
}

fn join_rel(base: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(base.to_path_buf(), |p, part| p.join(part))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::Game;
    use std::fs;

    // A Steam folder like the one of a test PC: ATS in the main library, MSFS in a second library, plus a
    // stale library entry that no longer exists on disk.
    fn fixture(ats_flags: u32) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let main = root.path().join("Steam");
        let second = root.path().join("Library2");
        let gone = root.path().join("Removed");
        for d in [&main, &second] {
            fs::create_dir_all(d.join("steamapps")).unwrap();
        }
        let esc = |p: &Path| p.display().to_string().replace('\\', "\\\\");
        let vdf = format!(
            "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t\t\"apps\"\n\t\t{{\n\t\t\t\"270880\"\t\t\"1\"\n\t\t}}\n\t}}\n\t\"1\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t\t\"apps\"\n\t\t{{\n\t\t\t\"2537590\"\t\t\"1\"\n\t\t}}\n\t}}\n\t\"2\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
            esc(&main),
            esc(&second),
            esc(&gone)
        );
        fs::write(main.join("steamapps/libraryfolders.vdf"), vdf).unwrap();
        let acf = |id: u32, name: &str, dir: &str, flags: u32, build: u64| {
            format!("\"AppState\"\n{{\n\t\"appid\"\t\t\"{id}\"\n\t\"universe\"\t\t\"1\"\n\t\"name\"\t\t\"{name}\"\n\t\"StateFlags\"\t\t\"{flags}\"\n\t\"installdir\"\t\t\"{dir}\"\n\t\"buildid\"\t\t\"{build}\"\n}}\n")
        };
        fs::write(main.join("steamapps/appmanifest_270880.acf"), acf(270880, "American Truck Simulator", "American Truck Simulator", ats_flags, 25952361)).unwrap();
        fs::write(second.join("steamapps/appmanifest_2537590.acf"), acf(2537590, "Microsoft Flight Simulator 2024", "Limitless", 4, 25941113)).unwrap();
        root
    }

    #[test]
    fn finds_games_across_libraries() {
        let root = fixture(4);
        let steam = SteamDir::from_dir(&root.path().join("Steam")).unwrap();
        let v = installs_in(&steam);
        let ats = v.iter().find(|i| i.game == Game::Ats);
        let msfs = v.iter().find(|i| i.game == Game::Msfs2024);
        if cfg!(any(windows, target_os = "linux")) {
            let ats = ats.expect("ATS found");
            assert!(ats.install_dir.ends_with("Steam/steamapps/common/American Truck Simulator"));
            assert_eq!(ats.steam_build_id, Some(25952361));
            assert!(ats.ready);
            assert_eq!(ats.version, None, "fixture EXE does not exist");
        }
        if cfg!(windows) {
            let msfs = msfs.expect("MSFS found on Windows");
            assert!(msfs.install_dir.ends_with("Library2/steamapps/common/Limitless"));
            assert!(msfs.exe.ends_with("Limitless/FlightSimulator2024.exe"));
        } else {
            assert!(msfs.is_none(), "MSFS is reported only where it runs natively");
        }
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn update_required_is_not_ready() {
        let root = fixture(6); // FullyInstalled + UpdateRequired
        let steam = SteamDir::from_dir(&root.path().join("Steam")).unwrap();
        let ats = installs_in(&steam).into_iter().find(|i| i.game == Game::Ats).unwrap();
        assert!(!ats.ready);
    }
}
