mod representation;
// Install pattern of the desktop app (plan M8): every version in its own folder under
// <root>/versions, and state.json names the active and the previous one. A new version is
// unpacked to a staging folder, marked complete, renamed, and only then made active with one
// atomic file replace, so a stop at any point leaves a version that starts. OS-neutral: the
// setup exe (installer/) and the app updater (app/src-tauri/src/update.rs) both use it.
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};

pub mod feature;
pub mod package;
pub mod release;
pub mod memory;
pub mod module;
pub mod notice;
pub mod policy;
pub mod signed_set;
pub mod surfaces;
pub mod views;
#[cfg(any(test, feature = "testing"))]
pub mod test_signing;

const STATE: &str = "state.json";
/// The file that marks a folder as an install root (the uninstaller checks it before it deletes).
pub const STATE_FILE: &str = STATE;
const VERSIONS: &str = "versions";
const COMPLETE: &str = ".fm-complete";
/// The package digest of an unpacked feature version, written by the app after the unpack, so a
/// second install of one version number must be the same package. Like `COMPLETE` it belongs to
/// the version store, so the signed-file inventory of a memory session allows it (2026-10-20: an
/// app install of mirage 0.2.5 was refused with "unsigned installed file: package.sha256").
pub const PACKAGE_STAMP: &str = "package.sha256";
const STAGING: &str = ".new-";

/// The app exe inside a version folder.
pub fn app_exe_name() -> String {
    format!("fm_app{}", std::env::consts::EXE_SUFFIX)
}

/// The file a feature package must hold (manifest). The feature store checks it like the app
/// store checks the exe.
pub const FEATURE_MANIFEST: &str = "feature.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub active: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
}

/// A version string that is safe as a folder name and parses as semver.
pub fn check_version(v: &str) -> io::Result<semver::Version> {
    let ok = !v.is_empty() && v.len() <= 64 && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
    let parsed = ok.then(|| semver::Version::parse(v).ok()).flatten();
    parsed.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("not a version: {v:?}")))
}

/// What a store holds. The app (M8) needs its exe in each version and copies the launcher at a
/// switch; a feature (feature packages plan) needs its manifest and has no launcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    App,
    Feature,
    /// A module package (module layer plan 8.1): module.json and its maps/ folder only.
    Module,
}

impl Kind {
    fn required_file(self) -> String {
        match self {
            Kind::App => app_exe_name(),
            Kind::Feature => FEATURE_MANIFEST.to_string(),
            Kind::Module => module::MANIFEST.to_string(),
        }
    }
}

/// Every entry of a package zip, by index, as (relative path, is a folder). One check for the
/// consent (feature::inspect) and the unpack, so what the member agreed to is what lands on disk:
/// a safe "/" path (feature::safe_rel), no symlink, no path twice (case does not count, as on
/// Windows), and in a feature package only feature.json and the part folders at the top.
pub(crate) fn checked_entries<R: Read + Seek>(a: &mut zip::ZipArchive<R>, kind: Kind) -> io::Result<Vec<(PathBuf, bool)>> {
    let bad = |m: String| io::Error::new(io::ErrorKind::InvalidData, m);
    let mut seen = std::collections::HashSet::new();
    (0..a.len())
        .map(|i| {
            let e = a.by_index_raw(i).map_err(io::Error::other)?;
            let (name, is_dir) = (e.name().to_string(), e.is_dir());
            if e.is_symlink() {
                return Err(bad(format!("the package has a link: {name:?}")));
            }
            let rel = feature::safe_rel(name.strip_suffix('/').unwrap_or(&name))?;
            let top = rel.components().next().and_then(|c| c.as_os_str().to_str()).unwrap_or_default().to_string();
            let file = [FEATURE_MANIFEST, memory::SESSION, memory::SIGNATURE].contains(&top.as_str()) && !is_dir && rel.components().count() == 1;
            if kind == Kind::Feature && !(feature::parts::DIRS.contains(&top.as_str()) || file) {
                return Err(bad(format!("not a feature package part: {name:?}")));
            }
            let module_part = (top == module::MANIFEST && !is_dir && rel.components().count() == 1) || (top == "maps" && (is_dir || rel.components().count() > 1));
            if kind == Kind::Module && !module_part {
                return Err(bad(format!("not a module package part: {name:?}")));
            }
            if !seen.insert(rel.to_string_lossy().to_lowercase()) {
                return Err(bad(format!("the package has {name:?} twice")));
            }
            Ok((rel, is_dir))
        })
        .collect()
}

