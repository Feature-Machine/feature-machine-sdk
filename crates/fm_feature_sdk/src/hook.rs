// Lifecycle hooks of feature libraries (abi.rs SYM_LIFECYCLE). Event and the exit codes are the
// contract between the app, which starts the helper fm_feature_hook, and the helper, which loads
// the library and calls the hook ("host" feature). The library code runs only in that helper, so
// a crash or a hang of a feature ends the helper (the app kills it after a time limit), never the
// app or a game. API 1 and 2 libraries have no hook and are skipped.
use crate::abi;
#[cfg(feature = "host")]
use std::path::Path;

/// One lifecycle point, as the app gives it to the helper (args) and the helper to the feature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Installed,
    Updated { from_version: String },
    Uninstalling,
    GameExited { game: String },
}

impl Event {
    /// The helper arguments of this event: "--event <name>" and its value, if any.
    pub fn args(&self) -> Vec<String> {
        let mut a = vec!["--event".to_string()];
        match self {
            Event::Installed => a.push("installed".into()),
            Event::Updated { from_version } => a.extend(["updated".into(), "--from-version".into(), from_version.clone()]),
            Event::Uninstalling => a.push("uninstalling".into()),
            Event::GameExited { game } => a.extend(["game-exited".into(), "--game".into(), game.clone()]),
        }
        a
    }

    /// The event of a name and its value (from_version or game), as `args` writes them.
    pub fn parse(name: &str, from_version: Option<&str>, game: Option<&str>) -> Result<Event, String> {
        match (name, from_version, game) {
            ("installed", None, None) => Ok(Event::Installed),
            ("updated", Some(v), None) => Ok(Event::Updated { from_version: v.into() }),
            ("uninstalling", None, None) => Ok(Event::Uninstalling),
            ("game-exited", None, Some(g)) => Ok(Event::GameExited { game: g.into() }),
            _ => Err(format!("bad event {name:?} (updated needs --from-version, game-exited needs --game, others neither)")),
        }
    }

    /// The ABI code of the event (only the helper calls hooks, so host builds only).
    #[cfg(feature = "host")]
    fn code(&self) -> u32 {
        match self {
            Event::Installed => abi::LIFECYCLE_INSTALLED,
            Event::Updated { .. } => abi::LIFECYCLE_UPDATED,
            Event::Uninstalling => abi::LIFECYCLE_UNINSTALLING,
            Event::GameExited { .. } => abi::LIFECYCLE_GAME_EXITED,
        }
    }
}

/// Helper exit codes. Another code (a crash) or the time limit is a failed hook for the app.
pub const EXIT_DONE: i32 = 0;
/// The hook returned FAILED (its reason is in the log).
pub const EXIT_FAILED: i32 = 1;
pub const EXIT_BAD_ARGS: i32 = 2;
/// The library did not load or is not a feature library of a known API.
pub const EXIT_LOAD_FAILED: i32 = 3;
/// API 1 or 2, or no export: nothing to do.
pub const EXIT_NO_HOOK: i32 = 4;

/// What the helper does with a library: call its hook, skip it, or refuse it (load failure).
pub enum Pick {
    Run(abi::LifecycleFn),
    Skip(String),
    Refuse(String),
}

/// The rule for a library of API `version` with or without the export.
pub fn pick(version: u32, export: Option<abi::LifecycleFn>) -> Pick {
    match (version, export) {
        (v, _) if !(abi::MIN_API_VERSION..=abi::API_VERSION).contains(&v) => Pick::Refuse(format!("built for feature API {v}, this helper runs {} to {}", abi::MIN_API_VERSION, abi::API_VERSION)),
        (v, _) if v < 3 => Pick::Skip(format!("feature API {v} has no lifecycle hook")),
        (_, None) => Pick::Skip("no fm_feature_lifecycle export".into()),
        (_, Some(f)) => Pick::Run(f),
    }
}

/// What the helper gives a hook besides its folder and log: the declared game files of the
/// version that runs the hook (the app passes them as --user-file), the user folder of a game,
/// and the game-running check (the helper asks fm_platform).
#[cfg(feature = "host")]
pub struct Services<'a> {
    pub user_files: &'a [crate::UserFile],
    pub root: &'a dyn Fn(&str) -> Result<std::path::PathBuf, String>,
    pub game_running: &'a dyn Fn(&str) -> Result<bool, String>,
}

/// The helper argument of one declared file: "<access>:<game>:<path>" ("write:ats:profiles/*/config.cfg").
pub fn user_file_arg(f: &crate::UserFile) -> String {
    let access = match f.access { crate::Access::Read => "read", crate::Access::Write => "write" };
    format!("{access}:{}:{}", f.game, f.path)
}

