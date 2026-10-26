// Game-side files that Feature Machine installs (M5: the ATS plugin). The app changes only files
// in its install record (game-files.json in the app data folder), so a file of the same name from
// someone else is never removed. The record keeps the SHA-256: a file changed outside Feature
// Machine is not replaced or removed without a new question. A write goes to a temporary file
// in the same folder and then a rename, so the game never loads half a file.
use crate::files::write_replace;
use crate::Game;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io;
use std::path::{Path, PathBuf};

/// The ATS plugin file in the game, and the library that the plugin crate builds.
pub const ATS_PLUGIN_NAME: &str = "feature_machine";

/// Where the ATS plugin goes: <install>/bin/win_x64/plugins/feature_machine.dll on Windows,
/// bin/linux_x64/plugins/feature_machine.so on Linux (ATS runs natively on these two only).
pub fn ats_plugin_path(install_dir: &Path) -> PathBuf {
    let platform = if cfg!(windows) { "win_x64" } else { "linux_x64" };
    install_dir.join("bin").join(platform).join("plugins").join(format!("{ATS_PLUGIN_NAME}{}", std::env::consts::DLL_SUFFIX))
}

/// File name of the plugin library in a build or app folder (Cargo adds "lib" on Linux).
pub fn ats_plugin_build_name() -> String {
    format!("{}{ATS_PLUGIN_NAME}{}", std::env::consts::DLL_PREFIX, std::env::consts::DLL_SUFFIX)
}

/// One installed file, with the user's consent (time and app version) as proof.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub game: Game,
    pub install_dir: PathBuf,
    pub path: PathBuf,
    pub sha256: String,
    /// Set only while a new file goes in (two phases, see Records::install): the file on disk
    /// is ours with either hash, so a stop between the rename and the last save loses nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256_next: Option<String>,
    pub consent_unix: u64,
    pub app_version: String,
    /// The feature package that brought the file (ATS mod, MSFS Community package); None for
    /// the ATS plugin of the app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
}

impl Record {
    /// True when `hash` is a file that Feature Machine wrote at this path.
    pub fn owns(&self, hash: &str) -> bool {
        self.sha256 == hash || self.sha256_next.as_deref() == Some(hash)
    }
}

/// State of one game-side file, from the record, the file on disk and the file the app ships.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileState {
    NotInstalled,
    Installed,
    /// Ours, but the app ships a different file: replaced when the game does not run.
    UpdateWaiting,
    /// In the record, but the hash differs: changed outside Feature Machine.
    Changed,
    /// A file with this name that Feature Machine did not install.
    Foreign,
}

impl FileState {
    /// Our own file, not changed by someone else (only the shipped version can differ).
    pub fn is_ours(self) -> bool {
        matches!(self, FileState::Installed | FileState::UpdateWaiting)
    }
}

pub fn state(record: Option<&Record>, on_disk: Option<&str>, shipped: Option<&str>) -> FileState {
    match (record, on_disk) {
        (_, None) => FileState::NotInstalled,
        (None, Some(_)) => FileState::Foreign,
        (Some(r), Some(d)) if !r.owns(d) => FileState::Changed,
        (Some(_), Some(d)) if shipped.is_some_and(|s| s != d) => FileState::UpdateWaiting,
        _ => FileState::Installed,
    }
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut hasher = Sha256::new();
    io::copy(&mut std::fs::File::open(path)?, &mut hasher)?;
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The physical place of a path: links and junctions of its nearest existing folder resolved,
/// then the missing rest added (a Community folder that does not exist yet). Two MSFS installs
/// can name one folder (on one test PC the Store Community is a junction to the Steam one), so
/// path text alone placed and recorded each file twice.
pub fn physical(p: &Path) -> PathBuf {
    let (mut at, mut rest) = (p, vec![]);
    loop {
        if let Ok(real) = std::fs::canonicalize(at) {
            return rest.iter().rev().fold(real, |acc, name| acc.join(name));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                at = parent;
            }
            _ => return p.to_path_buf(),
        }
    }
}

/// The install record. load() fails on a damaged file instead of starting empty, because an
/// empty record would make every installed file look foreign (and so never removable).
#[derive(Debug)]
pub struct Records {
    file: PathBuf,
    list: Vec<Record>,
}

impl Records {
    /// game-files.json in the app data folder.
    pub fn default_file() -> Option<PathBuf> {
        crate::data_dir().map(|d| d.join("game-files.json"))
    }