pub struct Install {
    root: PathBuf,
    kind: Kind,
}

impl Install {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), kind: Kind::App }
    }

    /// The store of one feature: <data>/features/<id>.
    pub fn feature(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), kind: Kind::Feature }
    }

    /// The store of one module: <data>/modules/store/<id>. It has no active version: each kept
    /// lock names the versions that it needs (module::prune).
    pub fn module(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), kind: Kind::Module }
    }

    /// The folders in versions/ that `keep` does not name (staging folders too): what a module
    /// prune removes. Err: the folder or one entry does not read, so the prune removes nothing
    /// (Sol r6 R6-1). A missing folder has none.
    pub fn stale(&self, keep: &[String]) -> Result<Vec<PathBuf>, String> {
        let dir = self.root.join(VERSIONS);
        let list = entries(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(list.into_iter().filter(|p| !keep.iter().any(|k| p.file_name().is_some_and(|n| n == k.as_str()))).collect())
    }

    /// The active complete version and its folder. A read error reads as none: for views only.
    /// The module lock uses active_checked (Sol r7 follow-up).
    pub fn active(&self) -> Option<(String, PathBuf)> {
        self.active_checked().ok().flatten()
    }

    /// active() that keeps a read error. Ok(None): not installed, a damaged state.json (the
    /// setup repairs it, and the hosts read it as none too) or an active version that is not
    /// complete. Err: state.json or the version marker does not read.
    pub fn active_checked(&self) -> io::Result<Option<(String, PathBuf)>> {
        let s = match self.state() {
            Ok(Some(s)) => s,
            Ok(None) => return Ok(None),
            Err(e) if e.kind() == io::ErrorKind::InvalidData => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(self.complete(&s.active)?.then(|| (s.active.clone(), self.version_dir(&s.active))))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The install that holds `exe` when it is <root>/versions/<version>/fm_app: an installed
    /// app finds its root this way. A dev build (target/debug) gives None.
    pub fn of_app_exe(exe: &Path) -> Option<(Self, String)> {
        let dir = exe.parent()?;
        let version = dir.file_name()?.to_str()?.to_string();
        let versions = dir.parent()?;
        if versions.file_name()? != VERSIONS || check_version(&version).is_err() {
            return None;
        }
        let install = Self::new(versions.parent()?);
        install.root.join(STATE).is_file().then_some((install, version))
    }

    pub fn version_dir(&self, version: &str) -> PathBuf {
        self.root.join(VERSIONS).join(version)
    }

    /// complete() with a stat error read as false: each caller then refuses (not installed).
    /// One rule for both (Sol r8 R8-2).
    pub fn is_complete(&self, version: &str) -> bool {
        self.complete(version).unwrap_or(false)
    }

    /// Ok(true) when `version` is a complete version folder: its marker is a file (a folder of
    /// that name is not one, Sol r8 R8-2). A missing marker is incomplete. Err: the marker does
    /// not stat; the error names it (R8-3).
    pub fn complete(&self, version: &str) -> io::Result<bool> {
        if check_version(version).is_err() { return Ok(false); }
        let marker = self.version_dir(version).join(COMPLETE);
        match std::fs::metadata(&marker) {
            Ok(m) => Ok(m.is_file()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(io::Error::new(e.kind(), format!("{}: {e}", marker.display()))),
        }
    }

    /// None before the first install. A damaged file is an error: the setup repairs it.
    pub fn state(&self) -> io::Result<Option<State>> {
        let path = self.root.join(STATE);
        match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            // The path in the error: the card shows it (Sol r8 R8-3).
            Err(e) => Err(io::Error::new(e.kind(), format!("{}: {e}", path.display()))),
        }
    }

    /// The app exe of the active version, when that version is complete.
    pub fn active_exe(&self) -> Option<PathBuf> {
        let s = self.state().ok()??;
        self.is_complete(&s.active).then(|| self.version_dir(&s.active).join(app_exe_name()))
    }

    fn write_state(&self, s: &State) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(s).map_err(io::Error::other)?;
        fm_platform::files::write_replace(&self.root.join(STATE), &bytes)
    }

    /// Unpacks a package zip into versions/<version>. A complete folder is kept unless `repair`.
    /// The staging folder gets the complete marker last, then one rename makes it the version.
    pub fn unpack<R: Read + Seek>(&self, version: &str, zip: R, repair: bool) -> io::Result<()> {
        check_version(version)?;
        if self.is_complete(version) && !repair {
            return Ok(());
        }
        let versions = self.root.join(VERSIONS);
        std::fs::create_dir_all(&versions)?;
        // A staging folder of its own (process and counter): two unpacks of one version never
        // write into or delete each other's folder. prune removes a folder that a stop left.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let staging = versions.join(format!("{STAGING}{version}-{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        let mut archive = zip::ZipArchive::new(zip).map_err(io::Error::other)?;
        let entries = checked_entries(&mut archive, self.kind)?;
        if self.kind == Kind::Feature { memory::verify_install(&mut archive, version).map_err(io::Error::other)?; }
        if self.kind == Kind::Module { module::verify_archive(&mut archive, version).map_err(io::Error::other)?; }
        for (i, (rel, is_dir)) in entries.into_iter().enumerate() {
            let path = staging.join(rel);
            if is_dir {
                std::fs::create_dir_all(&path)?;
                continue;
            }
            std::fs::create_dir_all(path.parent().unwrap_or(&staging))?;
            let mut e = archive.by_index(i).map_err(io::Error::other)?;
            io::copy(&mut e, &mut std::fs::File::create(&path)?)?;
            // The Linux app and launcher need their exec bit (what extract() used to set).
            #[cfg(unix)]
            if let Some(mode) = e.unix_mode() {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode & 0o777))?;
            }
        }
        let required = self.kind.required_file();
        if !staging.join(&required).is_file() {
            remove_dir(&staging)?;
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("the package has no {required}")));
        }
        std::fs::File::create(staging.join(COMPLETE))?.write_all(version.as_bytes())?;
        let target = self.version_dir(version);
        // Another unpack completed this version meanwhile (before or during the rename): keep
        // it, as at the start, and drop this staging folder.
        if !target.exists() {
            return match std::fs::rename(&staging, &target) {
                Err(_) if !repair && self.is_complete(version) => remove_dir(&staging),
                Err(_) => rename_retry(&staging, &target),
                ok => ok,
            };
        }
        if !repair && self.is_complete(version) {
            return remove_dir(&staging);
        }
        // A repair (the setup closed the app first): the old folder moves aside, the new one
        // takes its place, and the old one goes at the next prune. If the second rename fails
        // (a scanner holds a new file), the old folder comes back, so the version never goes.
        let aside = versions.join(format!("{STAGING}old-{version}-{}", std::process::id()));
        rename_retry(&target, &aside)?;
        rename_retry(&staging, &target).inspect_err(|_| {
            let _ = std::fs::rename(&aside, &target);
        })
    }

    /// Makes a complete version active; the old active one becomes the previous. Prunes after.
    pub fn activate(&self, version: &str) -> io::Result<State> {
        let s = self.activate_keep(version)?;
        self.prune();
        Ok(s)
    }

    /// activate() with no prune: a feature version that a running game still has loaded must
    /// stay until that game ends (the caller prunes then).
    pub fn activate_keep(&self, version: &str) -> io::Result<State> {
        self.activate_with(version, true)
    }

    /// A recovery from a pulled version (the update endpoint's rollback flag): no version stays
    /// as the previous, so "Switch to version" never offers a pulled one (live finding 3; the old
    /// previous can be pulled too, Sol review r2). No prune: the pulled version runs now; the
    /// prune of the next update removes it.
    pub fn activate_dropping_active(&self, version: &str) -> io::Result<State> {
        self.activate_with(version, false)
    }

    fn activate_with(&self, version: &str, keep_active: bool) -> io::Result<State> {
        if !self.is_complete(version) {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("version {version} is not installed")));
        }
        // A state that does not read stops the switch: the prune after it would remove the
        // version that it names (Sol r7 sweep). A damaged one names nothing; it is replaced.
        let old = match self.state() {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::InvalidData => None,
            Err(e) => return Err(e),
        };
        let old_active = old.as_ref().map(|s| s.active.clone());
        let previous = match old {
            _ if !keep_active => None,
            Some(s) if s.active != version => Some(s.active),
            Some(s) => s.previous,
            None => None,
        };
        let from = old_active.as_deref();
        let s = State { active: version.to_string(), previous: previous.filter(|p| self.is_complete(p)) };
        self.switch_to(&s, from)?;
        Ok(s)
    }

    /// Manual rollback (user decision): the previous version becomes active, the active one
    /// becomes the previous, so a second rollback goes forward again.
    pub fn rollback(&self) -> io::Result<State> {
        let s = self.state()?.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "not installed"))?;
        let prev = s.previous.filter(|p| self.is_complete(p)).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no previous version"))?;
        let s = State { active: prev, previous: Some(s.active) };
        self.switch_to(&s, s.previous.as_deref())?;
        Ok(s)
    }

    /// Each package carries the launcher (fm_setup) of its version; it goes to the root before
    /// the state switch, so launcher fixes reach members with updates. A version without one (an
    /// old package) keeps the launcher that is there. A switch to a version older than `from`
    /// keeps the newer launcher (its uninstall fixes, Sol r6) and puts it in the target folder too,
    /// because the older app's own switches copy the launcher of that folder (Sol r8).
    fn switch_to(&self, s: &State, from: Option<&str>) -> io::Result<()> {
        let back = from.is_some_and(|f| matches!((semver::Version::parse(&s.active), semver::Version::parse(f)), (Ok(to), Ok(f)) if to < f));
        let name = fm_platform::install::launcher_name();
        match (self.kind == Kind::App, back) {
            (true, false) => {
                self.copy_launcher(&s.active)?;
            }
            (true, true) if self.root.join(name).is_file() => fm_platform::files::write_replace(&self.version_dir(&s.active).join(name), &std::fs::read(self.root.join(name))?)?,
            _ => {}
        }
        self.write_state(s)
    }

    /// Puts the active version's launcher in the root again (setup repair). False: that
    /// version has none, so the caller writes one.
    pub fn restore_launcher(&self) -> io::Result<bool> {
        match self.state()? {
            Some(s) => self.copy_launcher(&s.active),
            None => Ok(false),
        }
    }

    fn copy_launcher(&self, version: &str) -> io::Result<bool> {
        let name = fm_platform::install::launcher_name();
        let shipped = self.version_dir(version).join(name);
        if !shipped.is_file() {
            return Ok(false);
        }
        let to = self.root.join(name);
        fm_platform::files::write_replace(&to, &std::fs::read(&shipped)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o755))?;
        }
        Ok(true)
    }

    /// Removes every folder in versions/ that is not active or previous (staging and old
    /// folders too). A folder in use stays until the next prune.
    /// The complete versions in the store (a game can still run one that is not active).
    pub fn versions(&self) -> Vec<String> {
        let Ok(dir) = std::fs::read_dir(self.root.join(VERSIONS)) else { return vec![] };
        let mut out: Vec<String> = dir.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|v| self.is_complete(v)).collect();
        out.sort();
        out
    }

    pub fn prune(&self) {
        let keep: Vec<String> = match self.state() {
            Ok(Some(s)) => std::iter::once(s.active).chain(s.previous).collect(),
            _ => return,
        };
        let Ok(dir) = std::fs::read_dir(self.root.join(VERSIONS)) else { return };
        for e in dir.flatten() {
            if !keep.iter().any(|k| e.file_name() == k.as_str()) {
                let _ = remove_dir(&e.path());
            }
        }
    }
}