/// The declared file of a helper argument (user_file_arg), checked with the manifest rules.
pub fn parse_user_file(arg: &str) -> Result<crate::UserFile, String> {
    let bad = || format!("bad --user-file {arg:?} (\"read|write:<game>:<path>\")");
    let (access, rest) = arg.split_once(':').ok_or_else(bad)?;
    let (game, path) = rest.split_once(':').ok_or_else(bad)?;
    let access = match access { "read" => crate::Access::Read, "write" => crate::Access::Write, _ => return Err(bad()) };
    let f = crate::UserFile { game: game.into(), path: path.into(), access };
    crate::files::check_list(std::slice::from_ref(&f), std::slice::from_ref(&f.game)).map_err(|e| format!("{}: {e}", bad()))?;
    Ok(f)
}

/// Loads `library` and runs its hook for `event`. Ok(code) is an EXIT_* code; Err is a load
/// failure (EXIT_LOAD_FAILED). Helper process only (header). The library stays loaded until the
/// process ends, as in host.rs (a Rust library can keep thread-local destructors).
#[cfg(feature = "host")]
pub fn run_library(library: &Path, event: &Event, data_dir: &Path, services: &Services, log: &mut dyn FnMut(&str)) -> Result<i32, String> {
    let lib = unsafe { libloading::Library::new(library) }.map_err(|e| format!("cannot load {}: {e}", library.display()))?;
    let version: abi::ApiVersionFn = *unsafe { lib.get::<abi::ApiVersionFn>(abi::SYM_API_VERSION) }.map_err(|_| "not a feature library (no fm_feature_api_version)".to_string())?;
    let export = unsafe { lib.get::<abi::LifecycleFn>(abi::SYM_LIFECYCLE) }.ok().map(|s| *s);
    let picked = pick(unsafe { version() }, export);
    std::mem::forget(lib);
    match picked {
        Pick::Refuse(e) => Err(e),
        Pick::Skip(why) => {
            log(&why);
            Ok(EXIT_NO_HOOK)
        }
        Pick::Run(f) => call(f, event, data_dir, services, log),
    }
}

/// The ctx of one LifecycleCall: the log sink and the services (call).
#[cfg(feature = "host")]
struct Ctx<'a, 'b> {
    sink: std::cell::RefCell<&'a mut dyn FnMut(&str)>,
    services: &'a Services<'b>,
}

/// Calls one hook function. Err: the call could not be made (data folder not UTF-8).
#[cfg(feature = "host")]
pub fn call(f: abi::LifecycleFn, event: &Event, data_dir: &Path, services: &Services, log: &mut dyn FnMut(&str)) -> Result<i32, String> {
    let dir = data_dir.to_str().ok_or_else(|| format!("the data folder {} is not UTF-8", data_dir.display()))?;
    let (from, game) = match event {
        Event::Updated { from_version } => (from_version.as_str(), ""),
        Event::GameExited { game } => ("", game.as_str()),
        _ => ("", ""),
    };
    let ctx = Ctx { sink: std::cell::RefCell::new(log), services };
    let c = abi::LifecycleCall {
        api_version: abi::API_VERSION,
        event: event.code(),
        ctx: &ctx as *const Ctx as *mut std::ffi::c_void,
        from_version: from.as_ptr(),
        from_version_len: from.len(),
        game: game.as_ptr(),
        game_len: game.len(),
        data_dir: dir.as_ptr(),
        data_dir_len: dir.len(),
        log: log_cb,
        file_info: file_info_cb,
        file_read: file_read_cb,
        file_write: file_write_cb,
        game_running: game_running_cb,
    };
    // The feature! glue catches its own panics; a foreign library that unwinds aborts the helper,
    // which the app sees as a crash.
    Ok(if unsafe { f(&c) } == abi::OK { EXIT_DONE } else { EXIT_FAILED })
}

#[cfg(feature = "host")]
unsafe fn hook_ctx<'a>(ctx: *mut std::ffi::c_void) -> Option<&'a Ctx<'a, 'a>> {
    (ctx as *const Ctx).as_ref()
}

/// A line to the sink of `call`, cut at 1000 characters. A line from inside another sink call
/// (not possible through the ABI, which has one thread) is dropped, never a double borrow.
#[cfg(feature = "host")]
fn line(c: &Ctx, t: &str) {
    let end = t.char_indices().nth(1000).map_or(t.len(), |(i, _)| i);
    if let Ok(mut sink) = c.sink.try_borrow_mut() {
        sink(&t[..end]);
    }
}

/// The log of LifecycleCall.
#[cfg(feature = "host")]
unsafe extern "C" fn log_cb(ctx: *mut std::ffi::c_void, text: *const u8, len: usize) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let (Some(c), Some(t)) = (hook_ctx(ctx), abi::text(text, len)) {
            line(c, t);
        }
    }));
}

