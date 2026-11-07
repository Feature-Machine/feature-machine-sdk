// The running features follow the app (session sync plan, decision 1; contract 1 "follow"): the
// list of active.json while the app is linked, or lost less than LINK_GRACE ago, and none with no
// app. Moved here from the ATS plugin, so both hosts use one rule. It takes the path and the
// clock from the host and gives back its diagnostics (no host log). A host calls `sync` on each
// loop pass; a check runs each LIST_CHECK, which also retries the items that wait (Admit::Wait).
use crate::active::ActiveList;
use crate::host::{FeatureSet, State};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// An app link lost less than this ago still counts: the features keep running through a short
/// stall or an app restart (the app connects again in 1 to 3 s), so live weather does not send
/// its weather again for nothing. After it, all features stop (plan decision 1).
pub const LINK_GRACE: Duration = Duration::from_secs(10);
/// How often the host checks active.json for a change (and retries the items that wait).
pub const LIST_CHECK: Duration = Duration::from_secs(1);
/// Status detail of the features that stopped because the app is gone.
pub const APP_GONE: &str = "Feature Machine is closed; it starts again when Feature Machine connects";

/// A file's modification time and size: the app writes active.json with a rename, so a new list
/// changes at least one of them, and a read never sees half a file.
type Stamp = Option<(Option<SystemTime>, u64)>;

/// The feature list as the host knows it, and the app link.
pub struct List {
    file: Option<PathBuf>,
    list: ActiveList,
    error: Option<String>,
    stamp: Stamp,
    next_check: Instant,
    /// The last pass that saw a connected app.
    linked_at: Option<Instant>,
    /// Lines for the host log, taken with `diagnostics`.
    diag: Vec<String>,
    /// The game and the program of this process: each read keeps only the features of its
    /// install (ActiveList::for_exe). None: the whole list (a host that cannot read its exe).
    scope: Option<Scope>,
}

enum Scope { Executable(String, PathBuf), Install(String, String) }

/// What `List::check` decided for this pass.
#[derive(Debug, PartialEq)]
pub enum Check {
    /// No check in this pass.
    Later,
    /// The running features follow the list; `read`: the file was read (false: the last good
    /// list, and the reason in `List::error`).
    Follow { read: bool },
    /// No app link for LINK_GRACE (or never): no feature runs.
    NoApp,
}

impl List {
    /// `file`: <features dir>/active.json; None when the host has no per-user data folder.
    pub fn new(file: Option<PathBuf>, now: Instant) -> Self {
        List { file, list: ActiveList::default(), error: None, stamp: None, next_check: now, linked_at: None, diag: vec![], scope: None }
    }

    /// The list of one install (per-install setup): `game` and `exe`, the program of this process.
    pub fn scoped(mut self, game: &str, exe: Option<&Path>) -> Self {
        self.scope = exe.map(|e| Scope::Executable(game.to_string(), e.to_path_buf()));
        self
    }

    /// A separate helper follows only the explicitly granted install on every list read.
    pub fn scoped_install(mut self, application: &str, install: &str) -> Self {
        self.scope = Some(Scope::Install(application.into(), install.into()));
        self
    }

    /// Why the list cannot be read now (for HostReport.error); None: the last read was good.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The lines that the host writes to its log since the last call.
    pub fn diagnostics(&mut self) -> Vec<String> {
        std::mem::take(&mut self.diag)
    }

    /// The last good list.
    pub fn list(&self) -> &ActiveList {
        &self.list
    }

    /// Reads the list when its stamp changed, or always (`force`). False: it cannot be read or
    /// parsed (the reason is in `error`); the last good list stays.
    fn read(&mut self, force: bool) -> bool {
        let Some(file) = &self.file else {
            self.error = Some("no per-user data folder".into());
            return false;
        };
        let stamp = std::fs::metadata(file).ok().map(|m| (m.modified().ok(), m.len()));
        if force || stamp != self.stamp {
            self.stamp = stamp;
            let loaded = ActiveList::load(file).and_then(|l| match &self.scope {
                Some(Scope::Executable(game, exe)) => Ok(l.for_exe(game, exe)),
                Some(Scope::Install(application, install)) => l.for_install(application, install),
                None => Ok(l),
            });
            match loaded {
                Ok(l) => {
                    (self.list, self.error) = (l, None)
                }
                Err(e) => {
                    let outcome = if matches!(self.scope, Some(Scope::Install(..))) {
                        self.list = ActiveList::default();
                        "the install grant is closed"
                    } else { "the running features stay" };
                    self.diag.push(format!("ERROR: feature list: {e}; {outcome}"));
                    self.error = Some(e);
                }
            }
        }
        self.error.is_none()
    }

