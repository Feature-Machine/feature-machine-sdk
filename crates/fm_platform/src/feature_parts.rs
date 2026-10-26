// Game-side parts of feature packages (feature packages plan, decisions 8 and 9): the ATS mod
// folder and the MSFS Community folder. Each copied file goes in the install record (game_files)
// with its feature id, so a removal deletes only files that Feature Machine wrote and that nobody
// changed since. A file of the same name from someone else stops the install. The caller makes
// sure that the game does not run, because a running game holds its files open.
use crate::game_files::{physical, sha256_file, Kept, Record, Records};
use crate::{Game, Source};
use std::io;
use std::path::{Path, PathBuf};

/// ATS mod folder: Documents\American Truck Simulator\mod (Windows) or
/// ~/.local/share/American Truck Simulator/mod (Linux), where the game reads mods.
pub fn ats_mod_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) { dirs::document_dir() } else { dirs::data_dir() };
    base.map(|d| d.join("American Truck Simulator").join("mod"))
}

/// UserCfg.opt of MSFS 2024: Steam and Store keep it in different folders.
pub fn msfs_user_cfg(source: Source) -> Option<PathBuf> {
    match source {
        Source::Steam => dirs::config_dir().map(|d| d.join("Microsoft Flight Simulator 2024").join("UserCfg.opt")),
        Source::MicrosoftStore => dirs::data_local_dir().map(|d| d.join("Packages").join("Microsoft.Limitless_8wekyb3d8bbwe").join("LocalCache").join("UserCfg.opt")),
    }
}

/// The InstalledPackagesPath line of UserCfg.opt: InstalledPackagesPath "F:\FS\MSFS 2024".
pub fn installed_packages_path(user_cfg: &str) -> Option<PathBuf> {
    user_cfg.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("InstalledPackagesPath")?.trim();
        let p = rest.strip_prefix('"')?.split('"').next()?;
        (!p.is_empty()).then(|| PathBuf::from(p))
    })
}

/// The Community folder of one MSFS 2024 install, from its UserCfg.opt.
pub fn msfs_community_dir(source: Source) -> Option<PathBuf> {
    let text = std::fs::read_to_string(msfs_user_cfg(source)?).ok()?;
    installed_packages_path(&text).map(|p| p.join("Community"))
}

/// One part to copy: every file under `from` goes to the same relative path under `to`.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub game: Game,
    pub from: PathBuf,
    pub to: PathBuf,
}

/// Lists the files under `dir`. file_type does not follow links: a link in a version folder
/// (packages have none) stops the install, so no file from outside the folder is copied.
fn files(base: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let (p, kind) = (e.path(), e.file_type()?);
        if kind.is_symlink() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{} is a link", p.display())));
        }
        if kind.is_dir() {
            files(base, &p, out)?;
        } else {
            out.push(p.strip_prefix(base).map_err(io::Error::other)?.to_path_buf());
        }
    }
    Ok(())
}

fn consent_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The result of an install: the files that are installed now, and the files of an earlier
/// version that stay (changed outside Feature Machine, or not removable now), as `remove` gives.
#[derive(Debug, Default, PartialEq)]
pub struct Installed {
    pub files: Vec<PathBuf>,
    pub kept: Vec<(PathBuf, Kept)>,
}

/// Copies the parts of a feature version. First checks every target, so a foreign or changed
/// file stops the install before any file moves. Then removes the files of an earlier version
/// that the new one does not have; one that cannot go is kept and listed, never an error, as
/// the new files are already in place (a downgrade must not stop half done, 7f defect B).
pub fn install(records: &mut Records, feature: &str, parts: &[Part], app_version: &str) -> io::Result<Installed> {
    let mut plan = vec![];
    // Each record is resolved once: physical() reads the disk, and a package can have many files.
    let mine: Vec<(PathBuf, Record)> = records.of_feature(feature).into_iter().map(|r| (physical(&r.path), r)).collect();
    for part in parts {
        let mut rel = vec![];
        files(&part.from, &part.from, &mut rel)?;
        rel.sort();
        for r in rel {
            let (src, dst) = (part.from.join(&r), part.to.join(&r));
            // Every record of this feature for the same physical file (an older install also
            // wrote it through a junction) can own it; one record with the file's hash is enough.
            let at = physical(&dst);
            let group: Vec<&Record> = mine.iter().filter(|(p, _)| *p == at).map(|(_, r)| r).collect();
            match sha256_file(&dst) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Ok(h) if group.iter().any(|r| r.owns(&h)) => {}
                Ok(_) if !group.is_empty() || records.get(&dst).is_some() => return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} was changed outside Feature Machine; it stays as it is", dst.display()))),
                Ok(_) => return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} is a file that Feature Machine did not install", dst.display()))),
                Err(e) => return Err(e),
            }
            plan.push((part.game, part.to.clone(), src, dst));
        }
    }
    let consent_unix = consent_now();
    let mut done = vec![];
    for (game, root, src, dst) in plan {
        if let Some(dir) = dst.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let rec = Record { game, install_dir: root, path: dst.clone(), sha256: String::new(), sha256_next: None, consent_unix, app_version: app_version.into(), feature: Some(feature.into()) };
        records.install(rec, &src)?;
        done.push(dst);
    }
    let old: Vec<Record> = records.of_feature(feature).into_iter().filter(|r| !done.contains(&r.path)).collect();
    let placed: Vec<PathBuf> = done.iter().map(|d| physical(d)).collect();
    let kept = remove_records(records, old, &placed);
    Ok(Installed { files: done, kept })
}