/// One user files call of a hook: files::Service with no game scope (the key names the game).
#[cfg(feature = "host")]
fn user_files<T>(c: &Ctx, key: *const u8, len: usize, f: impl FnOnce(&crate::files::Service, &str) -> Result<T, (i32, String)>) -> Result<T, i32> {
    let key = unsafe { abi::text(key, len) }.ok_or(abi::BAD_ARGS)?;
    let service = crate::files::Service { list: c.services.user_files, scope: None, root: c.services.root };
    f(&service, key).map_err(|(code, why)| {
        line(c, &format!("user file: {why}"));
        code
    })
}

#[cfg(feature = "host")]
unsafe extern "C" fn file_info_cb(ctx: *mut std::ffi::c_void, key: *const u8, len: usize, out: *mut abi::AbiFileInfo) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (Some(c), Some(out)) = (hook_ctx(ctx), out.as_mut()) else { return abi::BAD_ARGS };
        user_files(c, key, len, |s, k| s.info(k)).map_or_else(|code| code, |i| {
            *out = i;
            abi::OK
        })
    }))
    .unwrap_or(abi::FAILED)
}

#[cfg(feature = "host")]
unsafe extern "C" fn file_read_cb(ctx: *mut std::ffi::c_void, key: *const u8, len: usize, offset: u64, buf: *mut u8, cap: usize) -> i64 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(c) = hook_ctx(ctx) else { return abi::BAD_ARGS as i64 };
        if buf.is_null() && cap > 0 {
            return abi::BAD_ARGS as i64;
        }
        let buf: &mut [u8] = if cap == 0 { &mut [] } else { std::slice::from_raw_parts_mut(buf, cap) };
        user_files(c, key, len, |s, k| s.read(k, offset, buf)).map_or_else(|code| code as i64, |n| n as i64)
    }))
    .unwrap_or(abi::FAILED as i64)
}

#[cfg(feature = "host")]
unsafe extern "C" fn file_write_cb(ctx: *mut std::ffi::c_void, key: *const u8, len: usize, bytes: *const u8, bytes_len: usize) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(c) = hook_ctx(ctx) else { return abi::BAD_ARGS };
        if bytes.is_null() && bytes_len > 0 {
            return abi::BAD_ARGS;
        }
        let bytes: &[u8] = if bytes_len == 0 { &[] } else { std::slice::from_raw_parts(bytes, bytes_len) };
        user_files(c, key, len, |s, k| s.write(k, bytes)).map_or_else(|code| code, |()| abi::OK)
    }))
    .unwrap_or(abi::FAILED)
}

/// 1 or 0; FAILED (logged) when the process list cannot be read.
#[cfg(feature = "host")]
unsafe extern "C" fn game_running_cb(ctx: *mut std::ffi::c_void, game: *const u8, len: usize) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (Some(c), Some(game)) = (hook_ctx(ctx), abi::text(game, len)) else { return abi::BAD_ARGS };
        match (c.services.game_running)(game) {
            Ok(r) => r as i32,
            Err(e) => {
                line(c, &format!("game running {game:?}: {e}"));
                abi::FAILED
            }
        }
    }))
    .unwrap_or(abi::FAILED)
}

#[cfg(all(test, feature = "host"))]
mod tests {
    use super::*;
    use crate::host::tests::fm_feature_lifecycle;

    fn no_root(_: &str) -> Result<std::path::PathBuf, String> {
        Err("no user folder in this test".into())
    }

    fn not_running(_: &str) -> Result<bool, String> {
        Ok(false)
    }

    fn run(event: Event) -> (i32, Vec<String>) {
        let mut lines = vec![];
        let s = Services { user_files: &[], root: &no_root, game_running: &not_running };
        let code = call(fm_feature_lifecycle, &event, Path::new("/fm/data/weather"), &s, &mut |l| lines.push(l.to_string())).unwrap();
        (code, lines)
    }