    pub fn load(file: PathBuf) -> io::Result<Self> {
        let list = match std::fs::read(&file) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", file.display())))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => vec![],
            Err(e) => return Err(e),
        };
        Ok(Self { file, list })
    }

    pub fn get(&self, path: &Path) -> Option<&Record> {
        self.list.iter().find(|r| r.path == path)
    }

    /// The files of one feature package.
    pub fn of_feature(&self, feature: &str) -> Vec<Record> {
        self.list.iter().filter(|r| r.feature.as_deref() == Some(feature)).cloned().collect()
    }

    fn save(&self) -> io::Result<()> {
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec_pretty(&self.list).map_err(io::Error::other)?;
        write_replace(&self.file, &bytes)
    }

    fn put(&mut self, record: Record) {
        self.list.retain(|r| r.path != record.path);
        self.list.push(record);
    }

    /// Copies `source` to `record.path`. Phase 1 saves the record with the old and the new hash
    /// before the rename, phase 2 keeps only the new hash. A failed phase 2 save is repaired by
    /// settle() on the next check. A failed write (the game holds the file) restores the old
    /// record. The game must not run.
    pub fn install(&mut self, mut record: Record, source: &Path) -> io::Result<()> {
        let bytes = std::fs::read(source)?;
        let new = hex(&Sha256::digest(&bytes));
        let prev = self.get(&record.path).cloned();
        record.sha256 = prev.as_ref().map_or_else(|| new.clone(), |p| p.sha256.clone());
        record.sha256_next = Some(new.clone());
        self.put(record.clone());
        self.save()?;
        if let Err(e) = write_replace(&record.path, &bytes) {
            match prev {
                Some(p) => self.put(p),
                None => self.list.retain(|r| r.path != record.path),
            }
            let _ = self.save();
            return Err(e);
        }
        let _ = self.settle(&record.path, Some(&new));
        Ok(())
    }

    /// Ends an install that stopped between its two phases: keeps the hash of the file on disk
    /// when it is one of the two recorded hashes. Saves only when the record changes.
    pub fn settle(&mut self, path: &Path, on_disk: Option<&str>) -> io::Result<()> {
        let Some(r) = self.list.iter_mut().find(|r| r.path == path) else { return Ok(()) };
        let Some(next) = r.sha256_next.take() else { return Ok(()) };
        if on_disk == Some(next.as_str()) {
            r.sha256 = next;
        }
        self.save()
    }

    /// Deletes a recorded file and its record. A file that is not in the record is refused.
    pub fn remove(&mut self, path: &Path) -> io::Result<()> {
        if self.get(path).is_none() {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Feature Machine did not install this file"));
        }
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        self.forget(path)
    }

    /// Drops the record of a file that is gone (the user or Steam deleted it).
    pub fn forget(&mut self, path: &Path) -> io::Result<()> {
        let before = self.list.len();
        self.list.retain(|r| r.path != path);
        if self.list.len() != before {
            self.save()?;
        }
        Ok(())
    }

    /// App uninstall (M7): deletes every recorded file that is still ours. A file changed outside
    /// Feature Machine stays, and so does one record of it. Returns the files that stay and why:
    /// the uninstaller asks the user to close the game only for an error (a running game holds
    /// its plugin open), because a retry cannot help a changed file.
    pub fn remove_all_ours(&mut self) -> Vec<(PathBuf, Kept)> {
        let list = self.list.clone();
        self.remove_grouped(list, &[]).1
    }

    /// Removes the files of these records that are still ours. Records are grouped by physical
    /// file first (an older app recorded one file through a junction too, maybe with another
    /// hash): a group in `placed` only leaves the list; any record with the file's hash owns it;
    /// a changed file keeps one record and is listed once. Returns the removed records and the
    /// files that stay.
    pub fn remove_grouped(&mut self, list: Vec<Record>, placed: &[PathBuf]) -> (Vec<Record>, Vec<(PathBuf, Kept)>) {
        let mut groups: Vec<(PathBuf, Vec<Record>)> = vec![];
        for r in list {
            let at = physical(&r.path);
            match groups.iter_mut().find(|(p, _)| *p == at) {
                Some((_, g)) => g.push(r),
                None => groups.push((at, vec![r])),
            }
        }
        let (mut removed, mut kept) = (vec![], vec![]);
        for (at, group) in groups {
            let result = if placed.contains(&at) { self.forget_all(&group).map(|()| None) } else { self.remove_group(&group, &mut removed) };
            match result {
                Ok(None) => {}
                Ok(Some(k)) => kept.push((group[0].path.clone(), k)),
                Err(e) => kept.push((group[0].path.clone(), Kept::Error(e.to_string()))),
            }
        }
        (removed, kept)
    }

    fn forget_all(&mut self, group: &[Record]) -> io::Result<()> {
        group.iter().try_for_each(|r| self.forget(&r.path))
    }

    /// One physical file and all its records: gone (records go), ours (the file and records go),
    /// or changed (the first record stays, the others go).
    fn remove_group(&mut self, group: &[Record], removed: &mut Vec<Record>) -> io::Result<Option<Kept>> {
        match sha256_file(&group[0].path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => self.forget_all(group).map(|()| None),
            Err(e) => Err(e),
            Ok(h) => match group.iter().find(|r| r.owns(&h)) {
                Some(owner) => {
                    self.remove(&owner.path)?;
                    removed.push(owner.clone());
                    self.forget_all(group).map(|()| None)
                }
                None => self.forget_all(&group[1..]).map(|()| Some(Kept::Changed)),
            },
        }
    }
}

