// User files service (plan 2026-10-24-user-files-service.md): the game files that a feature reads
// or changes (capability user_files), named in feature.json so that consent and review see each
// one. A call names a file as "<game>:<path>" under the game's user folder. One rule set for the
// manifest check (fm_install), the in-game host (host.rs) and the lifecycle helper (hook.rs), so
// a path that one of them takes, the others take too. File I/O is host-side only (feature "host").
use serde::{Deserialize, Serialize};

/// The most entries in a feature's user_files list.
pub const MAX_USER_FILES: usize = 16;
/// The most bytes of one file_read or file_write call (config and log reads are small).
pub const MAX_BYTES: usize = 1024 * 1024;
/// The longest path, and the longest part of a path.
const MAX_PATH: usize = 256;
const MAX_PART: usize = 128;

/// What a feature may do with a declared file. Write includes read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Read,
    Write,
}

/// One declared file: a path pattern under the user folder of `game` ("profiles/*/config.cfg").
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserFile {
    pub game: String,
    pub path: String,
    pub access: Access,
}

/// What a call needs from the list: Info also takes a folder on the way to a declared file
/// (so a feature can find "profiles/<hex>"); Read and Write take only a declared file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    Info,
    Read,
    Write,
}

/// The games that have a user folder in Feature Machine (default_user_root). MSFS: none yet.
pub fn has_user_folder(game: &str) -> bool {
    game == "ats"
}

/// One part of a path or pattern: a normal name with no "\", ":", "*" or control character, and
/// no "." or " " at its end (Windows drops them, so "a." would name the file "a").
fn name_ok(p: &str) -> bool {
    (1..=MAX_PART).contains(&p.len()) && p != "." && p != ".." && !p.ends_with(['.', ' ']) && !p.chars().any(|c| c.is_control() || matches!(c, '\\' | ':' | '*' | '<' | '>' | '"' | '|' | '?'))
}

/// A path that a feature names: "/" between parts, each part name_ok.
pub fn valid_path(path: &str) -> bool {
    path.len() <= MAX_PATH && path.split('/').all(name_ok)
}

/// A declared pattern: like valid_path, but a part can be "*" (exactly one name).
pub fn valid_pattern(pattern: &str) -> bool {
    pattern.len() <= MAX_PATH && pattern.split('/').all(|p| p == "*" || name_ok(p))
}

/// True when `path` is `pattern` (all parts), or with `prefix` a folder on the way to it.
fn fits(pattern: &str, path: &str, prefix: bool) -> bool {
    let (pat, parts): (Vec<&str>, Vec<&str>) = (pattern.split('/').collect(), path.split('/').collect());
    let count = if prefix { parts.len() < pat.len() } else { parts.len() == pat.len() };
    count && parts.iter().zip(&pat).all(|(p, q)| *q == "*" || p == q)
}

/// The game and path of a key "<game>:<path>".
pub fn split_key(key: &str) -> Option<(&str, &str)> {
    key.split_once(':').filter(|(g, p)| !g.is_empty() && valid_path(p))
}

/// The list's rule for one call. `scope`: the only game that a key may name (in a game), or
/// None (a lifecycle hook). Err: the ABI code (BAD_ARGS for a bad key, REFUSED for a file that is
/// not declared or a write to a read entry) and the reason for the log.
pub fn check<'a>(list: &[UserFile], scope: Option<&str>, key: &'a str, need: Need) -> Result<(&'a str, &'a str), (i32, String)> {
    let (game, path) = split_key(key).ok_or_else(|| (crate::abi::BAD_ARGS, format!("{key:?} is not \"<game>:<path>\" with a plain relative path")))?;
    if scope.is_some_and(|s| s != game) {
        return Err((crate::abi::REFUSED, format!("{key:?}: this host serves {} files only", scope.unwrap_or(""))));
    }
    let ok = list.iter().filter(|f| f.game == game).any(|f| match need {
        Need::Info => fits(&f.path, path, false) || fits(&f.path, path, true),
        Need::Read => fits(&f.path, path, false),
        Need::Write => f.access == Access::Write && fits(&f.path, path, false),
    });
    ok.then_some((game, path)).ok_or_else(|| (crate::abi::REFUSED, format!("{key:?} is not declared in user_files{}", if need == Need::Write { " with access write" } else { "" })))
}