/// A virus scanner can hold a new exe for a moment (Windows): retry a rename for 3 seconds.
fn rename_retry(from: &Path, to: &Path) -> io::Result<()> {
    let mut tries = 0;
    loop {
        match std::fs::rename(from, to) {
            Err(_) if tries < 10 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
            r => return r,
        }
    }
}

/// The paths in folder `dir`. A missing folder has none. Any other error of the folder or of
/// one entry is returned, so a prune never acts on a part of the list (Sol r6 R6-1).
pub(crate) fn entries(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let list = match std::fs::read_dir(dir) {
        Ok(l) => l,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e),
    };
    list.map(|e| {
        #[cfg(test)]
        let e = tests::bad_entry(e);
        Ok(e?.path())
    })
    .collect()
}

/// Removes a file. A file that is already gone is no error.
pub(crate) fn remove_file(p: &Path) -> io::Result<()> {
    match std::fs::remove_file(p) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

pub(crate) fn remove_dir(p: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(p) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests: the entry at this path does not read (Sol r6 R6-1, an entry error of read_dir).
    thread_local! {
        pub(crate) static BAD_ENTRY: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    }

    pub(crate) fn bad_entry(e: io::Result<std::fs::DirEntry>) -> io::Result<std::fs::DirEntry> {
        match e {
            Ok(d) if BAD_ENTRY.with(|b| b.borrow().as_deref() == Some(d.path().as_path())) => Err(io::Error::other("test: an entry that does not read")),
            e => e,
        }
    }

    pub(crate) fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut out);
        for (name, data) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
        out.into_inner()
    }

    fn app(v: &str) -> Vec<u8> {
        zip_of(&[(&app_exe_name(), v.as_bytes()), ("resources/README.md", b"r")])
    }

    #[test]
    fn install_update_rollback_and_prune() {
        let dir = tempfile::tempdir().unwrap();
        let i = Install::new(dir.path());
        assert_eq!(i.state().unwrap(), None);
        for v in ["1.0.0", "1.1.0", "1.2.0"] {
            i.unpack(v, io::Cursor::new(app(v)), false).unwrap();
            i.activate(v).unwrap();
        }
        assert_eq!(i.state().unwrap(), Some(State { active: "1.2.0".into(), previous: Some("1.1.0".into()) }));
        assert!(!i.version_dir("1.0.0").exists(), "only active and previous stay");
        assert_eq!(std::fs::read(i.active_exe().unwrap()).unwrap(), b"1.2.0");
        let s = i.rollback().unwrap();
        assert_eq!((s.active.as_str(), s.previous.as_deref()), ("1.1.0", Some("1.2.0")));
        assert_eq!(i.rollback().unwrap().active, "1.2.0", "a second rollback goes forward");
        let (found, v) = Install::of_app_exe(&i.active_exe().unwrap()).unwrap();
        assert_eq!((found.root(), v.as_str()), (dir.path(), "1.2.0"));
    }

    #[test]
    fn a_recovery_never_keeps_the_pulled_version_as_the_previous() {
        let dir = tempfile::tempdir().unwrap();
        let i = Install::new(dir.path());
        for v in ["1.1.0", "1.2.0"] {
            i.unpack(v, io::Cursor::new(app(v)), false).unwrap();
            i.activate(v).unwrap();
        }
        // 1.2.0 pulled, recovery to the previous 1.1.0: no previous is left.
        let s = i.activate_dropping_active("1.1.0").unwrap();
        assert_eq!((s.active.as_str(), s.previous), ("1.1.0", None));
        assert!(i.version_dir("1.2.0").exists(), "the running pulled version stays until a later prune");
        // 1.2.0 and 1.1.0 both pulled, recovery to 1.0.0: 1.1.0 is not kept as the previous.
        i.activate("1.2.0").unwrap();
        i.unpack("1.0.0", io::Cursor::new(app("1.0.0")), false).unwrap();
        let s = i.activate_dropping_active("1.0.0").unwrap();
        assert_eq!((s.active.as_str(), s.previous), ("1.0.0", None));
    }

    // Sol r8 R8-2: a folder named like the marker is not a complete version, for complete()
    // and is_complete() alike (one helper).
    #[test]
    fn a_folder_marker_is_not_complete() {
        let dir = tempfile::tempdir().unwrap();
        let i = Install::new(dir.path());
        std::fs::create_dir_all(i.version_dir("1.0.0").join(COMPLETE)).unwrap();
        assert!(!i.complete("1.0.0").unwrap() && !i.is_complete("1.0.0"));
        i.unpack("1.0.0", io::Cursor::new(app("1.0.0")), true).unwrap();
        assert!(i.complete("1.0.0").unwrap() && i.is_complete("1.0.0"));
    }

    // Sol r7 sweep: a state.json that does not read (delete share only, Windows: a replace still
    // works) stops the switch. Before, the old active version was lost as the previous, and the
    // prune after the switch removed it.
    #[cfg(windows)]
    #[test]
    fn a_state_that_does_not_read_stops_the_switch() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let i = Install::new(dir.path());
        for v in ["1.0.0", "1.1.0"] { i.unpack(v, io::Cursor::new(app(v)), false).unwrap(); }
        i.activate_keep("1.0.0").unwrap();
        let held = std::fs::OpenOptions::new().read(true).share_mode(4).open(dir.path().join(STATE)).unwrap();
        assert!(i.activate("1.1.0").is_err());
        drop(held);
        assert!(i.is_complete("1.0.0"), "the active version stays");
        i.activate("1.1.0").unwrap();
        assert_eq!(i.state().unwrap().unwrap().previous.as_deref(), Some("1.0.0"));
    }

    /// The in-game hosts read the store with fm_feature_sdk::active::installed_versions (they do
    /// not link this crate): it must give the version that Install::active gives (feature session
    /// sync plan, the session versions). Complete, active not complete, no state.json, bad state.
    #[test]
    fn the_sdk_store_reader_agrees_with_active() {
        let dir = tempfile::tempdir().unwrap();
        let feature = || zip_of(&[(FEATURE_MANIFEST, b"{}")]);
        let store = |id: &str| Install::feature(dir.path().join(id));
        for v in ["1.0.0", "1.1.0"] {
            store("done").unpack(v, io::Cursor::new(feature()), false).unwrap();
            store("done").activate_keep(v).unwrap();
        }
        store("half").unpack("1.0.0", io::Cursor::new(feature()), false).unwrap();
        store("half").activate_keep("1.0.0").unwrap();
        std::fs::remove_file(store("half").version_dir("1.0.0").join(COMPLETE)).unwrap();
        store("no-state").unpack("1.0.0", io::Cursor::new(feature()), false).unwrap();
        std::fs::create_dir_all(dir.path().join("bad")).unwrap();
        std::fs::write(dir.path().join("bad").join(STATE), b"{").unwrap();
        let sdk = fm_feature_sdk::active::installed_versions(dir.path());
        for id in ["done", "half", "no-state", "bad"] {
            assert_eq!(sdk.get(id).cloned(), store(id).active().map(|(v, _)| v), "{id}");
        }
        assert_eq!(sdk.len(), 1, "{sdk:?}");
        assert_eq!(sdk["done"], "1.1.0");
    }

    #[test]
    fn an_incomplete_or_bad_package_never_becomes_active() {
        let dir = tempfile::tempdir().unwrap();
        let i = Install::new(dir.path());
        i.unpack("1.0.0", io::Cursor::new(app("1.0.0")), false).unwrap();
        i.activate("1.0.0").unwrap();
        // No app exe in the package: refused, staging removed, state unchanged.
        assert!(i.unpack("2.0.0", io::Cursor::new(zip_of(&[("x.txt", b"x")])), false).is_err());
        assert!(i.activate("2.0.0").is_err());
        // A folder without the marker (a stop during unpack) is not a version.
        std::fs::create_dir_all(i.version_dir("3.0.0")).unwrap();
        assert!(!i.is_complete("3.0.0") && i.activate("3.0.0").is_err());
        assert_eq!(i.state().unwrap().unwrap().active, "1.0.0");
        i.prune();
        assert!(!i.version_dir("3.0.0").exists());
        let names: Vec<_> = std::fs::read_dir(dir.path().join("versions")).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert!(names.iter().all(|n| !n.to_string_lossy().starts_with(STAGING)), "no staging folder stays: {names:?}");
    }

    #[test]
    fn a_repair_replaces_the_folder_and_leaves_nothing_aside() {
        let dir = tempfile::tempdir().unwrap();
        let i = Install::new(dir.path());
        i.unpack("1.0.0", io::Cursor::new(app("old")), false).unwrap();
        i.activate("1.0.0").unwrap();
        i.unpack("1.0.0", io::Cursor::new(app("new")), false).unwrap();
        assert_eq!(std::fs::read(i.active_exe().unwrap()).unwrap(), b"old", "no repair: the complete folder stays");
        i.unpack("1.0.0", io::Cursor::new(app("new")), true).unwrap();
        assert_eq!(std::fs::read(i.active_exe().unwrap()).unwrap(), b"new");
        i.prune();
        let left: Vec<_> = std::fs::read_dir(dir.path().join(VERSIONS)).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, ["1.0.0"]);
    }

    // The launcher in a package goes to the root at each switch forward; a switch back keeps the
    // newer launcher (its uninstall fixes, Sol review r6), also a recovery to an older version.
    #[test]
    fn a_forward_switch_installs_the_launcher_and_a_switch_back_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let i = Install::new(dir.path());
        let name = fm_platform::install::launcher_name();
        for v in ["1.0.0", "2.0.0"] {
            let launcher = format!("launcher {v}");
            i.unpack(v, io::Cursor::new(zip_of(&[(&app_exe_name(), b"a"), (name, launcher.as_bytes())])), false).unwrap();
            i.activate(v).unwrap();
        }
        let root_launcher = || std::fs::read_to_string(dir.path().join(name)).unwrap();
        assert_eq!(root_launcher(), "launcher 2.0.0");
        i.rollback().unwrap();
        assert_eq!(root_launcher(), "launcher 2.0.0", "back to 1.0.0: the 2.0.0 launcher stays");
        // The older app's own switch copies the launcher of its folder: now the newer one (Sol r8).
        assert!(i.copy_launcher("1.0.0").unwrap());
        assert_eq!(root_launcher(), "launcher 2.0.0");
        i.rollback().unwrap();
        assert_eq!(root_launcher(), "launcher 2.0.0");
        i.activate_dropping_active("1.0.0").unwrap();
        assert_eq!(root_launcher(), "launcher 2.0.0", "a recovery to an older version too");
        i.unpack("3.0.0", io::Cursor::new(zip_of(&[(&app_exe_name(), b"a"), (name, b"launcher 3.0.0")])), false).unwrap();
        i.activate_keep("3.0.0").unwrap();
        assert_eq!(root_launcher(), "launcher 3.0.0");
    }

    // A feature store needs the manifest (not the app exe) and never copies a launcher.
    #[test]
    fn a_feature_store_needs_the_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let f = Install::feature(dir.path());
        assert!(f.unpack("1.0.0", io::Cursor::new(app("1.0.0")), false).is_err(), "an app package is not a feature");
        let name = fm_platform::install::launcher_name();
        assert!(f.unpack("1.0.0", io::Cursor::new(zip_of(&[(FEATURE_MANIFEST, b"{}"), (name, b"x")])), false).is_err(), "only parts at the top");
        let pkg = zip_of(&[(FEATURE_MANIFEST, b"{}"), ("ui/index.html", b"x")]);
        f.unpack("1.0.0", io::Cursor::new(pkg), false).unwrap();
        f.activate("1.0.0").unwrap();
        assert_eq!(f.active().unwrap(), ("1.0.0".to_string(), f.version_dir("1.0.0")));
        assert!(!dir.path().join(name).exists(), "no launcher copy for a feature");
    }

    // Entries that the unpack would write somewhere else than their name says (review 2026-10-18):
    // a second manifest, a part hidden behind "..", a backslash path, a twice-named file, a link.
    // Each one is refused, by the consent check and by the unpack alike.
    #[test]
    fn a_feature_package_entry_lands_where_its_name_says_or_nowhere() {
        let dir = tempfile::tempdir().unwrap();
        let f = Install::feature(dir.path());
        let m = br#"{"x":1}"#;
        let bad: Vec<Vec<u8>> = vec![
            zip_of(&[(FEATURE_MANIFEST, m), ("ui/../feature.json", m)]),
            zip_of(&[(FEATURE_MANIFEST, m), ("ui/../ats-mod/x.scs", b"x")]),
            zip_of(&[(FEATURE_MANIFEST, m), ("ats-mod\\x.scs", b"x")]),
            zip_of(&[(FEATURE_MANIFEST, m), ("ui/a.html", b"x"), ("ui/A.html", b"y")]),
            {
                let mut out = io::Cursor::new(Vec::new());
                let mut z = zip::ZipWriter::new(&mut out);
                z.start_file(FEATURE_MANIFEST, zip::write::SimpleFileOptions::default()).unwrap();
                z.write_all(m).unwrap();
                z.add_symlink("ui/key", "/home/x/.ssh/id_rsa", zip::write::SimpleFileOptions::default()).unwrap();
                z.finish().unwrap();
                out.into_inner()
            },
        ];
        for (i, pkg) in bad.into_iter().enumerate() {
            assert!(feature::inspect(io::Cursor::new(pkg.clone())).is_err(), "inspect {i}");
            assert!(f.unpack("1.0.0", io::Cursor::new(pkg), false).is_err(), "unpack {i}");
        }
        assert!(!f.version_dir("1.0.0").exists());
    }

    #[test]
    fn a_dev_build_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Install::of_app_exe(&dir.path().join("target").join("debug").join(app_exe_name())).is_none());
        // A versions/<v> folder without state.json at the root is not an install either.
        let exe = dir.path().join(VERSIONS).join("1.0.0").join(app_exe_name());
        assert!(Install::of_app_exe(&exe).is_none());
    }

    #[test]
    fn versions_must_be_safe_folder_names() {
        for bad in ["", "..", "../x", "1.0.0/../../x", "1.0", "a b"] {
            assert!(check_version(bad).is_err(), "{bad}");
        }
        assert!(check_version("0.1.0-beta.3").is_ok());
        let dir = tempfile::tempdir().unwrap();
        assert!(Install::new(dir.path()).unpack("../evil", io::Cursor::new(app("x")), false).is_err());
    }

    // Sol r1 B3: unpacks of one version at the same time (two downloads of one module) each use
    // their own staging folder. With one shared name, one unpack deleted or renamed the folder
    // that the other still wrote. All must succeed, the version is complete, no staging stays.
    #[test]
    fn concurrent_unpacks_of_one_version_all_succeed() {
        for _ in 0..10 {
            let dir = tempfile::tempdir().unwrap();
            let i = Install::new(dir.path());
            let pkg = app("1.0.0");
            let results: Vec<io::Result<()>> = std::thread::scope(|s| {
                let runs: Vec<_> = (0..8).map(|_| s.spawn(|| i.unpack("1.0.0", io::Cursor::new(pkg.clone()), false))).collect();
                runs.into_iter().map(|r| r.join().unwrap()).collect()
            });
            assert!(results.iter().all(|r| r.is_ok()), "{results:?}");
            assert!(i.is_complete("1.0.0"));
            let names: Vec<_> = std::fs::read_dir(dir.path().join(VERSIONS)).unwrap().map(|e| e.unwrap().file_name()).collect();
            assert_eq!(names, ["1.0.0"], "no staging folder stays");
        }
    }
}

/// Exit codes of `fm_app --fm-uninstall` (the game-file cleanup), read by the uninstaller.
pub mod cleanup {
    /// All game files removed.
    pub const ALL_REMOVED: i32 = 0;
    /// A file could not be removed (a running game holds it): a retry can help.
    pub const RETRY: i32 = 1;
    /// The install record is unreadable, so nothing was removed.
    pub const NO_RECORD: i32 = 2;
    /// Only files changed outside Feature Machine stay: a retry cannot help.
    pub const CHANGED_KEPT: i32 = 3;
    /// The argument that runs the cleanup.
    pub const FLAG: &str = "--fm-uninstall";
    /// After FLAG: the uninstall also deletes the app data, so the cleanup also ends the account
    /// session (revoke, credential deleted). An older app ignores it and only cleans up.
    pub const DELETE_DATA: &str = "--delete-data";
    /// The argument that makes a running app quit (setup, uninstall).
    pub const QUIT: &str = "--fm-quit";
}