    /// Each pass: what the running features must follow now. With the app linked (or lost less
    /// than LINK_GRACE ago) the list; with no app, nothing. A check runs each LIST_CHECK, or now
    /// for a StopFeature (`force`), which also reads the file again.
    pub fn check(&mut self, now: Instant, connected: bool, force: bool) -> Check {
        if connected {
            self.linked_at = Some(now);
        }
        if !force && now < self.next_check {
            return Check::Later;
        }
        self.next_check = now + LIST_CHECK;
        let read = self.read(force);
        match self.linked_at.is_some_and(|t| now.duration_since(t) < LINK_GRACE) {
            true => Check::Follow { read },
            false => Check::NoApp,
        }
    }

    /// Does what `check` says. `stops`: the features that a StopFeature named in this pass.
    /// Returns the ids that started.
    pub fn sync(&mut self, now: Instant, connected: bool, set: &mut FeatureSet, stops: &[String]) -> Vec<String> {
        match self.check(now, connected, !stops.is_empty()) {
            Check::Later => vec![],
            Check::Follow { read } => {
                // The list cannot say what is on: a StopFeature still stops its feature. It leaves
                // the last good list until the next good read, so sync does not start it again
                // (review R1-N1).
                if !read {
                    self.list.features.retain(|f| !stops.contains(&f.id));
                }
                set.sync(&self.list)
            }
            Check::NoApp => {
                let running: Vec<String> = set.statuses().into_iter().filter(|f| f.state == State::Running).map(|f| f.id).collect();
                for id in running {
                    set.stop(&id, State::Stopped, Some(APP_GONE.into()));
                }
                vec![]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{Backend, Input};
    use std::sync::Arc;

    struct Quiet;
    impl Backend for Quiet {
        fn ui(&self, _: &str, _: &str, _: &str) {}
        fn input(&self, _: &str, _: &str, _: Input) -> Result<(), String> { Ok(()) }
        fn log(&self, _: &str) {}
        fn stopped(&self, _: &str) {}
    }

    fn list_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fm-follow-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("active.json")
    }

    fn ids(l: &List) -> Vec<String> {
        l.list.features.iter().map(|f| f.id.clone()).collect()
    }

    fn write_list(file: &std::path::Path, ids: &[&str]) {
        let features: Vec<_> = ids.iter().map(|id| serde_json::json!({ "id": id, "version": "1.0.0", "library": "x", "games": ["ats"], "capabilities": [] })).collect();
        std::fs::write(file, serde_json::json!({ "features": features }).to_string()).unwrap();
    }

    /// Feature session sync plan, decision 1 and root cause 1: no feature runs before the first
    /// app link or LINK_GRACE after the last one, the list is read again when it changes, and a
    /// StopFeature reads it at once.
    #[test]
    fn the_running_features_follow_the_link_and_the_list() {
        let file = list_file("link");
        write_list(&file, &["a"]);
        let t0 = Instant::now();
        let mut l = List::new(Some(file.clone()), t0);
        assert_eq!(l.check(t0, false, false), Check::NoApp, "game start with no app");
        assert_eq!(l.check(t0 + LIST_CHECK / 2, true, false), Check::Later, "one check per LIST_CHECK");
        assert_eq!(l.check(t0 + LIST_CHECK, true, false), Check::Follow { read: true });
        assert_eq!(ids(&l), ["a"]);
        // The member turns b on: the next check reads the new file.
        write_list(&file, &["a", "b"]);
        assert_eq!(l.check(t0 + LIST_CHECK * 2, true, false), Check::Follow { read: true });
        assert_eq!(ids(&l), ["a", "b"]);
        // The app is gone after the last check: the features stay through LINK_GRACE, then none.
        assert_eq!(l.check(t0 + LIST_CHECK * 2 + LINK_GRACE - Duration::from_millis(1), false, false), Check::Follow { read: true });
        assert_eq!(l.check(t0 + LIST_CHECK * 3 + LINK_GRACE, false, false), Check::NoApp);
        // It comes back: the list again.
        assert_eq!(l.check(t0 + LIST_CHECK * 5 + LINK_GRACE, true, false), Check::Follow { read: true });
        // A StopFeature checks now and reads the file, also when its stamp looks the same.
        let stamp = std::fs::metadata(&file).unwrap().modified().unwrap();
        write_list(&file, &["a", "c"]);
        std::fs::File::options().write(true).open(&file).unwrap().set_modified(stamp).unwrap();
        let t = t0 + LIST_CHECK * 6 + LINK_GRACE;
        assert_eq!(l.check(t, true, false), Check::Follow { read: true });
        assert_eq!(ids(&l), ["a", "b"], "same size and time: no read");
        assert_eq!(l.check(t, true, true), Check::Follow { read: true });
        assert_eq!(ids(&l), ["a", "c"]);
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    /// Per-install setup: a scoped list keeps the features of the install of its exe at each read,
    /// so a feature turned on in another install never starts in this game.
    #[test]
    fn a_scoped_list_follows_its_own_install() {
        let file = list_file("scope");
        let write = |steam: &[&str]| {
            let features: Vec<_> = ["a", "b"].iter().map(|id| serde_json::json!({ "id": id, "version": "1.0.0", "library": "x", "games": ["ats"], "capabilities": [] })).collect();
            let installs = serde_json::json!([{ "id": "s", "game": "ats", "dir": "/lib1/ATS", "features": steam }, { "id": "t", "game": "ats", "dir": "/lib2/ATS", "features": ["a", "b"] }]);
            std::fs::write(&file, serde_json::json!({ "features": features, "installs": installs }).to_string()).unwrap();
        };
        write(&["a"]);
        let t0 = Instant::now();
        let mut l = List::new(Some(file.clone()), t0).scoped("ats", Some(Path::new("/lib1/ATS/bin/win_x64/amtrucks.exe")));
        assert_eq!(l.check(t0, true, true), Check::Follow { read: true });
        assert_eq!(ids(&l), ["a"]);
        write(&["a", "b"]);
        assert_eq!(l.check(t0, true, true), Check::Follow { read: true });
        assert_eq!(ids(&l), ["a", "b"]);
        let mut whole = List::new(Some(file.clone()), t0);
        whole.check(t0, true, true);
        assert_eq!(ids(&whole), ["a", "b"], "no scope: the whole list");
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    // An external grant must remain present on each read. Damage or a removed install closes
    // its selection so a helper cannot keep authority from a previously valid file.
    #[test]
    fn an_external_list_loses_its_selection_when_the_grant_is_removed() {
        let file = list_file("external");
        let features = serde_json::json!([{"id":"a","version":"1.0.0","library":"x","games":["ats"],"capabilities":[]}]);
        let save = |installs: serde_json::Value| std::fs::write(&file, serde_json::json!({"features":features,"installs":installs}).to_string()).unwrap();
        save(serde_json::json!([{"id":"s","game":"ats","dir":"/vendor","features":["a"]}]));
        let now = Instant::now();
        let mut list = List::new(Some(file.clone()), now).scoped_install("ats", "s");
        assert_eq!(list.check(now, true, true), Check::Follow { read: true });
        assert_eq!(ids(&list), ["a"]);
        save(serde_json::json!([]));
        assert_eq!(list.check(now, true, true), Check::Follow { read: false });
        assert!(ids(&list).is_empty());
        std::fs::remove_dir_all(file.parent().unwrap()).unwrap();
    }

    /// A damaged file keeps the last good list, says why and gives one log line; a missing file
    /// is an empty list.
    #[test]
    fn a_bad_list_keeps_the_running_features() {
        let file = list_file("bad");
        write_list(&file, &["a"]);
        let t0 = Instant::now();
        let mut l = List::new(Some(file.clone()), t0);
        assert_eq!(l.check(t0, true, false), Check::Follow { read: true });
        std::fs::write(&file, b"{").unwrap();
        assert_eq!(l.check(t0 + LIST_CHECK, true, false), Check::Follow { read: false });
        assert_eq!((ids(&l), l.error().is_some()), (vec!["a".to_string()], true));
        assert_eq!(l.diagnostics().len(), 1);
        assert_eq!(l.check(t0 + LIST_CHECK * 2, true, false), Check::Follow { read: false });
        assert!(l.diagnostics().is_empty(), "the same bad file is logged once");
        // A StopFeature while the list is bad: the feature leaves the last good list.
        let mut set = FeatureSet::new("ats", Arc::new(Quiet), false);
        assert!(l.sync(t0 + LIST_CHECK * 3, true, &mut set, &["a".into()]).is_empty());
        assert_eq!(ids(&l), Vec::<String>::new());
        std::fs::remove_file(&file).unwrap();
        assert_eq!(l.check(t0 + LIST_CHECK * 4, true, false), Check::Follow { read: true });
        assert_eq!((ids(&l), l.error()), (vec![], None), "no file: no feature installed");
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    /// No per-user data folder: an error, no feature, no panic.
    #[test]
    fn no_data_folder_is_an_error() {
        let t0 = Instant::now();
        let mut l = List::new(None, t0);
        assert_eq!(l.check(t0, true, false), Check::Follow { read: false });
        assert_eq!(l.error(), Some("no per-user data folder"));
    }
}