/// The manifest rule (fm_install): 1 to MAX_USER_FILES entries, each for a game of the feature
/// that has a user folder, with a valid pattern, no entry twice.
pub fn check_list(list: &[UserFile], games: &[String]) -> Result<(), String> {
    if list.is_empty() || list.len() > MAX_USER_FILES {
        return Err(format!("user_files needs 1 to {MAX_USER_FILES} entries"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for f in list {
        if !games.contains(&f.game) || !has_user_folder(&f.game) {
            return Err(format!("user_files: {:?} is not a game of this feature with a user folder", f.game));
        }
        if !valid_pattern(&f.path) {
            return Err(format!("user_files: {:?} is not a plain relative path (\"/\" between parts, \"*\" only as a whole part)", f.path));
        }
        if !seen.insert((&f.game, &f.path)) {
            return Err(format!("user_files lists {}:{} twice", f.game, f.path));
        }
    }
    Ok(())
}

/// The real path of an existing user folder (std::fs::canonicalize) with no Windows "\\?\"
/// prefix, so it stays a normal path in the report, sessions.json and the helper arguments.
/// The plugin, the app record and the helper use it; Err: the folder does not exist.
pub fn canonical_root(root: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let p = std::fs::canonicalize(root)?;
    let plain = match p.to_str() {
        Some(t) if t.starts_with(r"\\?\UNC\") => format!(r"\\{}", &t[8..]).into(),
        Some(t) if t.starts_with(r"\\?\") => t[4..].into(),
        _ => p,
    };
    Ok(plain)
}

#[cfg(feature = "host")]
pub use io::*;

/// The file work of the hosts: one implementation for the in-game host and the hook helper.
#[cfg(feature = "host")]
mod io {
    use super::*;
    use crate::abi;
    use std::path::{Path, PathBuf};
    use std::time::UNIX_EPOCH;

    /// The default user folder of a game: ATS keeps its profiles and game.log.txt in
    /// Documents\American Truck Simulator (Linux: the data folder). A "-homedir" start uses
    /// another folder: the ATS plugin finds it (Backend::user_root) and reports it, and the app
    /// gives it to the exit hook (fm_feature_hook --user-root).
    pub fn default_user_root(game: &str) -> Option<PathBuf> {
        #[cfg(test)]
        if let Some(docs) = TEST_DOCUMENTS.with(|d| d.borrow().clone()) {
            return (game == "ats").then(|| docs.join("American Truck Simulator"));
        }
        match game {
            "ats" => if cfg!(windows) { dirs::document_dir() } else { dirs::data_dir() }.map(|d| d.join("American Truck Simulator")),
            _ => None,
        }
    }

    // Unit tests only: a Documents folder for this thread. The real default folder can be missing
    // on a test computer and tests must not write in Documents, so a test that needs an existing
    // default folder (a junction to it, review r2) sets a temp folder here. One thread only, so
    // tests that run at the same time keep the real default.
    #[cfg(test)]
    thread_local!(pub(crate) static TEST_DOCUMENTS: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) });

    /// The folder under a feature's data folder that holds one data folder per other user folder.
    pub const USER_FOLDERS: &str = ".user-folders";

    /// The data folder of a feature with user files when the game uses `root`: the base folder
    /// for the default user folder, else base/.user-folders/<id of game and root>. So state about
    /// one user folder's files (a journal of profile values) never meets another folder's files:
    /// a hook with the default folder cannot repair a -homedir profile. The in-game host and the
    /// hook helper both use this, so each gets the same folder for the same root.
    pub fn data_dir_for(base: &Path, game: &str, root: &Path) -> PathBuf {
        if default_user_root(game).is_some_and(|d| same_user_root(&d, root)) {
            return base.to_path_buf();
        }
        base.join(USER_FOLDERS).join(root_id(game, root))
    }

    /// A stable id of a game and a user folder: FNV-1a 64 of "<game>\n<identity>", 16 hex
    /// digits. Fixed math (not std's hasher, which can change), so the id stays over updates.
    pub fn root_id(game: &str, root: &Path) -> String {
        format!("{:016x}", fnv1a(&format!("{game}\n{}", identity(root))))
    }

    /// The one text of a folder for compare and hash (review r1 B1): its canonical path, which
    /// has the names as stored on disk ("." and "..", junctions, subst resolved). No case fold
    /// (review r2): Windows keeps "K" and the Kelvin sign apart, so must we. Windows: "\"
    /// separators, no end separator, an upper case drive letter. A folder that is gone keeps its
    /// text in that form (no I/O goes there: check_user_root).
    pub(crate) fn identity(root: &Path) -> String {
        let p = super::canonical_root(root).unwrap_or_else(|_| root.to_path_buf());
        let text = p.to_string_lossy();
        if !cfg!(windows) {
            return text.trim_end_matches('/').to_string();
        }
        let mut t = text.replace('/', "\\").trim_end_matches('\\').to_string();
        if t.as_bytes().get(1) == Some(&b':') {
            if let Some(drive) = t.get_mut(0..1) {
                drive.make_ascii_uppercase();
            }
        }
        t
    }

    /// True when two paths name one user folder (identity): the host's default folder check and
    /// data_dir_for use it, so an alias of the default folder is the default folder.
    pub fn same_user_root(a: &Path, b: &Path) -> bool {
        identity(a) == identity(b)
    }

    pub(crate) fn fnv1a(text: &str) -> u64 {
        text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
    }

    /// The check of a user folder that is not the default (a -homedir folder from a session
    /// record): an absolute path to a folder that has "profiles" or "steam_profiles" in it. The
    /// helper refuses another one with this reason, never a guess (plan, -homedir section).
    pub fn check_user_root(root: &Path) -> Result<(), String> {
        if !root.is_absolute() {
            return Err(format!("the user folder {} is not an absolute path", root.display()));
        }
        if !root.is_dir() {
            return Err(format!("the user folder {} does not exist", root.display()));
        }
        if !["profiles", "steam_profiles"].iter().any(|p| root.join(p).is_dir()) {
            return Err(format!("the user folder {} has no profiles or steam_profiles folder", root.display()));
        }
        Ok(())
    }

    /// The checked list, the scope and the user folder of one feature: what host.rs and hook.rs
    /// call. Each Err is the ABI code and the reason for the log.
    pub struct Service<'a> {
        pub list: &'a [UserFile],
        pub scope: Option<&'a str>,
        pub root: &'a dyn Fn(&str) -> Result<PathBuf, String>,
    }

    type Fail = (i32, String);

    impl Service<'_> {
        fn full(&self, key: &str, need: Need) -> Result<PathBuf, Fail> {
            let (game, path) = check(self.list, self.scope, key, need)?;
            let root = (self.root)(game).map_err(|e| (abi::FAILED, format!("{key:?}: {e}")))?;
            Ok(path.split('/').fold(root, |p, part| p.join(part)))
        }

        /// Kind, size and modification time; a missing file is FILE_NONE, not an error. A path on
        /// the way to a declared file tells only "folder" or FILE_NONE: a file there is not declared,
        /// so its size and time stay hidden (review r2 B2).
        pub fn info(&self, key: &str) -> Result<abi::AbiFileInfo, Fail> {
            let p = self.full(key, Need::Info)?;
            let declared = check(self.list, self.scope, key, Need::Read).is_ok();
            match std::fs::metadata(&p) {
                Ok(m) if !declared && !m.is_dir() => Ok(abi::AbiFileInfo { kind: abi::FILE_NONE, size: 0, modified_us: 0 }),
                Ok(m) => {
                    let modified_us = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_micros() as u64);
                    let kind = if m.is_dir() { abi::FILE_FOLDER } else { abi::FILE_FILE };
                    Ok(abi::AbiFileInfo { kind, size: if m.is_dir() { 0 } else { m.len() }, modified_us })
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(abi::AbiFileInfo { kind: abi::FILE_NONE, size: 0, modified_us: 0 }),
                Err(e) => Err(io_fail(key, &e)),
            }
        }

        /// Bytes from `offset` into `buf` (at most MAX_BYTES); 0 at the end of the file.
        pub fn read(&self, key: &str, offset: u64, buf: &mut [u8]) -> Result<usize, Fail> {
            use std::io::{Read, Seek, SeekFrom};
            let p = self.full(key, Need::Read)?;
            let cap = buf.len().min(MAX_BYTES);
            let mut f = std::fs::File::open(&p).map_err(|e| io_fail(key, &e))?;
            f.seek(SeekFrom::Start(offset)).map_err(|e| io_fail(key, &e))?;
            let mut n = 0;
            while n < cap {
                match f.read(&mut buf[n..cap]) {
                    Ok(0) => break,
                    Ok(k) => n += k,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(io_fail(key, &e)),
                }
            }
            Ok(n)
        }

        /// Replaces the file atomically (temp file, flush to disk, rename): a crash leaves the old
        /// file or the new one. The folder must exist; no folder is made.
        pub fn write(&self, key: &str, bytes: &[u8]) -> Result<(), Fail> {
            if bytes.len() > MAX_BYTES {
                return Err((abi::BAD_ARGS, format!("{key:?}: more than {MAX_BYTES} bytes")));
            }
            let p = self.full(key, Need::Write)?;
            write_atomic(&p, bytes).map_err(|e| io_fail(key, &e))
        }
    }

    fn io_fail(key: &str, e: &std::io::Error) -> Fail {
        (abi::FAILED, format!("{key:?}: {e}"))
    }

    /// The temp file is new and only this call's (create_new, a name with the process and a
    /// counter), in the target folder so that the rename stays on one volume. A file of another
    /// name is never truncated or replaced, and two writers never share one temp file (review r1 B2).
    fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = path.file_name().ok_or_else(|| std::io::Error::other("no file name"))?.to_string_lossy();
        let (mut f, tmp) = loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let tmp = path.with_file_name(format!(".{name}.fm-{}-{n}.tmp", std::process::id()));
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp) {
                Ok(f) => break (f, tmp),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        };
        let r = f.write_all(bytes).and_then(|()| f.sync_all());
        drop(f);
        r.and_then(|()| std::fs::rename(&tmp, path)).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi;

    fn list() -> Vec<UserFile> {
        vec![
            UserFile { game: "ats".into(), path: "game.log.txt".into(), access: Access::Read },
            UserFile { game: "ats".into(), path: "profiles/*/config.cfg".into(), access: Access::Write },
        ]
    }

    /// Paths and patterns: plain relative parts only; "*" is a whole part of a pattern only.
    #[test]
    fn paths_and_patterns() {
        for good in ["game.log.txt", "profiles/52756E/config.cfg", "a b/c"] {
            assert!(valid_path(good) && valid_pattern(good), "{good:?}");
        }
        for bad in ["", "/a", "a/", "a//b", "../a", "a/../b", "./a", "C:/a", "a\\b", "a*", "*", "a.", "a ", "a\nb", "con?"] {
            assert!(!valid_path(bad), "{bad:?}");
        }
        assert!(valid_pattern("profiles/*/config.cfg") && valid_pattern("*"));
        assert!(!valid_pattern("profiles/a*/config.cfg") && !valid_pattern("../*"));
        assert!(!valid_path(&"a/".repeat(200)));
    }

    /// A key needs its declaration: a file for read and write (write only with access write), and
    /// a folder on the way for info. Another game's key is refused in a game; a bad key is BAD_ARGS.
    #[test]
    fn each_call_needs_its_declaration() {
        let l = list();
        let ok = |key: &str, need| check(&l, Some("ats"), key, need).is_ok();
        assert!(ok("ats:game.log.txt", Need::Read) && ok("ats:game.log.txt", Need::Info));
        assert!(!ok("ats:game.log.txt", Need::Write), "a read entry");
        assert!(ok("ats:profiles/4142/config.cfg", Need::Write) && ok("ats:profiles/4142/config.cfg", Need::Read));
        assert!(ok("ats:profiles/4142", Need::Info) && ok("ats:profiles", Need::Info), "folders on the way");
        assert!(!ok("ats:profiles/4142", Need::Read), "a folder is not a file");
        assert!(!ok("ats:profiles/4142/profile.sii", Need::Read) && !ok("ats:profiles/a/b/config.cfg", Need::Read));
        assert!(!ok("ats:steam_profiles/4142/config.cfg", Need::Info));
        assert_eq!(check(&l, Some("msfs2024"), "ats:game.log.txt", Need::Read).unwrap_err().0, abi::REFUSED);
        assert!(check(&l, None, "ats:game.log.txt", Need::Read).is_ok(), "a hook names any game");
        for bad in ["game.log.txt", ":game.log.txt", "ats:../x", "ats:profiles/*/config.cfg"] {
            assert_eq!(check(&l, None, bad, Need::Read).unwrap_err().0, abi::BAD_ARGS, "{bad:?}");
        }
    }

    #[test]
    fn the_manifest_list_rule() {
        let games = vec!["ats".to_string()];
        assert!(check_list(&list(), &games).is_ok());
        assert!(check_list(&[], &games).is_err());
        assert!(check_list(&list(), &["msfs2024".to_string()]).is_err(), "not a game of the feature");
        let msfs = UserFile { game: "msfs2024".into(), path: "x".into(), access: Access::Read };
        assert!(check_list(&[msfs], &["msfs2024".to_string()]).is_err(), "no MSFS user folder yet");
        let mut twice = list();
        twice.push(twice[0].clone());
        assert!(check_list(&twice, &games).is_err());
        let bad = UserFile { path: "../x".into(), ..list()[0].clone() };
        assert!(check_list(&[bad], &games).is_err());
        let many: Vec<UserFile> = (0..=MAX_USER_FILES).map(|i| UserFile { path: format!("f{i}"), ..list()[0].clone() }).collect();
        assert!(check_list(&many, &games).is_err());
        let json = serde_json::to_string(&list()[1]).unwrap();
        assert_eq!(json, r#"{"game":"ats","path":"profiles/*/config.cfg","access":"write"}"#);
        assert!(serde_json::from_str::<UserFile>(r#"{"game":"ats","path":"x","access":"read","extra":1}"#).is_err());
    }

    /// -homedir: the default folder keeps the base data folder (no change for a default game);
    /// another folder gets its own data folder with a stable id. A record's folder must be
    /// absolute, exist and have profiles or steam_profiles.
    #[cfg(feature = "host")]
    #[test]
    fn another_user_folder_gets_its_own_data_folder() {
        let base = std::path::Path::new("/fm/features/weather/data");
        let default = default_user_root("ats").expect("a default ATS folder on this computer");
        assert_eq!(data_dir_for(base, "ats", &default), base, "the default folder: the base data folder");
        let home = std::path::Path::new("D:/ATSData/American Truck Simulator");
        let split = data_dir_for(base, "ats", home);
        assert_eq!(split, base.join(USER_FOLDERS).join(root_id("ats", home)));
        assert_eq!(root_id("ats", home), root_id("ats", home), "stable");
        assert_ne!(root_id("ats", home), root_id("ats", std::path::Path::new("D:/Other/American Truck Simulator")));
        assert_ne!(root_id("ats", home), root_id("ets2", home));
        assert_eq!(root_id("ats", std::path::Path::new("")).len(), 16);
        // FNV-1a 64 of "a": the published test vector, so the math is the fixed one.
        assert_eq!(fnv1a("a"), 0xaf63_dc4c_8601_ec8c);
        let t = tempfile::tempdir().unwrap();
        assert!(check_user_root(std::path::Path::new("relative/ATS")).unwrap_err().contains("not an absolute path"));
        assert!(check_user_root(&t.path().join("gone")).unwrap_err().contains("does not exist"));
        assert!(check_user_root(t.path()).unwrap_err().contains("no profiles"));
        std::fs::create_dir(t.path().join("steam_profiles")).unwrap();
        assert_eq!(check_user_root(t.path()), Ok(()));
    }

    /// Review r1 B1: one Windows folder in another form (case, "/" separators, an end separator,
    /// "." and "..") is one identity, so one data folder; canonical_root gives a normal path (no
    /// "\\?\" prefix). The default folder in another case is still the default.
    #[cfg(feature = "host")]
    #[test]
    fn one_folder_in_other_forms_is_one_data_folder() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("ATS Data").join("American Truck Simulator");
        std::fs::create_dir_all(home.join("profiles")).unwrap();
        let canon = canonical_root(&home).unwrap();
        assert!(!canon.to_string_lossy().starts_with(r"\\?\") && canon.is_absolute(), "{canon:?}");
        let base = std::path::Path::new("/fm/data");
        let want = data_dir_for(base, "ats", &home);
        let text = home.to_string_lossy().into_owned();
        let mut forms = vec![home.join("profiles").join(".."), home.join(".").join("profiles").join("..")];
        if cfg!(windows) {
            forms.extend([text.to_uppercase(), text.to_lowercase().replace('\\', "/") + "/", text.clone() + "\\"].map(std::path::PathBuf::from));
        }
        for f in &forms {
            assert_eq!(data_dir_for(base, "ats", f), want, "{f:?}");
        }
        assert_eq!(canonical_root(&forms[0]).unwrap(), canon);
        // The default folder can be missing on a test computer: separators, an end separator and
        // a lower case drive letter are the forms that need no disk (review r2).
        let default = default_user_root("ats").unwrap();
        let mut other = default.to_string_lossy().into_owned();
        if cfg!(windows) {
            other = other.replace('\\', "/") + "/";
            other.get_mut(0..1).unwrap().make_ascii_lowercase();
        } else {
            other += "/";
        }
        assert_eq!(data_dir_for(base, "ats", std::path::Path::new(&other)), base, "the default folder in another form: {other}");
    }

    /// Review r2: two real folders whose names only a case fold makes equal ("K" and the
    /// Kelvin sign U+212A; Windows keeps them apart) are two data folders, never one journal.
    #[cfg(all(feature = "host", windows))]
    #[test]
    fn folders_that_windows_keeps_apart_are_two_data_folders() {
        let t = tempfile::tempdir().unwrap();
        let (k, kelvin) = (t.path().join("K"), t.path().join("\u{212A}"));
        std::fs::create_dir(&k).unwrap();
        std::fs::create_dir(&kelvin).expect("NTFS keeps K and the Kelvin sign apart");
        assert_ne!(canonical_root(&k).unwrap(), canonical_root(&kelvin).unwrap());
        let base = std::path::Path::new("/fm/data");
        assert_ne!(data_dir_for(base, "ats", &k), data_dir_for(base, "ats", &kelvin));
        assert!(!same_user_root(&k, &kelvin));
    }

    /// Review r2: a junction to a folder is that folder (same_user_root, the host's default
    /// check), and so is one data folder.
    #[cfg(all(feature = "host", windows))]
    #[test]
    fn a_junction_is_the_folder_that_it_names() {
        let t = tempfile::tempdir().unwrap();
        let (real, link) = (t.path().join("real"), t.path().join("link"));
        std::fs::create_dir(&real).unwrap();
        let made = std::process::Command::new("cmd").args(["/d", "/c", "mklink", "/J"]).arg(&link).arg(&real).output().unwrap();
        assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
        assert!(same_user_root(&real, &link));
        let base = std::path::Path::new("/fm/data");
        assert_eq!(data_dir_for(base, "ats", &link), data_dir_for(base, "ats", &real));
    }

    /// The I/O of the service: info of a missing file, a read in parts, an atomic write that
    /// leaves no temp file, and the refusals before any I/O.
    #[cfg(feature = "host")]
    #[test]
    fn the_service_reads_and_writes_only_declared_files() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().to_path_buf();
        let root_of = move |g: &str| if g == "ats" { Ok(root.clone()) } else { Err("no folder".to_string()) };
        let l = list();
        let s = Service { list: &l, scope: Some("ats"), root: &root_of };
        assert_eq!(s.info("ats:game.log.txt").unwrap().kind, abi::FILE_NONE);
        std::fs::write(t.path().join("game.log.txt"), b"0123456789").unwrap();
        let i = s.info("ats:game.log.txt").unwrap();
        assert_eq!((i.kind, i.size), (abi::FILE_FILE, 10));
        assert!(i.modified_us > 0);
        let mut buf = [0u8; 4];
        assert_eq!(s.read("ats:game.log.txt", 8, &mut buf).unwrap(), 2);
        assert_eq!(&buf[..2], b"89");
        assert_eq!(s.read("ats:game.log.txt", 10, &mut buf).unwrap(), 0, "the end");
        std::fs::create_dir_all(t.path().join("profiles/41")).unwrap();
        assert_eq!(s.info("ats:profiles/41").unwrap().kind, abi::FILE_FOLDER);
        // Review r2 B2: a file where only a folder on the way is declared shows nothing.
        std::fs::write(t.path().join("profiles/notes.txt"), b"private").unwrap();
        let i = s.info("ats:profiles/notes.txt").unwrap();
        assert_eq!((i.kind, i.size, i.modified_us), (abi::FILE_NONE, 0, 0));
        s.write("ats:profiles/41/config.cfg", b"uset a \"1\"\n").unwrap();
        assert_eq!(std::fs::read(t.path().join("profiles/41/config.cfg")).unwrap(), b"uset a \"1\"\n");
        let names = || std::fs::read_dir(t.path().join("profiles/41")).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect::<std::collections::BTreeSet<_>>();
        // Review r1 B2: a file with the old temp name stays as it is, and no temp file stays.
        std::fs::write(t.path().join("profiles/41/config.cfg.fm-tmp"), b"not ours").unwrap();
        s.write("ats:profiles/41/config.cfg", b"uset a \"1\"\n").unwrap();
        assert_eq!(std::fs::read(t.path().join("profiles/41/config.cfg.fm-tmp")).unwrap(), b"not ours");
        assert_eq!(names(), ["config.cfg", "config.cfg.fm-tmp"].map(String::from).into());
        // Two writers at the same time: the file has one writer's whole bytes, never a mix. (A
        // rename can fail while Windows replaces the same file for the other writer: that write
        // fails as a whole and removes its temp file.)
        let (a, b) = (vec![b'a'; 300_000], vec![b'b'; 300_000]);
        std::thread::scope(|sc| {
            for bytes in [&a, &b] {
                let (l, root_of) = (&l, &root_of);
                sc.spawn(move || {
                    let s = Service { list: l, scope: Some("ats"), root: root_of };
                    for _ in 0..20 {
                        let _ = s.write("ats:profiles/41/config.cfg", bytes);
                    }
                });
            }
        });
        let end = std::fs::read(t.path().join("profiles/41/config.cfg")).unwrap();
        assert!(end == a || end == b, "one writer's whole bytes");
        assert_eq!(names().len(), 2, "no temp file stays");
        assert_eq!(s.write("ats:profiles/42/config.cfg", b"x").unwrap_err().0, abi::FAILED, "no folder is made");
        assert_eq!(s.write("ats:game.log.txt", b"x").unwrap_err().0, abi::REFUSED);
        assert_eq!(s.write("ats:profiles/41/config.cfg", &vec![0; MAX_BYTES + 1]).unwrap_err().0, abi::BAD_ARGS);
        assert_eq!(std::fs::read(t.path().join("game.log.txt")).unwrap(), b"0123456789");
        let hook = Service { list: &l, scope: None, root: &root_of };
        let no_root = UserFile { game: "x".into(), path: "a".into(), access: Access::Read };
        let l2 = [no_root];
        let other = Service { list: &l2, ..hook };
        assert_eq!(other.info("x:a").unwrap_err().0, abi::FAILED, "a game with no user folder");
    }
}