    /// A hook reads and writes only the files of its --user-file list, of any game of the list,
    /// and asks if a game runs; each refusal is logged with its reason.
    #[test]
    fn a_hook_gets_its_declared_files_and_the_game_check() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("profiles/41")).unwrap();
        std::fs::write(t.path().join("profiles/41/config.cfg"), "uset a \"1\"\n").unwrap();
        std::fs::write(t.path().join("profiles/notes.txt"), "private").unwrap();
        let list = [parse_user_file("write:ats:profiles/*/config.cfg").unwrap()];
        let root = t.path().to_path_buf();
        let root_of = move |g: &str| if g == "ats" { Ok(root.clone()) } else { Err("none".to_string()) };
        let running = |g: &str| if g == "ats" { Ok(true) } else { Err("no process list".to_string()) };
        let s = Services { user_files: &list, root: &root_of, game_running: &running };
        let mut lines = vec![];
        let code = call(fm_feature_lifecycle, &Event::GameExited { game: "files".into() }, Path::new("/d"), &s, &mut |l| lines.push(l.to_string())).unwrap();
        assert_eq!(code, EXIT_DONE);
        assert!(lines.contains(&"running ats: Ok(true), msfs2024: Err(Failed)".to_string()), "{lines:?}");
        assert!(lines.contains(&r#"read: Ok("uset a \"1\"\n")"#.to_string()), "{lines:?}");
        assert!(lines.contains(&"write: Ok(()), other: Err(Refused)".to_string()), "{lines:?}");
        assert!(lines.contains(&"on the way: Ok((None, 0, None))".to_string()), "review r2 B2: {lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("user file: \"ats:game.log.txt\" is not declared")), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("game running \"msfs2024\": no process list")));
        assert_eq!(std::fs::read_to_string(t.path().join("profiles/41/config.cfg")).unwrap(), "uset a \"0.3\"\n");
    }

    /// The app passes each declared file as one argument; the helper takes only valid entries.
    #[test]
    fn declared_files_go_through_the_helper_arguments() {
        let f = crate::UserFile { game: "ats".into(), path: "profiles/*/config.cfg".into(), access: crate::Access::Write };
        assert_eq!(user_file_arg(&f), "write:ats:profiles/*/config.cfg");
        assert_eq!(parse_user_file(&user_file_arg(&f)), Ok(f));
        for bad in ["ats:game.log.txt", "delete:ats:x", "read:ats:../x", "read:msfs2024:x", "read::x"] {
            assert!(parse_user_file(bad).is_err(), "{bad:?}");
        }
    }

    /// Each event reaches the feature with its value and the data folder; a failed or panicked
    /// hook is EXIT_FAILED with its reason in the log.
    #[test]
    fn the_hook_gets_each_event_and_its_folder() {
        let dir = Path::new("/fm/data/weather").display().to_string();
        assert_eq!(run(Event::Installed), (EXIT_DONE, vec![format!("Installed in {dir}")]));
        assert_eq!(run(Event::Uninstalling), (EXIT_DONE, vec![format!("Uninstalling in {dir}")]));
        assert_eq!(run(Event::GameExited { game: "ats".into() }), (EXIT_DONE, vec![format!("GameExited {{ game: \"ats\" }} in {dir}")]));
        assert_eq!(run(Event::Updated { from_version: "0.1.0".into() }).0, EXIT_DONE);
        assert_eq!(run(Event::Updated { from_version: "fail".into() }).1.last().map(String::as_str), Some("cannot repair"));
        assert_eq!(run(Event::Updated { from_version: "fail".into() }).0, EXIT_FAILED);
        assert_eq!(run(Event::Updated { from_version: "boom".into() }), (EXIT_FAILED, vec![format!("Updated {{ from_version: \"boom\" }} in {dir}"), "panic in lifecycle".into()]));
    }

    /// API 1 and 2 libraries and a library with no export are skipped; an unknown API is refused.
    #[test]
    fn old_libraries_are_skipped() {
        let f: abi::LifecycleFn = fm_feature_lifecycle;
        assert!(matches!(pick(1, None), Pick::Skip(_)));
        assert!(matches!(pick(2, Some(f)), Pick::Skip(_)), "an API 2 library never gets the call");
        assert!(matches!(pick(3, None), Pick::Skip(_)));
        assert!(matches!(pick(3, Some(f)), Pick::Run(_)));
        assert!(matches!(pick(abi::API_VERSION + 1, Some(f)), Pick::Refuse(_)));
        assert!(matches!(pick(0, None), Pick::Refuse(_)));
    }

    #[test]
    fn events_go_through_the_helper_arguments() {
        for e in [Event::Installed, Event::Updated { from_version: "0.1.0".into() }, Event::Uninstalling, Event::GameExited { game: "ats".into() }] {
            let a = e.args();
            let value = |flag: &str| a.iter().position(|x| x == flag).map(|i| a[i + 1].as_str());
            assert_eq!(Event::parse(&a[1], value("--from-version"), value("--game")), Ok(e));
        }
        assert!(Event::parse("updated", None, None).is_err() && Event::parse("installed", Some("1"), None).is_err() && Event::parse("x", None, None).is_err());
    }

    /// A file that is not a library, and a library that is not a feature, fail to load.
    #[test]
    fn a_bad_library_is_a_load_failure() {
        let mut log = |_: &str| {};
        let s = Services { user_files: &[], root: &no_root, game_running: &not_running };
        let missing = std::env::temp_dir().join("fm-no-such-feature.dll");
        assert!(run_library(&missing, &Event::Installed, Path::new("/x"), &s, &mut log).unwrap_err().contains("cannot load"));
        #[cfg(windows)]
        assert!(run_library(Path::new("kernel32.dll"), &Event::Installed, Path::new("/x"), &s, &mut log).unwrap_err().contains("not a feature library"));
    }
}