/// Why remove_all_ours left a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kept {
    /// Changed outside Feature Machine: never removed without a question.
    Changed,
    /// Could not be removed now (the game holds it, or a disk error).
    Error(String),
}

/// A folder link for tests: a junction on Windows (as the MSFS Store Community folder on
/// one test PC), a symlink elsewhere.
#[cfg(test)]
pub(crate) fn link_dir(link: &Path, target: &Path) {
    #[cfg(windows)]
    assert!(std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(link).arg(target).output().unwrap().status.success());
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(dir: &Path) -> Record {
        Record {
            game: Game::Ats,
            install_dir: dir.into(),
            path: dir.join("feature_machine.dll"),
            sha256: String::new(),
            sha256_next: None,
            consent_unix: 1,
            app_version: "0.1.0".into(),
            feature: None,
        }
    }

    fn rec(hash: &str, next: Option<&str>) -> Record {
        Record { sha256: hash.into(), sha256_next: next.map(Into::into), ..record(Path::new("/g")) }
    }

    #[test]
    fn states_cover_every_case() {
        use FileState::*;
        let a = rec("a", None);
        assert_eq!(state(None, None, Some("a")), NotInstalled);
        assert_eq!(state(Some(&a), None, Some("a")), NotInstalled);
        assert_eq!(state(None, Some("a"), Some("a")), Foreign);
        assert_eq!(state(Some(&a), Some("b"), Some("b")), Changed);
        assert_eq!(state(Some(&a), Some("a"), Some("b")), UpdateWaiting);
        assert_eq!(state(Some(&a), Some("a"), Some("a")), Installed);
        assert_eq!(state(Some(&a), Some("a"), None), Installed, "a build with no plugin does not ask for an update");
        // Between the two phases of an update, the old and the new file are both ours.
        assert_eq!(state(Some(&rec("a", Some("b"))), Some("b"), Some("b")), Installed);
        assert_eq!(state(Some(&rec("a", Some("b"))), Some("a"), Some("b")), UpdateWaiting);
        assert_eq!(state(Some(&rec("a", Some("b"))), Some("c"), Some("b")), Changed);
    }

    #[test]
    fn plugin_path_is_in_the_game_plugins_folder() {
        let p = ats_plugin_path(Path::new("/games/ats"));
        let plugins = if cfg!(windows) { "win_x64" } else { "linux_x64" };
        assert_eq!(p.parent().unwrap(), Path::new("/games/ats").join("bin").join(plugins).join("plugins"));
        assert!(p.file_name().unwrap().to_string_lossy().starts_with("feature_machine."));
    }

    #[test]
    fn install_records_the_hash_and_remove_deletes_only_recorded_files() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dll");
        std::fs::write(&src, b"plugin v1").unwrap();
        let mut recs = Records::load(dir.path().join("data").join("game-files.json")).unwrap();
        // The plugins folder does not exist yet (a game with no other plugin).
        let rec = record(&dir.path().join("plugins"));
        recs.install(rec.clone(), &src).unwrap();
        let hash = sha256_file(&rec.path).unwrap();
        assert_eq!(recs.get(&rec.path).unwrap().sha256, hash);
        assert_eq!(recs.get(&rec.path).unwrap().sha256_next, None, "phase 2 is done");
        assert!(!dir.path().join("plugins").join("feature_machine.dll.fm-new").exists(), "no temporary file stays");
        // The record survives a restart of the app.
        let mut recs = Records::load(dir.path().join("data").join("game-files.json")).unwrap();
        assert_eq!(recs.get(&rec.path).unwrap().sha256, hash);
        let foreign = dir.path().join("other.dll");
        std::fs::write(&foreign, b"not ours").unwrap();
        assert_eq!(recs.remove(&foreign).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert!(foreign.exists());
        recs.remove(&rec.path).unwrap();
        assert!(!rec.path.exists() && recs.get(&rec.path).is_none());
    }

    // An update whose write fails (ATS holds the DLL open on Windows; here the target is a
    // folder) keeps the old record, and a stop between the phases settles to the file on disk.
    #[test]
    fn a_failed_update_keeps_the_old_record_and_settle_ends_a_stopped_one() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data").join("game-files.json");
        let mut recs = Records::load(file.clone()).unwrap();
        let blocked = Record { path: dir.path().join("blocked"), ..record(dir.path()) };
        std::fs::create_dir_all(blocked.path.join("child")).unwrap();
        recs.put(Record { sha256: "old".into(), ..blocked.clone() });
        let src = dir.path().join("src.dll");
        std::fs::write(&src, b"plugin v2").unwrap();
        assert!(recs.install(blocked.clone(), &src).is_err());
        let kept = recs.get(&blocked.path).unwrap();
        assert_eq!((kept.sha256.as_str(), kept.sha256_next.as_deref()), ("old", None));

        let path = dir.path().join("feature_machine.dll");
        recs.put(Record { path: path.clone(), ..rec("old", Some("new")) });
        recs.settle(&path, Some("new")).unwrap();
        let r = Records::load(file).unwrap().get(&path).cloned().unwrap();
        assert_eq!((r.sha256.as_str(), r.sha256_next), ("new", None));
    }

    // Uninstall: our file goes, a changed file and its record stay, a file already gone only
    // loses its record.
    #[test]
    fn remove_all_ours_keeps_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data").join("game-files.json");
        let mut recs = Records::load(file.clone()).unwrap();
        let src = dir.path().join("src.dll");
        std::fs::write(&src, b"plugin").unwrap();
        let ours = record(&dir.path().join("a"));
        let changed = record(&dir.path().join("b"));
        recs.install(ours.clone(), &src).unwrap();
        recs.install(changed.clone(), &src).unwrap();
        std::fs::write(&changed.path, b"edited by the user").unwrap();
        let gone = Record { path: dir.path().join("gone.dll"), ..rec("x", None) };
        recs.put(gone.clone());
        let kept = recs.remove_all_ours();
        assert_eq!(kept, [(changed.path.clone(), Kept::Changed)]);
        assert!(!ours.path.exists() && changed.path.exists());
        let recs = Records::load(file).unwrap();
        assert!(recs.get(&ours.path).is_none() && recs.get(&gone.path).is_none());
        assert!(recs.get(&changed.path).is_some());
    }

    // Live 2026-10-24: an older app recorded one file under two paths (the MSFS Store Community
    // folder is a junction to the Steam one), and an update through one path left the other
    // record with the old hash. In both record orders, uninstall deletes the file when one
    // record owns it, and lists a changed file once with one record.
    #[test]
    fn remove_all_ours_counts_two_paths_of_one_file_once() {
        for (owner_first, changed) in [(false, false), (true, false), (false, true), (true, true)] {
            let case = format!("owner_first {owner_first}, changed {changed}");
            let dir = tempfile::tempdir().unwrap();
            let (real, link) = (dir.path().join("Community"), dir.path().join("StoreCommunity"));
            std::fs::create_dir_all(&real).unwrap();
            link_dir(&link, &real);
            let (v1, v2) = (dir.path().join("v1"), dir.path().join("v2"));
            std::fs::write(&v1, b"1").unwrap();
            std::fs::write(&v2, b"2").unwrap();
            let file = dir.path().join("game-files.json");
            let mut recs = Records::load(file.clone()).unwrap();
            let (a, b) = (record(&real), record(&link));
            recs.install(a.clone(), &v1).unwrap();
            recs.install(b.clone(), &v1).unwrap();
            recs.install(a.clone(), &v2).unwrap();
            if owner_first {
                recs.put(recs.get(&b.path).cloned().unwrap());
            }
            assert_eq!(recs.list[usize::from(!owner_first)].path, a.path, "{case}: the seeded order");
            if changed {
                std::fs::write(&a.path, b"mine").unwrap();
            }
            let kept = recs.remove_all_ours();
            let left = Records::load(file).unwrap().list.len();
            if changed {
                assert_eq!(kept.iter().map(|k| &k.1).collect::<Vec<_>>(), [&Kept::Changed], "{case}: listed once");
                assert!(a.path.exists() && left == 1, "{case}: the file and one record stay");
            } else {
                assert!(kept.is_empty(), "{case}: {kept:?}");
                assert!(!a.path.exists() && left == 0, "{case}: the file and both records go");
            }
        }
    }

    #[test]
    fn a_damaged_record_is_an_error_not_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("game-files.json");
        std::fs::write(&file, b"{ not json").unwrap();
        assert_eq!(Records::load(file).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}