/// Records::remove_grouped (one physical file is one group, see there), then the empty package
/// folders above each removed file go too.
fn remove_records(records: &mut Records, list: Vec<Record>, placed: &[PathBuf]) -> Vec<(PathBuf, Kept)> {
    let (removed, kept) = records.remove_grouped(list, placed);
    for r in removed {
        remove_empty_dirs(&r.path, &r.install_dir);
    }
    kept
}

/// Removes the empty folders above a removed file, up to (not including) the part root.
fn remove_empty_dirs(file: &Path, root: &Path) {
    let mut dir = file.parent();
    while let Some(d) = dir {
        if d == root || !d.starts_with(root) || std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

/// Removes every recorded file of a feature that is still ours. Returns the files that stay.
pub fn remove(records: &mut Records, feature: &str) -> Vec<(PathBuf, Kept)> {
    let list = records.of_feature(feature);
    remove_records(records, list, &[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game_files::link_dir;

    fn tree(root: &Path, files: &[(&str, &str)]) {
        for (rel, text) in files {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
    }

    #[test]
    fn user_cfg_gives_the_packages_folder() {
        let cfg = "Version 1\nInstalledPackagesPath \"F:\\FS\\MSFS 2024\"\n";
        assert_eq!(installed_packages_path(cfg), Some(PathBuf::from("F:\\FS\\MSFS 2024")));
        assert_eq!(installed_packages_path("InstalledPackagesPath \"\""), None);
        assert_eq!(installed_packages_path("Other 1"), None);
    }

    #[test]
    fn install_update_and_remove_with_the_record() {
        let t = tempfile::tempdir().unwrap();
        let (v1, v2, community) = (t.path().join("v1"), t.path().join("v2"), t.path().join("Community"));
        tree(&v1, &[("fm-alert/layout.json", "1"), ("fm-alert/html_ui/a.js", "a"), ("fm-alert/old.txt", "o")]);
        tree(&v2, &[("fm-alert/layout.json", "2"), ("fm-alert/html_ui/a.js", "a2")]);
        tree(&community, &[("someone-else/manifest.json", "x")]);
        let mut rec = Records::load(t.path().join("game-files.json")).unwrap();
        let part = |from: &Path| vec![Part { game: Game::Msfs2024, from: from.into(), to: community.clone() }];
        assert_eq!(install(&mut rec, "speed-alert", &part(&v1), "0.1.0").unwrap().files.len(), 3);
        assert_eq!(std::fs::read_to_string(community.join("fm-alert/html_ui/a.js")).unwrap(), "a");
        // Update: new content, and the file that v2 dropped goes away.
        install(&mut rec, "speed-alert", &part(&v2), "0.1.0").unwrap();
        assert_eq!(std::fs::read_to_string(community.join("fm-alert/layout.json")).unwrap(), "2");
        assert!(!community.join("fm-alert/old.txt").exists());
        assert_eq!(rec.of_feature("speed-alert").len(), 2);
        // A file changed by the member stays, with its record; the rest goes, and so do empty folders.
        std::fs::write(community.join("fm-alert/layout.json"), "mine").unwrap();
        let kept = remove(&mut rec, "speed-alert");
        assert_eq!(kept, vec![(community.join("fm-alert/layout.json"), Kept::Changed)]);
        assert!(!community.join("fm-alert/html_ui").exists(), "empty folder removed");
        assert!(community.join("someone-else/manifest.json").exists(), "foreign files stay");
        assert!(Records::load(t.path().join("game-files.json")).unwrap().of_feature("speed-alert").len() == 1);
    }

    // 7f defect B: a downgrade drops a file that only the newer version has. When the member
    // changed it, it stays with its record and the install lists it; the rest installs.
    #[test]
    fn a_changed_file_that_the_older_version_lacks_stays_and_is_listed() {
        let t = tempfile::tempdir().unwrap();
        let (v1, v2, community) = (t.path().join("v1"), t.path().join("v2"), t.path().join("Community"));
        tree(&v1, &[("fm-alert/layout.json", "1")]);
        tree(&v2, &[("fm-alert/layout.json", "2"), ("fm-alert/new.txt", "n")]);
        let mut rec = Records::load(t.path().join("game-files.json")).unwrap();
        let part = |from: &Path| vec![Part { game: Game::Msfs2024, from: from.into(), to: community.clone() }];
        assert!(install(&mut rec, "speed-alert", &part(&v2), "0.1.0").unwrap().kept.is_empty());
        std::fs::write(community.join("fm-alert/new.txt"), "mine").unwrap();
        let back = install(&mut rec, "speed-alert", &part(&v1), "0.1.0").unwrap();
        assert_eq!(back.kept, vec![(community.join("fm-alert/new.txt"), Kept::Changed)]);
        assert_eq!(back.files, vec![community.join("fm-alert/layout.json")]);
        assert_eq!(std::fs::read_to_string(community.join("fm-alert/layout.json")).unwrap(), "1", "the older version is in place");
        assert_eq!(std::fs::read_to_string(community.join("fm-alert/new.txt")).unwrap(), "mine", "the member's change stays");
        assert_eq!(rec.of_feature("speed-alert").len(), 2, "its record stays, as remove keeps it");
    }

    // Live 2026-10-24: the Store Community folder was a junction to the Steam one, so an older
    // app placed and recorded each file under both paths, and a downgrade listed a changed file
    // twice. An update now keeps the file that it placed through the real path (the record under
    // the junction path only goes), and a changed file is listed once.
    #[test]
    fn a_second_path_through_a_junction_is_one_file() {
        let t = tempfile::tempdir().unwrap();
        let (v1, v2, community, store) = (t.path().join("v1"), t.path().join("v2"), t.path().join("Community"), t.path().join("StoreCommunity"));
        tree(&v1, &[("fm-alert/layout.json", "1"), ("fm-alert/a.css", "a")]);
        tree(&v2, &[("fm-alert/layout.json", "2")]);
        std::fs::create_dir_all(&community).unwrap();
        link_dir(&store, &community);
        assert_eq!(physical(&store), physical(&community));
        let mut rec = Records::load(t.path().join("game-files.json")).unwrap();
        let to = |dir: &Path, from: &Path| Part { game: Game::Msfs2024, from: from.into(), to: dir.into() };
        // The older app: one part for each path, so 4 records for 2 files.
        install(&mut rec, "speed-alert", &[to(&community, &v1), to(&store, &v1)], "0.1.0").unwrap();
        assert_eq!(rec.of_feature("speed-alert").len(), 4);
        std::fs::write(community.join("fm-alert/a.css"), "mine").unwrap();
        // The update through the real path only.
        let up = install(&mut rec, "speed-alert", &[to(&community, &v2)], "0.1.0").unwrap();
        assert_eq!(std::fs::read_to_string(community.join("fm-alert/layout.json")).unwrap(), "2", "not deleted by the old junction record");
        assert_eq!(up.kept, vec![(community.join("fm-alert/a.css"), Kept::Changed)], "listed once");
        let paths: Vec<PathBuf> = rec.of_feature("speed-alert").into_iter().map(|r| r.path).collect();
        assert_eq!(paths.len(), 2, "one record for each file: {paths:?}");
        assert!(paths.iter().all(|p| p.starts_with(&community)));
        assert!(remove(&mut rec, "speed-alert") == vec![(community.join("fm-alert/a.css"), Kept::Changed)]);
        assert!(!community.join("fm-alert/layout.json").exists());
    }

    // Review r2: the package folders are linked and no Community folder exists yet. Both paths
    // are still one place, so an install through one path and an update through the other keep
    // one record for each file.
    #[test]
    fn linked_package_folders_without_community_are_one_place() {
        let t = tempfile::tempdir().unwrap();
        let (v1, v2, steam, store) = (t.path().join("v1"), t.path().join("v2"), t.path().join("Packages"), t.path().join("StorePackages"));
        tree(&v1, &[("fm/x.txt", "1"), ("fm/y.txt", "y")]);
        tree(&v2, &[("fm/x.txt", "2"), ("fm/y.txt", "y")]);
        std::fs::create_dir_all(&steam).unwrap();
        link_dir(&store, &steam);
        let (a, b) = (steam.join("Community"), store.join("Community"));
        assert!(!a.exists());
        assert_eq!(physical(&a), physical(&b));
        assert_eq!(physical(&b.join("fm/x.txt")), physical(&a).join("fm").join("x.txt"));
        let mut rec = Records::load(t.path().join("game-files.json")).unwrap();
        let to = |dir: &Path, from: &Path| Part { game: Game::Msfs2024, from: from.into(), to: dir.into() };
        install(&mut rec, "f", &[to(&b, &v1)], "0.1.0").unwrap();
        install(&mut rec, "f", &[to(&a, &v2)], "0.1.0").unwrap();
        assert_eq!(std::fs::read_to_string(a.join("fm/x.txt")).unwrap(), "2");
        let paths: Vec<PathBuf> = rec.of_feature("f").into_iter().map(|r| r.path).collect();
        assert_eq!(paths, [a.join("fm/x.txt"), a.join("fm/y.txt")], "one record for each file");
    }

    // Review r1: two records of one file can have different hashes (an older update went through
    // one path only). For each path as the current one, and with the current record last (as
    // Records::put leaves it) or first (review r2), the record with the file's hash owns it: an
    // install through the stale path goes on, and a removal deletes the file and every record.
    // A file that no record owns is listed once and keeps one record.
    #[test]
    fn the_record_with_the_file_hash_owns_a_file_with_two_paths() {
        for (via_store, current_first) in [(false, false), (false, true), (true, false), (true, true)] {
            let case = format!("via_store {via_store}, current_first {current_first}");
            let t = tempfile::tempdir().unwrap();
            let (v1, v2, v3, community, store) = (t.path().join("v1"), t.path().join("v2"), t.path().join("v3"), t.path().join("Community"), t.path().join("StoreCommunity"));
            for (dir, text) in [(&v1, "1"), (&v2, "2"), (&v3, "3")] {
                tree(dir, &[("fm/x.txt", text)]);
            }
            std::fs::create_dir_all(&community).unwrap();
            link_dir(&store, &community);
            let file = t.path().join("game-files.json");
            let mut rec = Records::load(file.clone()).unwrap();
            let to = |dir: &Path, from: &Path| Part { game: Game::Msfs2024, from: from.into(), to: dir.into() };
            let (cur, stale) = if via_store { (&store, &community) } else { (&community, &store) };
            let legacy = |rec: &mut Records| {
                install(rec, "f", &[to(&community, &v1), to(&store, &v1)], "0.1.0").unwrap();
                let r = rec.get(&cur.join("fm/x.txt")).cloned().unwrap();
                rec.install(r, &v2.join("fm/x.txt")).unwrap();
                let mut list = rec.of_feature("f");
                if current_first {
                    list.reverse();
                    std::fs::write(&file, serde_json::to_vec(&list).unwrap()).unwrap();
                    *rec = Records::load(file.clone()).unwrap();
                }
                let at = if current_first { 0 } else { 1 };
                assert_eq!(rec.of_feature("f")[at].path, cur.join("fm/x.txt"), "{case}: the seeded order");
            };
            legacy(&mut rec);
            assert!(remove(&mut rec, "f").is_empty(), "{case}: ours");
            assert!(!community.join("fm/x.txt").exists() && rec.of_feature("f").is_empty(), "{case}: removed");
            legacy(&mut rec);
            install(&mut rec, "f", &[to(stale, &v3)], "0.1.0").unwrap();
            assert_eq!(std::fs::read_to_string(community.join("fm/x.txt")).unwrap(), "3", "{case}: the stale path installs");
            assert_eq!(rec.of_feature("f").len(), 1);
            assert!(remove(&mut rec, "f").is_empty());
            install(&mut rec, "f", &[to(&community, &v1), to(&store, &v1)], "0.1.0").unwrap();
            std::fs::write(community.join("fm/x.txt"), "mine").unwrap();
            assert_eq!(remove(&mut rec, "f"), vec![(community.join("fm/x.txt"), Kept::Changed)], "listed once");
            assert_eq!(rec.of_feature("f").len(), 1, "one record keeps the changed file");
        }
    }

    #[test]
    fn a_foreign_file_stops_the_install_before_any_copy() {
        let t = tempfile::tempdir().unwrap();
        let (v1, mods) = (t.path().join("v1"), t.path().join("mod"));
        tree(&v1, &[("a/manifest.sii", "m"), ("b.scs", "b")]);
        tree(&mods, &[("b.scs", "not ours")]);
        let mut rec = Records::load(t.path().join("game-files.json")).unwrap();
        let err = install(&mut rec, "speed-alert", &[Part { game: Game::Ats, from: v1, to: mods.clone() }], "0.1.0").unwrap_err();
        assert!(err.to_string().contains("did not install"), "{err}");
        assert!(!mods.join("a/manifest.sii").exists(), "nothing copied");
        assert_eq!(std::fs::read_to_string(mods.join("b.scs")).unwrap(), "not ours");
    }
}
