// The shared loader for in-process, external and test hosts. It loads each
// library of the active list for this application (API 1 to 6, abi.rs change rule), gives each feature
// its own HostApi that checks the declared capabilities, and calls the features on one thread.
// HTTP requests wait here for an answer or the timeout; native value subscriptions are counted
// here, so a host watches each native value name once. No unload. Lifecycle hooks: hook.rs.
use crate::abi::{self, AbiEvent, AbiFrame, AbiValue, AbiVar, FeatureApi, HostApi};
pub use crate::active::{FeatureStatus, State};
use crate::active::{ActiveFeature, ActiveList, Apply, PolicyStop};
use crate::{valid_control_id, Capability, Control, HostError, HttpAnswer, Multiplayer, RenderParams, ShaderStage, Value, Var};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, MutexGuard};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    Press { ms: u32 },
    Axis(f32),
}

/// What the host does for features. Called on the feature thread; must return fast.
pub trait Backend: Send + Sync + 'static {
    /// Runs before LoadLibrary. Hosts that implement maps must validate the full session first.
    /// A successful prepare is always followed by `cleanup` (after the stop, or at once when the
    /// library does not load), so a host may reserve state here (the MSFS camera reader owner).
    fn prepare(&self, feature: &ActiveFeature) -> Result<(), String> {
        if feature.capabilities.iter().any(|c| matches!(c, Capability::VarsMemory | Capability::VarsWrite)) {
            Err("mapped memory is unavailable in this host".into())
        } else if feature.capabilities.contains(&Capability::RenderPass) {
            Err("the color pass is unavailable in this host".into())
        } else { Ok(()) }
    }
    /// Admission of one start try (session sync plan 12.2): `activation` is new for each try.
    /// Wait keeps the item stopped with the reason and the host tries again (`waits`); Refuse
    /// makes it Failed. A host must give back what it took before it returns Wait or Refuse.
    /// The default maps `prepare`, so a host from before slice 7 keeps its rules.
    fn admit(&self, feature: &ActiveFeature, _activation: u64) -> Admit {
        match self.prepare(feature) { Ok(()) => Admit::Ok, Err(e) => Admit::Refuse(e) }
    }
    /// Sync: the item is clean when `cleanup` returns. Pending: `cleanup` only starts the work
    /// (a game-thread restore), and the item waits until `FeatureSet::cleaned` (no new start).
    fn completion(&self) -> Completion { Completion::Sync }
    fn ui(&self, feature: &str, topic: &str, json: &str);
    /// A page message with the activation that sent it. Never Busy (plan 12.2: the host keeps
    /// the latest state per topic); an error only for a rule such as too many topics. The
    /// default gives it to `ui`, so a host from before slice 7 keeps its path.
    fn ui_send(&self, feature: &str, _activation: u64, topic: &str, json: &str) -> Result<(), HostError> {
        self.ui(feature, topic, json);
        Ok(())
    }
    fn input(&self, feature: &str, name: &str, input: Input) -> Result<(), String>;
    fn log(&self, line: &str);
    /// The feature is about to stop: drop its waiting engine commands. This runs BEFORE the
    /// feature's own stop, so the commands that stop sends (a release of a held value) run.
    fn stopping(&self, _feature: &str) {}
    /// The feature stopped (turned off, blocked, failed, session end): release the inputs it
    /// holds.
    fn stopped(&self, feature: &str);
    /// A checked engine command (capability and first word): queue it for the game thread.
    fn game_command(&self, _feature: &str, _text: &str) -> Result<(), String> {
        Err("this application has no engine commands".into())
    }
    /// A checked HTTPS GET (capability, https, declared host) with its request id: send it to
    /// the app, which answers later through FeatureSet::http_answer. Err: no request was sent,
    /// and the feature gets that error at once: Busy for "try again later" (no app link for a
    /// moment, a full queue), so a feature does not count it as a failed source. The backend logs
    /// the reason.
    fn http_get(&self, feature: &str, _id: u64, _url: &str) -> Result<(), HostError> {
        self.log(&format!("[{feature}] http_get: this host has no network yet"));
        Err(HostError::Failed)
    }
    /// Start (on) or stop reading the native value `native` (the key without "<game>:"). The
    /// set calls it once per name: on for the first reader, off after the last. The game decides
    /// later if the name exists; the host then calls FeatureSet::var_result. Err: not watched.
    fn watch(&self, _native: &str, _on: bool) -> Result<(), String> {
        Err("this host reads no native values".into())
    }
    /// A checked sim event (capability, name): send it once with its data, never reversed.
    fn send_event(&self, _feature: &str, _name: &str, _data: i32) -> Result<(), String> {
        Err("this application has no simulation events; use an input or an engine command".into())
    }
    /// Mapped subscriptions use their complete key. Defaults refuse an unimplemented adapter.
    fn memory_watch(&self, _key: &str, _on: bool) -> Result<(), String> { Err("mapped memory is unavailable in this host".into()) }
    fn memory_acquire(&self, _feature: &str, _key: &str) -> Result<u64, HostError> { Err(HostError::Refused) }
    fn memory_set(&self, _feature: &str, _handle: u64, _value: Var) -> Result<(), HostError> { Err(HostError::Refused) }
    fn memory_release(&self, _feature: &str, _handle: u64) -> Result<(), HostError> { Err(HostError::Refused) }
    /// The color pass: checked parameters, one owner. Cleanup must also remove a held pass.
    fn render_acquire(&self, _feature: &str, _params: RenderParams) -> Result<u64, HostError> { Err(HostError::Refused) }
    fn render_set(&self, _feature: &str, _handle: u64, _params: RenderParams) -> Result<(), HostError> { Err(HostError::Refused) }
    fn render_release(&self, _feature: &str, _handle: u64) -> Result<(), HostError> { Err(HostError::Refused) }
    /// A feature shader pass: checked stage, name and parameters, and the compiled shader from
    /// the feature's package. Any number per feature; render_release and cleanup remove them.
    fn shader_acquire(&self, _feature: &str, _stage: ShaderStage, _code: &ShaderCode, _params: [f32; abi::SHADER_PARAMS]) -> Result<u64, HostError> { Err(HostError::Refused) }
    /// `code`: a shader swap (Some) or the same shader (None).
    fn shader_set(&self, _feature: &str, _handle: u64, _code: Option<&ShaderCode>, _params: [f32; abi::SHADER_PARAMS]) -> Result<(), HostError> { Err(HostError::Refused) }
    /// Restore all host-owned mutations. Err is reported as a failed stop, never clean success.
    /// `activation`: the start that ends, for a Completion::Pending host (FeatureSet::cleaned).
    /// Only this hook needs it: no second start of a feature exists while its cleanup is pending,
    /// so the feature id names the activation in `stopping` and `stopped` (plan 12.2, contract 1).
    fn cleanup(&self, _feature: &str, _activation: u64) -> Result<(), String> { Ok(()) }
    /// The user folder of `game` for the user files service (files.rs): the default folder. A
    /// host gives another folder only when it proved that the game uses it (ATS -homedir); the
    /// report carries it to the app for the exit hook. Err (logged): no user files. Keep the answer.
    fn user_root(&self, game: &str) -> Result<std::path::PathBuf, String> {
        crate::files::default_user_root(game).ok_or_else(|| format!("{game} has no user folder in Feature Machine"))
    }
    /// When the game process started (HostApi::game_started_us). None: not known.
    fn application_started(&self) -> Option<std::time::SystemTime> { self.game_started() }
    /// Compatibility provider for existing backends.
    fn game_started(&self) -> Option<std::time::SystemTime> { None }
}

/// The result of `Backend::admit` for one start try.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admit {
    Ok,
    /// Not now (no app link, no sim, a cleanup or resources that wait): tried again later.
    Wait(String),
    /// Never in this game session: the item is Failed.
    Refuse(String),
}

/// When a stop's cleanup is complete (`Backend::completion`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completion { Sync, Pending }

/// A compiled shader of a feature package (shaders/<name>.cso), read by the SDK for every host.
/// The host checks the bytes (fm_render::Program) and builds the GPU object from them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderCode { pub name: String, pub bytes: Arc<[u8]> }

/// Status detail of a feature with a capability that this build does not know.
pub const UNKNOWN_CAPABILITY: &str = "needs a capability that this version does not know; update Feature Machine";
/// Requests of one feature that can wait for an answer at the same time (the app fetches at
/// most this many per feature at once). One more gets BUSY.
pub const MAX_WAITING: usize = 4;
/// A request with no answer after this gets TIMEOUT from the host (the app gives up at 30 s).
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(60);
/// The longest URL that http_get takes.
pub const MAX_URL: usize = 2048;
/// Native values that one feature can read at the same time, and all features together. They
/// keep the game thread fast (each name is one game registration); one more gets BUSY.
pub const MAX_VARS_PER_FEATURE: usize = 256;
pub const MAX_VARS: usize = 1024;
/// The longest native name (the key without "<game>:").
pub const MAX_VAR_NAME: usize = 256;
pub use crate::abi::PAGE_LIMIT;

/// The native values that each feature reads (feature id to native names), the names that the
/// game has confirmed, and the later readers of a confirmed name that wait for their "var.ready"
/// (the backend watches a name once, so no new answer comes for them; review 2026-10-19).
/// Shared by the set and the subscribe callbacks, all on the feature thread.
#[derive(Default)]
struct Subs {
    readers: BTreeMap<String, BTreeSet<String>>,
    ready: BTreeSet<String>,
    /// (queue number, feature, native) in queue order; `late_seq` is the next queue number.
    late: Vec<(u64, String, String)>,
    late_seq: u64,
}

impl Subs {
    fn add_late(&mut self, feature: &str, native: &str) {
        self.late.push((self.late_seq, feature.to_string(), native.to_string()));
        self.late_seq += 1;
    }

    /// The oldest waiting reader queued before `upto`, removed from the queue.
    fn next_late(&mut self, upto: u64) -> Option<(String, String)> {
        let (seq, ..) = self.late.first()?;
        (*seq < upto).then(|| self.late.remove(0)).map(|(_, f, n)| (f, n))
    }

    fn used(&self, native: &str) -> bool {
        self.readers.values().any(|s| s.contains(native))
    }

    /// Removes every name of `feature`; returns the names that no other feature reads.
    fn drop_feature(&mut self, feature: &str) -> Vec<String> {
        let names = self.readers.remove(feature).unwrap_or_default();
        self.late.retain(|(_, f, _)| f != feature);
        let gone: Vec<String> = names.into_iter().filter(|n| !self.used(n)).collect();
        gone.iter().for_each(|n| self.forget(n));
        gone
    }

    /// No feature reads `native` any more: the next first reader waits for a new answer.
    fn forget(&mut self, native: &str) {
        self.ready.remove(native);
        self.late.retain(|(_, _, n)| n != native);
    }
}

/// The native name in a subscribe key "<game>:<native>" of this game: printable text with no
/// control character, at most MAX_VAR_NAME bytes. MSFS names have spaces, commas and colons.
fn native_of<'a>(key: &'a str, game: &str) -> Option<&'a str> {
    if memory_key(key) { return Some(key); }
    let n = key.strip_prefix(game)?.strip_prefix(':')?;
    (!n.is_empty() && n.len() <= MAX_VAR_NAME && !n.chars().any(char::is_control) && n.trim() == n).then_some(n)
}

fn memory_key(key: &str) -> bool {
    let id = |s: &str| (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !s.starts_with('-') && !s.ends_with('-');
    key.strip_prefix("mem:").and_then(|s| s.split_once('/')).is_some_and(|(map, symbol)| id(map) && id(symbol))
}

fn var_key(game: &str, native: &str) -> String { if memory_key(native) { native.to_string() } else { format!("{game}:{native}") } }

fn watch(backend: &dyn Backend, native: &str, on: bool) -> Result<(), String> {
    if memory_key(native) { backend.memory_watch(native, on) } else { backend.watch(native, on) }
}

struct Waiting {
    feature: String,
    id: u64,
    since: Instant,
}

/// The HTTP requests of a feature set that wait for an answer. Shared by the set (answers,
/// timeouts, stops) and the http_get callback of each feature, all on the feature thread.
#[derive(Default)]
struct Net {
    last_id: u64,
    waiting: Vec<Waiting>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct Ctx {
    id: String,
    /// The start that this context serves (page messages carry it).
    activation: u64,
    caps: Vec<Capability>,
    commands: Vec<String>,
    hosts: Vec<String>,
    game: String,
    /// HostApi::data_dir points here (empty: none, or a path that is not UTF-8).
    data_dir: String,
    backend: Arc<dyn Backend>,
    net: Arc<Mutex<Net>>,
    subs: Arc<Mutex<Subs>>,
    stopping: AtomicBool,
    memory_maps: BTreeSet<String>,
    /// The package's shaders/ folder: <version>/shaders beside bin/<os>/<library> (the package
    /// layout of fm_install). None: the library path has no version folder (a test feature).
    shaders: Option<std::path::PathBuf>,
    /// The held shader passes and the time of the last file check (hot reload, reload_shaders).
    held: Mutex<(Vec<Held>, Option<Instant>)>,
    /// The declared game files (capability user_files).
    user_files: Vec<crate::UserFile>,
    /// HostApi::settings points here: the projection at this start (API 6; "{}" otherwise).
    settings: String,
    settings_revision: u64,
}

/// A file's modification time and size: a new compile changes at least one of them.
type Stamp = (Option<std::time::SystemTime>, u64);

/// A shader pass of a feature: its file stamp at load and its last parameters, so a reload
/// sends the same parameters with the new shader.
struct Held { handle: u64, name: String, stamp: Stamp, params: [f32; abi::SHADER_PARAMS] }

/// Time between two checks of the held shader files.
const RELOAD_EVERY: Duration = Duration::from_secs(1);

/// A loaded feature library: its start function and the API version it was built for.
#[derive(Clone, Copy)]
pub struct Entry {
    pub start: abi::StartFn,
    pub api: u32,
}

/// The checked feature table: every function is there (start refuses a table with a null).
struct Running {
    instance: *mut c_void,
    on_frame: unsafe extern "C" fn(*mut c_void, *const AbiFrame) -> i32,
    on_event: unsafe extern "C" fn(*mut c_void, *const u8, usize) -> i32,
    stop: unsafe extern "C" fn(*mut c_void),
    /// Version 3 tables only; None for an older feature (it cannot call http_get).
    on_http: Option<unsafe extern "C" fn(*mut c_void, u64, i32, *const u8, usize) -> i32>,
    /// Version 4 tables only: events with attributes, in place of on_event. A version 4 feature
    /// also gets native values in its frames.
    on_game_event: Option<unsafe extern "C" fn(*mut c_void, *const AbiEvent) -> i32>,
    /// Boxed: the feature keeps pointers to both until stop.
    _host: Box<HostApi>,
    _ctx: Box<Ctx>,
}

/// One feature of the session. `meta` and `entry` are the record of its session version: a clean
/// stop keeps them, so `sync` can start the same version again. `entry` None: the library never
/// loaded (failed, blocked) or the item only waits for the next game start (UPDATE_WAITS).
struct Item {
    meta: ActiveFeature,
    entry: Option<Entry>,
    run: Option<Running>,
    status: FeatureStatus,
    /// The activation of the last start try (0: none yet).
    activation: u64,
    /// The last try got Admit::Wait: the host tries again (FeatureSet::waits).
    waiting: bool,
    /// A Completion::Pending cleanup of this activation is not complete: no new start.
    cleaning: Option<u64>,
}

/// Why `open` gave no entry.
enum Open {
    /// A blocked feature in multiplayer, or a game policy (the state detail).
    Blocked(String),
    Refused(String),
    Wait(String),
    /// Admitted, then the library did not load: cleanup ran (it can still be pending).
    Cleaned(String),
}

pub use crate::active::PageTarget;

/// A message of the app for one feature that a host gives on its feature thread (app_message).
#[derive(Clone, Debug, PartialEq)]
pub enum AppMessage {
    /// From the feature's page or a widget action: a topic and JSON text (FeatureSet::page
    /// checks both). `target`: only that start of the feature may get it.
    Page { topic: String, data: String, target: Option<PageTarget> },
    /// The settings projection of one version at a revision (FeatureSet::settings).
    Settings { version: String, revision: u64, values: String },
}

impl AppMessage {
    /// Its name in the host log.
    pub fn shown(&self) -> String {
        match self {
            AppMessage::Page { topic, .. } => format!("page {topic}"),
            AppMessage::Settings { revision, .. } => format!("settings {revision}"),
        }
    }
}

/// Status detail of a feature whose list version is not its session version (decisions 2 and 5
/// of the feature session sync plan): it starts at the next game start.
pub const UPDATE_WAITS: &str = "the update starts at the next application session";
/// Status detail of a feature that `sync` stopped because the list no longer has it.
pub const TURNED_OFF: &str = "turned off in the app";
/// Status detail of a memory or color pass feature that stopped at an app link loss, and the
/// host's admission text while no app link is live (plan 12.2, "Other host rules").
pub const APP_LOST: &str = "waits for Feature Machine";
/// Status detail of a feature with the multiplayer flag "blocked" in a multiplayer session.
pub const NOT_IN_MULTIPLAYER: &str = "not loaded in multiplayer";

/// Status detail of a feature that a game policy keeps off (PolicyStop): the policy reason.
pub fn policy_detail(stop: &PolicyStop) -> String {
    format!("application policy {}: {}", stop.policy, stop.reason)
}

pub struct FeatureSet {
    items: Vec<Item>,
    backend: Arc<dyn Backend>,
    multiplayer: bool,
    /// The multiplayer client that the host found (set_multiplayer), for policies that name it.
    client: Option<String>,
    /// The game policies of this game from the last list (load_more, sync).
    stops: Vec<PolicyStop>,
    /// Each policy that applied in this game session: a lifted policy still refuses a start
    /// until the next game start, also for a feature that never ran here (Sol r3).
    applied: Vec<PolicyStop>,
    net: Arc<Mutex<Net>>,
    /// Game id of this set: the prefix of the subscribe keys.
    game: String,
    subs: Arc<Mutex<Subs>>,
    /// Controls that are not at rest in their feature (a button down, an axis off 0): the
    /// release rule gives them their rest value when the app goes away.
    held: BTreeMap<(String, String), Control>,
    /// The session version of each feature id (`sync` only): the installed versions at the host
    /// start (`pin`), and a new install's version at its first start.
    pins: BTreeMap<String, String>,
    /// This host instance (random) and its last activation id: an activation is unique and
    /// grows in the instance, so the app can tell a new start from a late message of an old one.
    instance: u64,
    last_activation: u64,
}

/// A random id for one host instance, from the OS generator (std's hash keys): an ATS plugin
/// reload in the same game process gets a new one, so no wall clock is needed for order.
pub fn new_instance() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u32(std::process::id());
    h.finish().max(1)
}

/// Starts one feature from its entry (the loaded library, or a test feature). The HostApi
/// carries the feature's own version; an API 1 feature reads only the first fields.
fn start(meta: &ActiveFeature, activation: u64, game: &str, backend: &Arc<dyn Backend>, net: &Arc<Mutex<Net>>, subs: &Arc<Mutex<Subs>>, entry: Entry) -> Result<Running, String> {
    // A feature with user files gets the data folder of the game's user folder (files.rs
    // data_dir_for): another folder (-homedir) has its own, which the exit hook of that session
    // gets too. A refused folder keeps the base one: no user file call works then.
    let data_dir = match (&meta.data_dir, meta.capabilities.contains(&Capability::UserFiles)) {
        (Some(base), true) => Some(backend.user_root(game).map_or_else(|_| base.clone(), |r| crate::files::data_dir_for(base, game, &r))),
        (d, _) => d.clone(),
    };
    // The settings file is read at every start (also a restart after a link loss or an update),
    // so the start runs the current values. A bad file is logged; the feature uses its defaults.
    let (settings_revision, settings) = match (&meta.settings, entry.api >= 6) {
        (Some(file), true) => crate::settings::SettingsFile::load(file).map(|f| f.start(&meta.version)).unwrap_or_else(|e| {
            backend.log(&format!("feature {} settings not read: {e}", meta.id));
            (0, "{}".into())
        }),
        _ => (0, "{}".into()),
    };
    let ctx = Box::new(Ctx {
        id: meta.id.clone(),
        activation,
        caps: meta.capabilities.clone(),
        commands: meta.game_commands.clone(),
        hosts: meta.network_hosts.clone(),
        game: game.to_string(),
        data_dir: data_dir.as_deref().and_then(|p| p.to_str()).unwrap_or("").to_string(),
        backend: backend.clone(),
        net: net.clone(),
        subs: subs.clone(),
        stopping: AtomicBool::new(false),
        memory_maps: meta.map_ids().map(String::from).collect(),
        held: Mutex::new((Vec::new(), None)),
        shaders: meta.library.parent().and_then(|p| p.parent()).and_then(|p| p.parent()).filter(|p| !p.as_os_str().is_empty()).map(|v| v.join("shaders")),
        user_files: meta.user_files.clone(),
        settings,
        settings_revision,
    });
    let game_started_us = backend.application_started().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_micros() as u64);
    let host = Box::new(HostApi {
        api_version: entry.api,
        ctx: &*ctx as *const Ctx as *mut c_void,
        game: ctx.game.as_ptr(),
        game_len: ctx.game.len(),
        ui_send: ui_send_cb,
        input: input_cb,
        log: log_cb,
        game_command: game_command_cb,
        http_get: http_get_cb,
        data_dir: ctx.data_dir.as_ptr(),
        data_dir_len: ctx.data_dir.len(),
        subscribe: subscribe_cb,
        unsubscribe: unsubscribe_cb,
        send_event: send_event_cb,
        memory_acquire: memory_acquire_cb,
        memory_set: memory_set_cb,
        memory_release: memory_release_cb,
        render_acquire: render_acquire_cb,
        render_set: render_set_cb,
        render_release: render_release_cb,
        shader_acquire: shader_acquire_cb,
        shader_set: shader_set_cb,
        game_started_us,
        file_info: file_info_cb,
        file_read: file_read_cb,
        file_write: file_write_cb,
        settings: ctx.settings.as_ptr(),
        settings_len: ctx.settings.len(),
        settings_revision: ctx.settings_revision,
    });
    let start_fn = entry.start;
    let mut out = std::mem::MaybeUninit::<FeatureApi>::zeroed();
    let code = catch_unwind(AssertUnwindSafe(|| unsafe { start_fn(&*host, out.as_mut_ptr()) })).map_err(|_| "panic in start".to_string())?;
    if code != abi::OK {
        return Err(format!("start failed (code {code})"));
    }
    // Every field of FeatureApi is valid when zeroed (null pointer, None), so this is defined
    // even when a C feature returned OK without filling the table, and an older feature that
    // wrote its shorter table leaves on_http None. A version 3 table must have on_http, and a
    // version 4 table also on_game_event.
    let api = unsafe { out.assume_init() };
    let on_http = if entry.api >= 3 { api.on_http } else { None };
    let on_game_event = if entry.api >= 4 { api.on_game_event } else { None };
    let complete = (entry.api < 3 || on_http.is_some()) && (entry.api < 4 || on_game_event.is_some());
    match (api.api_version == entry.api && !api.instance.is_null(), api.on_frame, api.on_event, api.stop) {
        (true, Some(on_frame), Some(on_event), Some(stop)) if complete => Ok(Running { instance: api.instance, on_frame, on_event, stop, on_http, on_game_event, _host: host, _ctx: ctx }),
        _ => Err("start gave a bad feature table".into()),
    }
}

/// Loads a library and returns its entry. The library is leaked on purpose (header).
fn load_library(meta: &ActiveFeature) -> Result<Entry, String> {
    let lib = unsafe { libloading::Library::new(&meta.library) }.map_err(|e| format!("cannot load {}: {e}", meta.library.display()))?;
    let version: abi::ApiVersionFn = *unsafe { lib.get::<abi::ApiVersionFn>(abi::SYM_API_VERSION) }.map_err(|_| "not a feature library (no fm_feature_api_version)".to_string())?;
    let v = unsafe { version() };
    if !(abi::MIN_API_VERSION..=abi::API_VERSION).contains(&v) {
        return Err(format!("built for feature API {v}, this host loads {} to {}", abi::MIN_API_VERSION, abi::API_VERSION));
    }
    let start: abi::StartFn = *unsafe { lib.get::<abi::StartFn>(abi::SYM_START) }.map_err(|_| "no fm_feature_start".to_string())?;
    std::mem::forget(lib);
    Ok(Entry { start, api: v })
}

impl FeatureSet {
    /// An empty set for `game` (tests add features with add).
    pub fn new(game: &str, backend: Arc<dyn Backend>, multiplayer: bool) -> FeatureSet {
        FeatureSet { items: vec![], backend, multiplayer, client: None, stops: vec![], applied: vec![], net: Default::default(), game: game.to_string(), subs: Default::default(), held: BTreeMap::new(), pins: BTreeMap::new(), instance: new_instance(), last_activation: 0 }
    }

    /// This host instance (Hello.instance).
    pub fn instance(&self) -> u64 {
        self.instance
    }

    /// A host that sends its Hello before the set exists (ATS: the set comes at the first
    /// telemetry frame) makes the id at its init and gives it here, before any start.
    pub fn with_instance(mut self, instance: u64) -> FeatureSet {
        self.instance = instance;
        self
    }

    fn next_activation(&mut self) -> u64 {
        self.last_activation += 1;
        self.last_activation
    }

    /// An item got Admit::Wait: the host calls `sync` again each 1 s and at a link, sim, list or
    /// cleanup change (plan 12.2, contract 1 "Retry").
    pub fn waits(&self) -> bool {
        self.items.iter().any(|i| i.waiting)
    }

    /// The end of a Completion::Pending cleanup. One for another activation, or a second one, is
    /// ignored and logged; a failure is final for the game session (no new start).
    pub fn cleaned(&mut self, id: &str, activation: u64, result: Result<(), String>) {
        let Some(item) = self.items.iter_mut().find(|i| i.meta.id == id && i.cleaning == Some(activation)) else {
            self.backend.log(&format!("feature {id}: cleanup result for activation {activation} ignored (not pending)"));
            return;
        };
        item.cleaning = None;
        if let Err(e) = result { self.cleanup_failed(id, &e); }
    }

    /// The session versions for `sync`: the installed version of each feature when the host
    /// started (active::installed_versions), read before any app link, so an update that comes
    /// later in the session waits for the next game start (plan decisions 2 and 5).
    pub fn pin(&mut self, versions: BTreeMap<String, String>) {
        self.pins = versions;
    }

    /// Loads and starts every feature of `list` for `game`. A feature that fails is listed with
    /// its reason; the others still run. `multiplayer`: blocked features do not start.
    pub fn load(game: &str, list: &ActiveList, backend: Arc<dyn Backend>, multiplayer: bool) -> FeatureSet {
        let mut set = FeatureSet::new(game, backend, multiplayer);
        set.load_more(list);
        set
    }

    /// A host can defer session-bound features until its first app connection. Existing items
    /// never restart, so reconnect cannot revive a stopped owner or duplicate an instance.
    pub fn load_more(&mut self, list: &ActiveList) {
        self.take_stops(list);
        for meta in list.for_game(&self.game.clone()) {
            if self.items.iter().any(|i| i.meta.id == meta.id) { continue; }
            let activation = self.next_activation();
            let start_fn = self.open(meta, activation, load_library);
            let item = self.launch(meta.clone(), activation, start_fn, None);
            self.items.push(item);
        }
    }

    /// The checks before a start, then admission and `load`: the entry, or why not. A load
    /// failure after admission runs cleanup. `meta` is the item's own record for a restart, so
    /// the rules of its session version apply (plan R1-B5), not the list's newer version.
    fn open(&self, meta: &ActiveFeature, activation: u64, load: impl FnOnce(&ActiveFeature) -> Result<Entry, String>) -> Result<Entry, Open> {
        if meta.multiplayer == Multiplayer::Blocked && self.multiplayer {
            Err(Open::Blocked(NOT_IN_MULTIPLAYER.into()))
        } else if let Some(stop) = self.stop_for(meta) {
            Err(Open::Blocked(policy_detail(stop)))
        } else if meta.capabilities.contains(&Capability::Unknown) {
            // Checked before the load: a library that needs a call this host lacks never runs.
            Err(Open::Refused(UNKNOWN_CAPABILITY.into()))
        } else {
            match self.backend.admit(meta, activation) {
                Admit::Ok => load(meta).map_err(|e| match self.backend.cleanup(&meta.id, activation) {
                    Ok(()) => Open::Cleaned(e),
                    Err(c) => {
                        self.backend.log(&format!("feature {} cleanup failed after a load failure: {c}", meta.id));
                        Open::Refused(format!("{e}; cleanup failed; restart the application session: {c}"))
                    }
                }),
                Admit::Wait(why) => Err(Open::Wait(why)),
                Admit::Refuse(why) => Err(Open::Refused(why)),
            }
        }
    }

    /// Makes the running features equal to the features of `list` for this game (feature session
    /// sync plan): a running feature that is not in the list stops; a listed feature that does not
    /// run starts, at its session version only (`pins`). A cleanly stopped feature starts again
    /// from its own record, whatever version the list names. Failed, blocked and cleanup failures
    /// stay as they are until the next game start (no start loop). Returns the ids that started.
    pub fn sync(&mut self, list: &ActiveList) -> Vec<String> {
        self.sync_with(list, load_library)
    }

    /// `sync` with the library loader as a parameter, so tests give entries from this binary.
    fn sync_with(&mut self, list: &ActiveList, load: impl Fn(&ActiveFeature) -> Result<Entry, String>) -> Vec<String> {
        let game = self.game.clone();
        // On or off goes by id: an update can change the games of the new version, but this
        // session keeps the version that it has (review R2-B3). The new games count for a
        // feature that did not start yet.
        let wanted: Vec<ActiveFeature> = list.features.clone();
        self.take_stops(list);
        self.apply_stops();
        let gone: Vec<String> = self.items.iter().filter(|i| i.run.is_some() && !wanted.iter().any(|m| m.id == i.meta.id)).map(|i| i.meta.id.clone()).collect();
        for id in gone {
            self.stop(&id, State::Stopped, Some(TURNED_OFF.into()));
        }
        let mut started = vec![];
        for meta in wanted {
            let item = self.items.iter().find(|i| i.meta.id == meta.id);
            let start_fn = match item {
                Some(i) if i.run.is_some() => continue,
                // A pending cleanup of the last start: no new start until FeatureSet::cleaned.
                Some(i) if i.cleaning.is_some() => continue,
                // The session version stopped cleanly: the same record again (admit again too).
                Some(Item { meta: own, entry: Some(e), status, .. }) if status.state == State::Stopped => {
                    let (own, e) = (own.clone(), *e);
                    let activation = self.next_activation();
                    let start_fn = self.open(&own, activation, |_| Ok(e));
                    self.put(own.clone(), activation, start_fn, Some(e));
                    if self.running(&own.id) { started.push(own.id); }
                    continue;
                }
                // Failed, blocked or a cleanup failure: until the next game start.
                Some(i) if !(i.entry.is_none() && i.status.state == State::Stopped) => continue,
                // Never started, or an update that waits: only the session version starts.
                _ => {
                    if !meta.games.contains(&game) {
                        continue;
                    }
                    let pin = self.pins.entry(meta.id.clone()).or_insert_with(|| meta.version.clone()).clone();
                    if pin != meta.version {
                        if item.is_none_or(|i| i.meta.version != meta.version) {
                            let status = FeatureStatus { id: meta.id.clone(), version: meta.version.clone(), state: State::Stopped, detail: Some(UPDATE_WAITS.into()), pass: None, activation: None, settings_revision: None };
                            self.backend.log(&format!("feature {} {}: waits, the session version is {pin} ({UPDATE_WAITS})", meta.id, meta.version));
                            self.replace(Item { meta, entry: None, run: None, status, activation: 0, waiting: false, cleaning: None });
                        }
                        continue;
                    }
                    let activation = self.next_activation();
                    (activation, self.open(&meta, activation, &load))
                }
            };
            self.put(meta.clone(), start_fn.0, start_fn.1, None);
            if self.running(&meta.id) { started.push(meta.id); }
        }
        started
    }

    fn running(&self, id: &str) -> bool {
        self.items.iter().any(|i| i.meta.id == id && i.run.is_some())
    }

    /// Starts a feature in place of its old item, or as a new item. `kept`: the entry of a
    /// stopped item, which a Wait keeps for the next try.
    fn put(&mut self, meta: ActiveFeature, activation: u64, start_fn: Result<Entry, Open>, kept: Option<Entry>) {
        let item = self.launch(meta, activation, start_fn, kept);
        self.replace(item);
    }

    /// The item of that id becomes `item` (an item that does not run), or `item` is added.
    fn replace(&mut self, item: Item) {
        match self.items.iter_mut().find(|i| i.meta.id == item.meta.id) {
            Some(old) => *old = item,
            None => self.items.push(item),
        }
    }

    /// Adds one feature with its entry (tests give a function from this binary).
    pub fn add(&mut self, meta: ActiveFeature, start_fn: Result<Entry, Option<String>>) {
        let activation = self.next_activation();
        let item = self.launch(meta, activation, start_fn.map_err(|e| e.map_or_else(|| Open::Blocked(NOT_IN_MULTIPLAYER.into()), Open::Refused)), None);
        self.items.push(item);
    }

    fn launch(&mut self, meta: ActiveFeature, activation: u64, start_fn: Result<Entry, Open>, kept: Option<Entry>) -> Item {
        let entry = start_fn.as_ref().ok().copied();
        let mut status = FeatureStatus { id: meta.id.clone(), version: meta.version.clone(), state: State::Failed, detail: None, pass: None, activation: Some(activation), settings_revision: None };
        let mut waiting = false;
        // A cleanup that ran here and that the host completes later (Completion::Pending).
        let pending = self.backend.completion() == Completion::Pending;
        let mut cleaning = None;
        let run = match start_fn {
            Err(Open::Blocked(why)) => {
                status.state = State::Blocked;
                status.detail = Some(why);
                None
            }
            Err(Open::Refused(e)) => {
                status.detail = Some(e);
                None
            }
            Err(Open::Cleaned(e)) => {
                status.detail = Some(e);
                cleaning = pending.then_some(activation);
                None
            }
            // Not now: stopped with the reason, and the entry of a restart stays for the next try.
            Err(Open::Wait(why)) => {
                status.state = State::Stopped;
                status.detail = Some(why);
                waiting = true;
                None
            }
            Ok(f) => match start(&meta, activation, &self.game, &self.backend, &self.net, &self.subs, f) {
                Ok(r) => {
                    status.state = State::Running;
                    status.settings_revision = (f.api >= 6).then_some(r._ctx.settings_revision);
                    Some(r)
                }
                // Start can call the host before it fails: release what it took (inputs,
                // commands, requests, watched values).
                Err(e) => {
                    self.drop_requests(&meta.id);
                    self.backend.stopping(&meta.id);
                    self.unwatch(&meta.id);
                    self.backend.stopped(&meta.id);
                    status.detail = Some(match self.backend.cleanup(&meta.id, activation) {
                        Ok(()) => {
                            cleaning = pending.then_some(activation);
                            e
                        }
                        Err(cleanup) => {
                            self.backend.log(&format!("feature {} cleanup failed after start: {cleanup}", meta.id));
                            format!("{e}; cleanup failed; restart the application session: {cleanup}")
                        }
                    });
                    None
                }
            },
        };
        // A start that waits is tried again each list check (1 s) while the app or sim is gone:
        // the log gets a line only when the state or its reason changes, not one each try.
        let repeat = run.is_none() && self.items.iter().any(|i| i.meta.id == meta.id && i.run.is_none() && i.meta.version == meta.version && i.status.state == status.state && i.status.detail == status.detail);
        if !repeat {
            self.backend.log(&format!("feature {} {}: {:?}{}", meta.id, meta.version, status.state, status.detail.as_deref().map(|d| format!(" ({d})")).unwrap_or_default()));
        }
        // Only a started feature keeps its entry: a failed start never starts again. A Wait keeps
        // the entry of the stopped item, so its next try starts the same session version.
        let entry = if waiting { kept } else { entry.filter(|_| run.is_some()) };
        Item { meta, entry, run, status, activation, waiting, cleaning }
    }

    /// Calls every running feature with one frame. Features without vars_read get the frame
    /// time only (no values), so they still have a clock. `vars`: the watched native values by
    /// native name; a version 4 feature gets the ones it subscribed to, keyed "<game>:<native>".
    pub fn frame<N: AsRef<str>>(&mut self, seq: u64, time_us: u64, values: &[(&str, Value)], vars: &[(N, Var)]) {
        // Later readers of a name that the game had already confirmed (Subs::late). One at a
        // time from the shared queue: a "var.ready" call can unsubscribe a name that waits in it
        // (review round 3). Readers queued during this loop wait for the next frame, so a feature
        // that subscribes again in its "var.ready" call cannot hold the frame in this loop.
        let upto = lock(&self.subs).late_seq;
        loop {
            // Its own statement: the lock ends before the call (the feature can subscribe).
            let next = lock(&self.subs).next_late(upto);
            let Some((feature, native)) = next else { break };
            let attrs = [("name", Var::Text(var_key(&self.game, &native)))];
            self.send(|item| item.meta.id == feature, "var.ready", &attrs, false);
        }
        let abi_values: Vec<AbiValue> = values.iter().map(|(n, v)| v.to_abi(n)).collect();
        let keys: Vec<String> = vars.iter().map(|(n, _)| var_key(&self.game, n.as_ref())).collect();
        // Built before the calls: a feature can subscribe during its frame, which locks subs.
        let mine: Vec<Vec<AbiVar>> = {
            let subs = lock(&self.subs);
            self.items
                .iter()
                .map(|item| {
                    let Some(names) = subs.readers.get(&item.meta.id).filter(|_| item.run.as_ref().is_some_and(|r| r.on_game_event.is_some())) else { return vec![] };
                    vars.iter().zip(&keys).filter(|((n, _), _)| names.contains(n.as_ref())).map(|((_, v), k)| v.to_abi(k)).collect()
                })
                .collect()
        };
        let failed: Vec<String> = self
            .items
            .iter()
            .zip(&mine)
            .filter(|(item, mine)| {
                let Some(r) = &item.run else { return false };
                let f = if item.meta.capabilities.contains(&Capability::VarsRead) {
                    AbiFrame { seq, time_us, values: abi_values.as_ptr(), len: abi_values.len(), vars: mine.as_ptr(), vars_len: mine.len() }
                } else {
                    AbiFrame { seq, time_us, values: std::ptr::null(), len: 0, vars: std::ptr::null(), vars_len: 0 }
                };
                unsafe { (r.on_frame)(r.instance, &f) == abi::FAILED }
            })
            .map(|(i, _)| i.meta.id.clone())
            .collect();
        self.fail(failed, "frame");
        for r in self.items.iter().filter_map(|i| i.run.as_ref()) { reload_shaders(&r._ctx); }
    }

    /// A feature whose call failed (a panic) stops at once: its state and held inputs are no
    /// longer known, so it must not keep a control applied or run on with broken state.
    fn fail(&mut self, ids: Vec<String>, call: &str) {
        for id in ids {
            self.stop(&id, State::Failed, Some(format!("stopped: the feature failed in {call}")));
        }
    }

    /// A game event ("paused", "started", ...) with no attributes to every running feature.
    pub fn event(&mut self, id: &str) {
        self.game_event::<&str>(id, &[]);
    }

    /// A game event with its attributes (native names) to every running feature. A version 4
    /// feature gets the attributes when it has vars_read; an older one gets the id only.
    pub fn application_event<N: AsRef<str>>(&mut self, id: &str, attrs: &[(N, Var)]) {
        self.game_event(id, attrs);
    }

    pub fn game_event<N: AsRef<str>>(&mut self, id: &str, attrs: &[(N, Var)]) {
        // The page, control and settings prefixes belong to page(), control() and settings(): a
        // game event with one would pose as the member's page, input or settings.
        if abi::app_event(id) {
            self.backend.log(&format!("application event {id} dropped: the prefixes {}, {} and {} are for the app", abi::PAGE_EVENT, abi::CONTROL_EVENT, abi::SETTINGS_PREFIX));
            return;
        }
        self.send(|_| true, id, attrs, false);
    }

    /// A game event to one running feature only, with the same rules as game_event: a host gives
    /// a feature that starts during a session the current state that the others already have (the
    /// ATS configuration events).
    pub fn game_event_to<N: AsRef<str>>(&mut self, feature: &str, id: &str, attrs: &[(N, Var)]) {
        if abi::app_event(id) {
            self.backend.log(&format!("application event {id} dropped: the prefixes {}, {} and {} are for the app", abi::PAGE_EVENT, abi::CONTROL_EVENT, abi::SETTINGS_PREFIX));
            return;
        }
        self.send(|i| i.meta.id == feature, id, attrs, false);
    }

    /// A message from the page of `feature` in the app, to that feature only (abi::PAGE_EVENT).
    /// It needs ui_app, a topic of 1 to 64 characters a-z, 0-9, '.', '_' or '-', and JSON of at
    /// most PAGE_LIMIT bytes. Err: why it did not go (no such running feature, or a bad message).
    pub fn page(&mut self, feature: &str, topic: &str, json: &str) -> Result<(), String> {
        let item = self.items.iter().find(|i| i.meta.id == feature && i.run.is_some()).ok_or_else(|| format!("{feature} does not run"))?;
        if !item.meta.capabilities.contains(&Capability::UiApp) { return Err(format!("{feature} has no page (ui_app)")); }
        let topic_ok = (1..=64).contains(&topic.len()) && topic.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b".-_".contains(&b));
        if !topic_ok { return Err(format!("bad page topic {topic:?}")); }
        if json.len() > PAGE_LIMIT || serde_json::from_str::<serde_json::Value>(json).is_err() { return Err("the page message is not JSON or is too large".into()); }
        let id = format!("{}{topic}", abi::PAGE_EVENT);
        self.send(|i| i.meta.id == feature, &id, &[(abi::PAGE_DATA, Var::Text(json.into()))], true);
        Ok(())
    }

    /// New settings of `feature` from the app (abi::SETTINGS_EVENT, API 6): `values` is the full
    /// projection of `version` at `revision`. It goes only to a version 6 feature that runs that
    /// version, and only when the revision is newer than the one that the activation runs (a
    /// resend after a reconnect is not a change). Ok: the Ack detail; the report then names the
    /// revision (FeatureStatus::settings_revision), which is the app's applied state.
    pub fn settings(&mut self, feature: &str, version: &str, revision: u64, values: &str) -> Result<String, String> {
        let item = self.items.iter().find(|i| i.meta.id == feature && i.run.is_some()).ok_or_else(|| format!("{feature} does not run"))?;
        let Some(current) = item.status.settings_revision.filter(|_| item.entry.is_some_and(|e| e.api >= 6)) else { return Err(format!("{feature} takes no settings (feature API 5 or older)")) };
        if item.meta.version != version { return Err(format!("{feature} runs {}, not {version}", item.meta.version)); }
        if !crate::settings::valid_values(values) { return Err("the settings are not a JSON object or are too large".into()); }
        if revision <= current { return Ok(format!("settings {feature} {revision}: {current} runs already")); }
        let attrs = [(abi::SETTINGS_DATA, Var::Text(values.into())), (abi::SETTINGS_REVISION, Var::U64(revision))];
        self.send(|i| i.meta.id == feature, abi::SETTINGS_EVENT, &attrs, true);
        // A feature that failed in the call stopped (send): its revision does not change.
        match self.items.iter_mut().find(|i| i.meta.id == feature && i.run.is_some()) {
            Some(i) => {
                i.status.settings_revision = Some(revision);
                Ok(format!("settings {feature} {revision} delivered"))
            }
            None => Err(format!("{feature} failed in its settings call")),
        }
    }

    /// One message of the app for one feature (a page message or settings), as the hosts queue
    /// them between their I/O thread and the feature thread. Ok: the Ack detail.
    pub fn app_message(&mut self, feature: &str, message: &AppMessage) -> Result<String, String> {
        match message {
            AppMessage::Page { topic, data, target } => {
                // A start that ended (a restart while the message waited) does not get it.
                if let Some(t) = target {
                    let now = self.items.iter().find(|i| i.meta.id == feature && i.run.is_some()).map(|i| i.activation);
                    if t.instance != self.instance || now != Some(t.activation) {
                        return Err(format!("this start of {feature} ended"));
                    }
                }
                self.page(feature, topic, data).map(|_| format!("page {feature}/{topic} delivered"))
            }
            AppMessage::Settings { version, revision, values } => self.settings(feature, version, *revision, values),
        }
    }

    /// A control event from the app's input reader, to that feature only (abi::CONTROL_EVENT).
    /// It needs the controls capability and a control id of 1 to 64 characters a-z, 0-9, '.',
    /// '_' or '-'; the app checks the id against the feature's list. Err: why it did not go.
    pub fn control(&mut self, feature: &str, control: &str, value: Control) -> Result<(), String> {
        let item = self.items.iter().find(|i| i.meta.id == feature && i.run.is_some()).ok_or_else(|| format!("{feature} does not run"))?;
        if !item.meta.capabilities.contains(&Capability::Controls) { return Err(format!("{feature} has no controls capability")); }
        if !valid_control_id(control) { return Err(format!("bad control id {control:?}")); }
        let key = (feature.to_string(), control.to_string());
        if matches!(value, Control::Button(true)) || matches!(value, Control::Axis(v) if v != 0.0) { self.held.insert(key, value); } else { self.held.remove(&key); }
        self.give_control(feature, control, value);
        Ok(())
    }

    fn give_control(&mut self, feature: &str, control: &str, value: Control) {
        let id = format!("{}{control}", abi::CONTROL_EVENT);
        let (name, var) = value.to_attr();
        self.send(|i| i.meta.id == feature, &id, &[(name, var)], true);
    }

    /// The release rule in the game: each held control of a running feature goes to rest (button
    /// up, axis 0). For an app link loss or a stuck control queue, where the input reader's own
    /// release can not arrive.
    pub fn release_controls(&mut self) {
        self.release_where(|_| true);
    }

    fn release_where(&mut self, feature: impl Fn(&str) -> bool) {
        let gone: Vec<_> = self.held.keys().filter(|(f, _)| feature(f)).cloned().collect();
        for key in gone {
            let Some(value) = self.held.remove(&key) else { continue };
            let rest = if matches!(value, Control::Button(_)) { Control::Button(false) } else { Control::Axis(0.0) };
            if self.items.iter().any(|i| i.meta.id == key.0 && i.run.is_some()) { self.give_control(&key.0, &key.1, rest); }
        }
    }

    /// The game's answer for a watched native value: Ok (the game has it) or Err (no such name
    /// or type; the host has already stopped watching it). Each feature that subscribed gets the
    /// event "var.ready" or "var.unknown"; an unknown name leaves every feature's list.
    /// An answer for a name with no reader is old (all readers left first) and is dropped; a
    /// second Ok while the name stays read (an unsubscribe and subscribe again before the old
    /// answer came, or an MSFS reconnect) gives no second "var.ready" (review round 2).
    pub fn var_result(&mut self, native: &str, result: Result<(), String>) {
        let key = var_key(&self.game, native);
        let readers: Vec<String> = {
            let mut subs = lock(&self.subs);
            if !subs.used(native) {
                return;
            }
            let readers = subs.readers.iter().filter(|(_, s)| s.contains(native)).map(|(f, _)| f.clone()).collect();
            match result {
                Ok(()) if !subs.ready.insert(native.to_string()) => return,
                Ok(()) => {}
                Err(_) => {
                    subs.readers.values_mut().for_each(|s| {
                        s.remove(native);
                    });
                    subs.forget(native);
                }
            }
            readers
        };
        let (id, attrs) = match &result {
            Ok(()) => ("var.ready", vec![("name", Var::Text(key))]),
            Err(why) => ("var.unknown", vec![("name", Var::Text(key)), ("reason", Var::Text(why.clone()))]),
        };
        self.send(|item| readers.contains(&item.meta.id), id, &attrs, false);
    }

    /// Game data in attributes needs vars_read; `page` attributes are the feature's own page data.
    fn send<N: AsRef<str>>(&mut self, to: impl Fn(&Item) -> bool, id: &str, attrs: &[(N, Var)], page: bool) {
        let abi_attrs: Vec<AbiVar> = attrs.iter().map(|(n, v)| v.to_abi(n.as_ref())).collect();
        let failed: Vec<String> = self
            .items
            .iter()
            .filter(|item| to(item))
            .filter(|item| {
                let Some(r) = &item.run else { return false };
                let code = match r.on_game_event {
                    Some(on_game_event) => {
                        let a: &[AbiVar] = if page || item.meta.capabilities.contains(&Capability::VarsRead) { &abi_attrs } else { &[] };
                        let e = AbiEvent { id: id.as_ptr(), id_len: id.len(), attrs: a.as_ptr(), attrs_len: a.len() };
                        unsafe { on_game_event(r.instance, &e) }
                    }
                    None => unsafe { (r.on_event)(r.instance, id.as_ptr(), id.len()) },
                };
                code == abi::FAILED
            })
            .map(|i| i.meta.id.clone())
            .collect();
        self.fail(failed, "event");
    }

    /// The native values that only `feature` read are no longer watched.
    fn unwatch(&self, feature: &str) {
        let gone = lock(&self.subs).drop_feature(feature);
        for n in gone {
            if let Err(e) = watch(self.backend.as_ref(), &n, false) {
                self.backend.log(&format!("[{feature}] unwatch {n}: {e}"));
            }
        }
    }

    /// Stops one feature (turned off, blocked, failed, session end) and tells the backend, which
    /// releases the inputs that the feature holds. False when it does not run.
    pub fn stop(&mut self, id: &str, state: State, detail: Option<String>) -> bool {
        if !self.items.iter().any(|i| i.meta.id == id && i.run.is_some()) { return false; }
        // Release rule: held controls go to rest before the stop call. A feature that fails in
        // that call is stopped as Failed inside it; it still stopped here, so the result is true.
        self.release_where(|f| f == id);
        let Some(item) = self.items.iter_mut().find(|i| i.meta.id == id) else { return true };
        let Some(r) = item.run.take() else { return true };
        r._ctx.stopping.store(true, Ordering::Release);
        self.backend.stopping(id);
        unsafe { (r.stop)(r.instance) };
        item.status.state = state;
        item.status.detail = detail;
        let line = format!("feature {id}: {state:?}{}", item.status.detail.as_deref().map(|d| format!(" ({d})")).unwrap_or_default());
        let activation = item.activation;
        lock(&self.net).waiting.retain(|w| w.feature != id);
        self.unwatch(id);
        self.backend.stopped(id);
        match self.backend.cleanup(id, activation) {
            Err(e) => self.cleanup_failed(id, &e),
            // The host completes it later (FeatureSet::cleaned); until then no new start.
            Ok(()) if self.backend.completion() == Completion::Pending => {
                if let Some(item) = self.items.iter_mut().find(|i| i.meta.id == id) { item.cleaning = Some(activation); }
            }
            Ok(()) => {}
        }
        self.backend.log(&line);
        true
    }

    /// A host can finish queued cleanup after the feature call returns. Its failure must
    /// reach the same status as a synchronous cleanup failure.
    pub fn cleanup_failed(&mut self, id: &str, error: &str) {
        if let Some(item) = self.items.iter_mut().find(|i| i.meta.id == id) {
            item.status.state = State::Failed;
            item.status.detail = Some(format!("cleanup failed; restart the application session: {error}"));
            self.backend.log(&format!("feature {id}: cleanup failed: {error}"));
        }
    }

    /// A multiplayer client was found (ATS: TruckersMP): features with the flag "blocked" and
    /// features that a policy for this client stops "now" stop at once. Returns true when a
    /// feature stopped. They start again only at the next game start (no start loop).
    pub fn set_multiplayer(&mut self, client: &str) -> bool {
        let first = !self.multiplayer;
        self.multiplayer = true;
        self.client = Some(client.to_string());
        self.keep_applied();
        let blocked: Vec<String> = self.items.iter().filter(|i| first && i.meta.multiplayer == Multiplayer::Blocked && i.run.is_some()).map(|i| i.meta.id.clone()).collect();
        for id in &blocked {
            self.stop(id, State::Blocked, Some("stopped: multiplayer started".into()));
        }
        self.apply_stops() | !blocked.is_empty()
    }

    /// Keeps the policies of this game from the list: the newest list decides, also for an
    /// item that runs its own session record.
    fn take_stops(&mut self, list: &ActiveList) {
        self.stops = list.stops.iter().filter(|s| s.game == self.game).cloned().collect();
        self.keep_applied();
    }

    /// Adds the policies that apply now (this client, or always) to `applied`.
    fn keep_applied(&mut self) {
        let client = self.client.clone();
        for s in self.stops.iter().filter(|s| s.applies(client.as_deref())) {
            if !self.applied.contains(s) { self.applied.push(s.clone()); }
        }
    }

    /// The policy that keeps this feature record off in this game session, if any.
    fn stop_for(&self, meta: &ActiveFeature) -> Option<&PolicyStop> {
        self.applied.iter().find(|s| s.is_for(meta))
    }

    /// Stops each running feature that a policy stops "now" (decision D4); a "next start"
    /// policy lets the running start go on, and `open` refuses the next one. True: one stopped.
    fn apply_stops(&mut self) -> bool {
        let client = self.client.as_deref();
        let now_stop = |m: &ActiveFeature| self.stops.iter().find(|s| s.is_for(m) && s.apply == Apply::Now && s.applies(client));
        let now: Vec<(String, String)> = self.items.iter().filter(|i| i.run.is_some()).filter_map(|i| now_stop(&i.meta).map(|s| (i.meta.id.clone(), policy_detail(s)))).collect();
        for (id, why) in &now {
            self.stop(id, State::Blocked, Some(format!("stopped: {why}")));
        }
        !now.is_empty()
    }

    pub fn multiplayer(&self) -> bool {
        self.multiplayer
    }

    pub fn statuses(&self) -> Vec<FeatureStatus> {
        self.items.iter().map(|i| i.status.clone()).collect()
    }

    /// The report for the app (wire message Features).
    pub fn report(&self) -> crate::active::HostReport {
        crate::active::HostReport { multiplayer: self.multiplayer, error: None, features: self.statuses(), session_versions: self.pins.clone(), user_root: self.other_user_root() }
    }

    /// The game's user folder when it is not the default one (ATS -homedir), as UTF-8 text for
    /// the session record. Reports come often, so a backend keeps its answer (the ATS plugin
    /// finds the folder once per game process).
    fn other_user_root(&self) -> Option<String> {
        let root = self.backend.user_root(&self.game).ok()?;
        // One compare with data_dir_for (review r2): an alias of the default folder is it.
        let default = crate::files::default_user_root(&self.game).is_some_and(|d| crate::files::same_user_root(&d, &root));
        (!default).then(|| root.to_str().map(String::from)).flatten()
    }

    /// The answer of the app to request `id` of `feature`, given to that feature now (feature
    /// thread). False when no such request waits (it timed out, or the feature stopped): the
    /// answer is dropped, so a late or foreign answer never reaches a feature.
    pub fn http_answer(&mut self, feature: &str, id: u64, answer: Result<HttpAnswer, HostError>) -> bool {
        {
            let mut net = lock(&self.net);
            let Some(i) = net.waiting.iter().position(|w| w.id == id && w.feature == feature) else { return false };
            net.waiting.remove(i);
        }
        self.deliver(feature, id, answer);
        true
    }

    /// Gives TIMEOUT to each request that waits longer than HTTP_TIMEOUT, so a feature never
    /// waits forever (lost app link, dropped message). Hosts call it on each loop pass.
    pub fn expire(&mut self) {
        self.expire_at(Instant::now());
    }

    pub fn expire_at(&mut self, now: Instant) {
        let late: Vec<Waiting> = {
            let mut net = lock(&self.net);
            let (late, keep) = std::mem::take(&mut net.waiting).into_iter().partition(|w| now.saturating_duration_since(w.since) >= HTTP_TIMEOUT);
            net.waiting = keep;
            late
        };
        for w in late {
            self.deliver(&w.feature, w.id, Err(HostError::Timeout));
        }
    }

    /// Gives `error` now to every request that waits. For a lost app link: the app forgot those
    /// requests, so no answer comes; Busy at once (not Timeout after 60 s) tells the feature to
    /// try again later, not that its source failed.
    pub fn fail_waiting(&mut self, error: HostError) {
        let all = std::mem::take(&mut lock(&self.net).waiting);
        for w in all {
            self.deliver(&w.feature, w.id, Err(error));
        }
    }

    /// Requests that wait for an answer (all features).
    pub fn waiting(&self) -> usize {
        lock(&self.net).waiting.len()
    }

    fn drop_requests(&self, feature: &str) {
        lock(&self.net).waiting.retain(|w| w.feature != feature);
    }

    fn deliver(&mut self, feature: &str, id: u64, answer: Result<HttpAnswer, HostError>) {
        let Some(r) = self.items.iter().find(|i| i.meta.id == feature).and_then(|i| i.run.as_ref()) else { return };
        let Some(on_http) = r.on_http else { return };
        let (status, body): (i32, &[u8]) = match &answer {
            Ok(a) if (1..=999).contains(&a.status) => (a.status as i32, &a.body),
            Ok(_) => (abi::FAILED, &[]),
            Err(e) => (e.code(), &[]),
        };
        if unsafe { on_http(r.instance, id, status, body.as_ptr(), body.len()) } == abi::FAILED {
            self.fail(vec![feature.to_string()], "http");
        }
    }

    pub fn stop_all(&mut self) {
        let ids: Vec<String> = self.items.iter().filter(|i| i.run.is_some()).map(|i| i.meta.id.clone()).collect();
        for id in ids {
            self.stop(&id, State::Stopped, None);
        }
    }

    /// App disconnect revokes only memory and color pass users, and releases held controls.
    /// Read-only API 1 to 4 behavior is unchanged. A revoked item is Stopped with its entry, so
    /// it starts again with a new activation when the host admits it (plan 12.2).
    pub fn disconnect(&mut self) {
        self.release_controls();
        self.revoke(APP_LOST);
    }

    /// Stops the memory and color pass users with `detail` (an app link or a sim session that
    /// ended: their activation was bound to it). The host's admission says when they can start.
    pub fn revoke(&mut self, detail: &str) {
        let ids: Vec<_> = self.items.iter().filter(|i| i.run.is_some() && i.meta.capabilities.iter().any(|c| matches!(c, Capability::VarsMemory | Capability::VarsWrite | Capability::RenderPass))).map(|i| i.meta.id.clone()).collect();
        for id in ids { self.stop(&id, State::Stopped, Some(detail.into())); }
    }
}

impl Drop for FeatureSet {
    fn drop(&mut self) {
        self.stop_all();
    }
}

/// Control events between a host's I/O thread and its feature thread. Buttons keep their order
/// and are never merged; an axis value replaces a waiting value of the same control, and deltas
/// add up, unless a button event of that control came after it. So a slow feature thread
/// gets each press and the latest lever position, and the queue stays small. `link` is the app
/// link that sent the event: the host drops events of a lost link (they came before its release).
#[derive(Default)]
pub struct ControlQueue {
    items: std::collections::VecDeque<(u64, String, String, Control)>,
    release: bool,
}

/// What the feature thread takes: when `release` is true, it calls FeatureSet::release_controls
/// before it gives `events` (the queue was full and dropped what waited).
#[derive(Debug, Default, PartialEq)]
pub struct ControlBatch {
    pub release: bool,
    pub events: Vec<(u64, String, String, Control)>,
}

impl ControlQueue {
    /// Button events that may wait; a queue this full means the feature thread is stuck.
    pub const MAX: usize = 1024;

    /// Err: the queue was full. It dropped the events that waited (not this one) and the next
    /// batch releases all held controls, so a dropped "up" never leaves a button down.
    pub fn push(&mut self, link: u64, feature: String, control: String, value: Control) -> Result<(), String> {
        if let Some(last) = self.items.iter_mut().rev().find(|(l, f, c, _)| *l == link && *f == feature && *c == control) {
            match (&mut last.3, value) {
                (Control::Axis(v), Control::Axis(n)) => { *v = n; return Ok(()); }
                (Control::Delta(v), Control::Delta(n)) => { *v += n; return Ok(()); }
                _ => {}
            }
        }
        let mut r = Ok(());
        if self.items.len() >= Self::MAX {
            r = Err(format!("{} control events waited for a stuck feature thread: dropped, and held controls go to rest", self.items.len()));
            self.items.clear();
            self.release = true;
        }
        self.items.push_back((link, feature, control, value));
        r
    }

    pub fn take(&mut self) -> ControlBatch {
        ControlBatch { release: std::mem::take(&mut self.release), events: self.items.drain(..).collect() }
    }
}

fn ctx<'a>(p: *mut c_void) -> Option<&'a Ctx> {
    unsafe { (p as *const Ctx).as_ref() }
}

/// The callbacks run inside feature calls: a panic here must not unwind into the feature.
unsafe extern "C" fn ui_send_cb(c: *mut c_void, topic: *const u8, topic_len: usize, json: *const u8, json_len: usize) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(c) = ctx(c) else { return abi::BAD_ARGS };
        if !c.caps.contains(&Capability::UiApp) {
            return abi::REFUSED;
        }
        match (abi::text(topic, topic_len), abi::text(json, json_len)) {
            (Some(t), Some(j)) if !t.is_empty() && t.len() <= 64 && j.len() <= 64 * 1024 => {
                match c.backend.ui_send(&c.id, c.activation, t, j) { Ok(()) => abi::OK, Err(e) => e.code() }
            }
            _ => abi::BAD_ARGS,
        }
    }))
    .unwrap_or(abi::FAILED)
}

unsafe extern "C" fn input_cb(c: *mut c_void, name: *const u8, name_len: usize, kind: u32, value: f32, ms: u32) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(c) = ctx(c) else { return abi::BAD_ARGS };
        if !c.caps.contains(&Capability::InputInject) {
            return abi::REFUSED;
        }
        let input = match kind {
            abi::INPUT_PRESS if ms > 0 && ms <= 10_000 => Input::Press { ms },
            abi::INPUT_AXIS if value.is_finite() && (-1.0..=1.0).contains(&value) => Input::Axis(value),
            _ => return abi::BAD_ARGS,
        };
        let Some(name) = abi::text(name, name_len).filter(|n| !n.is_empty()) else { return abi::BAD_ARGS };
        match c.backend.input(&c.id, name, input) {
            Ok(()) => abi::OK,
            Err(e) => {
                c.backend.log(&format!("[{}] input {name} failed: {e}", c.id));
                abi::FAILED
            }
        }
    }))
    .unwrap_or(abi::FAILED)
}

/// Checked here, so every host gets the same rule: the capability, one line of plain text, and
/// a first word from the feature's game_commands. No ";" or line break: one call, one command.
unsafe extern "C" fn game_command_cb(c: *mut c_void, text: *const u8, len: usize) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(c) = ctx(c) else { return abi::BAD_ARGS };
        if !c.caps.contains(&Capability::GameCommand) {
            return abi::REFUSED;
        }
        let plain = |t: &&str| !t.is_empty() && t.len() <= 256 && t.chars().all(|ch| (' '..='~').contains(&ch) && ch != ';');
        let Some(t) = abi::text(text, len).filter(plain) else { return abi::BAD_ARGS };
        if !t.split(' ').next().is_some_and(|w| c.commands.iter().any(|d| d == w)) {
            return abi::REFUSED;
        }
        match c.backend.game_command(&c.id, t) {
            Ok(()) => abi::OK,
            Err(e) => {
                c.backend.log(&format!("[{}] command {t:?} failed: {e}", c.id));
                abi::FAILED
            }
        }
    }))
    .unwrap_or(abi::FAILED)
}

/// Checked here, so every host gets the same rule: vars_read, a key of this game, the limits.
/// The backend watches a name only for its first reader; again for the same feature is OK.
unsafe extern "C" fn subscribe_cb(c: *mut c_void, key: *const u8, len: usize) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(c) = ctx(c) else { return abi::BAD_ARGS };
        if !c.caps.contains(&Capability::VarsRead) {
            return abi::REFUSED;
        }
        let Some(native) = abi::text(key, len).and_then(|k| native_of(k, &c.game)) else { return abi::BAD_ARGS };
        if memory_key(native) && (!c.caps.contains(&Capability::VarsMemory) || !owns_map(c, native) || c.stopping.load(Ordering::Acquire)) { return abi::REFUSED; }
        let first = {
            let subs = lock(&c.subs);
            let mine = subs.readers.get(&c.id);
            if mine.is_some_and(|m| m.contains(native)) {
                return abi::OK;
            }
            if mine.map_or(0, |m| m.len()) >= MAX_VARS_PER_FEATURE || subs.readers.values().map(|s| s.len()).sum::<usize>() >= MAX_VARS {
                return abi::BUSY;
            }
            !subs.used(native)
        };
        if first {
            if let Err(e) = watch(c.backend.as_ref(), native, true) {
                c.backend.log(&format!("[{}] subscribe {native:?} failed: {e}", c.id));
                return abi::FAILED;
            }
        }
        {
            let mut subs = lock(&c.subs);
            subs.readers.entry(c.id.clone()).or_default().insert(native.to_string());
            // Not from here: the feature is inside its own call; the set tells it at the next frame.
            if subs.ready.contains(native) {
                subs.add_late(&c.id, native);
            }
        }
        c.backend.log(&format!("[{}] reads {}:{native}", c.id, c.game));
        abi::OK
    }))
    .unwrap_or(abi::FAILED)
}

/// OK also when the feature did not read the name; the backend stops after the last reader.
unsafe extern "C" fn unsubscribe_cb(c: *mut c_void, key: *const u8, len: usize) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(c) = ctx(c) else { return abi::BAD_ARGS };
        let Some(native) = abi::text(key, len).and_then(|k| native_of(k, &c.game)) else { return abi::BAD_ARGS };
        let last = {
            let mut subs = lock(&c.subs);
            let removed = subs.readers.get_mut(&c.id).is_some_and(|m| m.remove(native));
            subs.late.retain(|(_, f, n)| !(f == &c.id && n == native));
            let last = removed && !subs.used(native);
            if last {
                subs.forget(native);
            }
            last
        };
        if last {
            if let Err(e) = watch(c.backend.as_ref(), native, false) {
                c.backend.log(&format!("[{}] unsubscribe {native:?}: {e}", c.id));
            }
        }
        abi::OK
    }))
    .unwrap_or(abi::FAILED)
}

/// Only owned scalar handles cross the ABI. Admission closes before feature stop; release stays
/// available for stop callbacks, and the backend restores anything the feature did not release.
unsafe extern "C" fn memory_acquire_cb(c: *mut c_void, key: *const u8, len: usize) -> i64 {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(c) = ctx(c) else { return abi::BAD_ARGS as i64 };
        if !write_allowed(c) || c.stopping.load(Ordering::Acquire) { return abi::REFUSED as i64; }
        let Some(key) = abi::text(key, len).filter(|s| memory_key(s)) else { return abi::BAD_ARGS as i64 };
        if !owns_map(c, key) { return abi::REFUSED as i64; }
        match c.backend.memory_acquire(&c.id, key) {
            Ok(h) if h > 0 && h <= i64::MAX as u64 => h as i64,
            Ok(_) => abi::FAILED as i64,
            Err(e) => e.code() as i64,
        }
    })).unwrap_or(abi::FAILED as i64)
}

fn write_allowed(c: &Ctx) -> bool { c.caps.contains(&Capability::VarsMemory) && c.caps.contains(&Capability::VarsWrite) }

fn owns_map(c: &Ctx, key: &str) -> bool {
    key.strip_prefix("mem:").and_then(|s| s.split_once('/')).is_some_and(|(map, _)| c.memory_maps.contains(map))
}

unsafe extern "C" fn memory_set_cb(c: *mut c_void, handle: u64, value: *const AbiVar) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(c) = ctx(c) else { return abi::BAD_ARGS };
        if !write_allowed(c) || c.stopping.load(Ordering::Acquire) { return abi::REFUSED; }
        let Some(v) = value.as_ref() else { return abi::BAD_ARGS };
        let value = match v.kind {
            abi::KIND_F64 if v.num[0].is_finite() => Var::F64(v.num[0]),
            abi::KIND_I64 => Var::I64(v.int),
            abi::KIND_BOOL if v.num[0] == 0.0 || v.num[0] == 1.0 => Var::Bool(v.num[0] == 1.0),
            _ => return abi::BAD_ARGS,
        };
        c.backend.memory_set(&c.id, handle, value).map_or_else(|e| e.code(), |_| abi::OK)
    })).unwrap_or(abi::FAILED)
}

unsafe extern "C" fn memory_release_cb(c: *mut c_void, handle: u64) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(c) = ctx(c) else { return abi::BAD_ARGS };
        if !write_allowed(c) { return abi::REFUSED; }
        c.backend.memory_release(&c.id, handle).map_or_else(|e| e.code(), |_| abi::OK)
    })).unwrap_or(abi::FAILED)
}

/// An effect that reads the game's depth needs render_depth as well (checked for every host).
fn depth_allowed(caps: &[Capability], p: RenderParams) -> bool {
    !matches!(p.effect, crate::RenderEffect::Mirage(_)) || caps.contains(&Capability::RenderDepth)
}

/// The color pass: capability render_pass, parameters checked here for every host. Acquire and
/// set close at stop; release stays available to the feature's stop, and cleanup removes the rest.
unsafe extern "C" fn render_acquire_cb(c: *mut c_void, params: *const abi::AbiRender) -> i64 {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(c) = ctx(c) else { return abi::BAD_ARGS as i64 };
        if !c.caps.contains(&Capability::RenderPass) || c.stopping.load(Ordering::Acquire) { return abi::REFUSED as i64; }
        let Some(p) = params.as_ref().and_then(RenderParams::from_abi) else { return abi::BAD_ARGS as i64 };
        if !depth_allowed(&c.caps, p) { return abi::REFUSED as i64; }
        match c.backend.render_acquire(&c.id, p) {
            Ok(h) if h > 0 && h <= i64::MAX as u64 => h as i64,
            Ok(_) => abi::FAILED as i64,
            Err(e) => e.code() as i64,
        }
    })).unwrap_or(abi::FAILED as i64)
}

unsafe extern "C" fn render_set_cb(c: *mut c_void, handle: u64, params: *const abi::AbiRender) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(c) = ctx(c) else { return abi::BAD_ARGS };
        if !c.caps.contains(&Capability::RenderPass) || c.stopping.load(Ordering::Acquire) { return abi::REFUSED; }
        let Some(p) = params.as_ref().and_then(RenderParams::from_abi) else { return abi::BAD_ARGS };
        if !depth_allowed(&c.caps, p) { return abi::REFUSED; }
        c.backend.render_set(&c.id, handle, p).map_or_else(|e| e.code(), |_| abi::OK)
    })).unwrap_or(abi::FAILED)
}

unsafe extern "C" fn render_release_cb(c: *mut c_void, handle: u64) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(c) = ctx(c) else { return abi::BAD_ARGS };
        if !c.caps.contains(&Capability::RenderPass) { return abi::REFUSED; }
        let r = c.backend.render_release(&c.id, handle).map_or_else(|e| e.code(), |_| abi::OK);
        if r == abi::OK { lock(&c.held).0.retain(|h| h.handle != handle); }
        r
    })).unwrap_or(abi::FAILED)
}

/// The checked inputs of a shader call: render_shader (and render_depth for the depth stage),
/// finite parameters, and the named shader read from the package (`name` empty: None, allowed
/// for set only). A bad name, a missing or too large file is BAD_ARGS, with the reason logged.
fn shader_input(c: &Ctx, p: &abi::AbiShader, stage: Option<ShaderStage>, need_name: bool) -> Result<Option<(ShaderCode, Stamp)>, i32> {
    if !c.caps.contains(&Capability::RenderShader) || c.stopping.load(Ordering::Acquire) { return Err(abi::REFUSED); }
    if stage == Some(ShaderStage::SceneDepth) && !c.caps.contains(&Capability::RenderDepth) { return Err(abi::REFUSED); }
    if !p.params.iter().all(|v| v.is_finite()) { return Err(abi::BAD_ARGS); }
    let name = if p.name_len == 0 { "" } else { unsafe { abi::text(p.name, p.name_len) }.ok_or(abi::BAD_ARGS)? };
    if name.is_empty() { return if need_name { Err(abi::BAD_ARGS) } else { Ok(None) }; }
    if !crate::shader_name_valid(name) { return Err(abi::BAD_ARGS); }
    match read_shader(c, name) {
        Ok(read) => Ok(Some(read)),
        Err(e) => { c.backend.log(&format!("[{}] shader {name}: {e}", c.id)); Err(abi::BAD_ARGS) }
    }
}

/// The package file shaders/<name>.cso (at most SHADER_SIZE_MAX bytes) and its stamp.
fn shader_path(c: &Ctx, name: &str) -> Result<std::path::PathBuf, String> {
    Ok(c.shaders.as_ref().ok_or("the feature has no package folder")?.join(format!("{name}.cso")))
}
fn stamp(path: &std::path::Path) -> Result<Stamp, String> {
    let m = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((m.modified().ok(), m.len()))
}
fn read_shader(c: &Ctx, name: &str) -> Result<(ShaderCode, Stamp), String> {
    let path = shader_path(c, name)?;
    let s = stamp(&path)?;
    if s.1 > abi::SHADER_SIZE_MAX as u64 { return Err(format!("{} has {} bytes, more than {}", path.display(), s.1, abi::SHADER_SIZE_MAX)); }
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((ShaderCode { name: name.into(), bytes: bytes.into() }, s))
}

/// Hot reload (shader pipeline plan, slice 7): once per RELOAD_EVERY, each held pass whose file
/// changed (a new compile into the version folder, fm_pack shaders) gets the new shader with
/// its last parameters through Backend::shader_set. A file that does not load or that the host
/// refuses keeps the old shader; the new stamp is kept, so the log says it once per change.
fn reload_shaders(c: &Ctx) {
    if c.stopping.load(Ordering::Acquire) { return; }
    let mut guard = lock(&c.held);
    let (held, last) = &mut *guard;
    if held.is_empty() || last.is_some_and(|t| t.elapsed() < RELOAD_EVERY) { return; }
    *last = Some(Instant::now());
    for h in held.iter_mut() {
        let Ok(now) = shader_path(c, &h.name).and_then(|p| stamp(&p)) else { continue };
        if now == h.stamp { continue; }
        h.stamp = now;
        let result = read_shader(c, &h.name).and_then(|(code, s)| {
            h.stamp = s;
            c.backend.shader_set(&c.id, h.handle, Some(&code), h.params).map_err(|e| format!("the host refused it ({e:?})"))
        });
        match result {
            Ok(()) => c.backend.log(&format!("[{}] shader {} reloaded (pass {})", c.id, h.name, h.handle)),
            Err(e) => c.backend.log(&format!("[{}] shader {} not reloaded: {e}; the pass keeps the old shader", c.id, h.name)),
        }
    }
}

unsafe extern "C" fn shader_acquire_cb(c: *mut c_void, params: *const abi::AbiShader) -> i64 {
    catch_unwind(AssertUnwindSafe(|| {
        let (Some(c), Some(p)) = (ctx(c), params.as_ref()) else { return abi::BAD_ARGS as i64 };
        let Some(stage) = ShaderStage::from_abi(p.stage) else {
            return if c.caps.contains(&Capability::RenderShader) { abi::BAD_ARGS as i64 } else { abi::REFUSED as i64 };
        };
        let (code, stamp) = match shader_input(c, p, Some(stage), true) { Ok(Some(read)) => read, Ok(None) => return abi::BAD_ARGS as i64, Err(e) => return e as i64 };
        match c.backend.shader_acquire(&c.id, stage, &code, p.params) {
            Ok(h) if h > 0 && h <= i64::MAX as u64 => {
                lock(&c.held).0.push(Held { handle: h, name: code.name, stamp, params: p.params });
                h as i64
            }
            Ok(_) => abi::FAILED as i64,
            Err(e) => e.code() as i64,
        }
    })).unwrap_or(abi::FAILED as i64)
}

/// The host keeps the stage of the pass and checks that a new shader fits it (depth or not).
unsafe extern "C" fn shader_set_cb(c: *mut c_void, handle: u64, params: *const abi::AbiShader) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let (Some(c), Some(p)) = (ctx(c), params.as_ref()) else { return abi::BAD_ARGS };
        let read = match shader_input(c, p, None, false) { Ok(read) => read, Err(e) => return e };
        if let Err(e) = c.backend.shader_set(&c.id, handle, read.as_ref().map(|(code, _)| code), p.params) { return e.code(); }
        if let Some(h) = lock(&c.held).0.iter_mut().find(|h| h.handle == handle) {
            h.params = p.params;
            if let Some((code, stamp)) = read { h.name = code.name; h.stamp = stamp; }
        }
        abi::OK
    })).unwrap_or(abi::FAILED)
}

/// Checked here, so every host gets the same rule: input_inject and a printable ASCII name with
/// no space (sim event names). The game decides if the name exists.
unsafe extern "C" fn send_event_cb(c: *mut c_void, name: *const u8, len: usize, data: i32) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(c) = ctx(c) else { return abi::BAD_ARGS };
        if !c.caps.contains(&Capability::InputInject) {
            return abi::REFUSED;
        }
        let ok = |n: &&str| !n.is_empty() && n.len() <= 128 && n.bytes().all(|b| (b'!'..=b'~').contains(&b));
        let Some(name) = abi::text(name, len).filter(ok) else { return abi::BAD_ARGS };
        match c.backend.send_event(&c.id, name, data) {
            Ok(()) => abi::OK,
            Err(e) => {
                c.backend.log(&format!("[{}] event {name} {data} failed: {e}", c.id));
                abi::FAILED
            }
        }
    }))
    .unwrap_or(abi::FAILED)
}

/// The host rule for a request URL: https, a host from network_hosts (ASCII case does not
/// count), no user name, password or port, and printable ASCII with no backslash, so each URL
/// parser finds the same host. Policy refusals are REFUSED, broken text is BAD_ARGS. The app
/// checks the URL again against the installed manifest (it is the authority).
fn check_url(url: &str, hosts: &[String]) -> Result<(), i32> {
    if url.is_empty() || url.len() > MAX_URL || !url.bytes().all(|b| (0x21..=0x7e).contains(&b) && b != b'\\') {
        return Err(abi::BAD_ARGS);
    }
    if !url.get(..8).is_some_and(|s| s.eq_ignore_ascii_case("https://")) {
        return Err(abi::REFUSED);
    }
    let rest = &url[8..];
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    if authority.is_empty() {
        return Err(abi::BAD_ARGS);
    }
    // "@": user info; ":": port; "[" or "]": IPv6 literal; "%": encoded host.
    if authority.contains(['@', ':', '[', ']', '%']) {
        return Err(abi::REFUSED);
    }
    let host = authority.to_ascii_lowercase();
    if hosts.contains(&host) {
        Ok(())
    } else {
        Err(abi::REFUSED)
    }
}

/// The capability, the URL rule and MAX_WAITING are checked here, so every host gets the same
/// rule. The request waits in Net before the backend sends it, so an answer always finds it.
unsafe extern "C" fn http_get_cb(c: *mut c_void, url: *const u8, len: usize) -> i64 {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(c) = ctx(c) else { return abi::BAD_ARGS as i64 };
        if !c.caps.contains(&Capability::Network) {
            return abi::REFUSED as i64;
        }
        let Some(url) = abi::text(url, len) else { return abi::BAD_ARGS as i64 };
        if let Err(code) = check_url(url, &c.hosts) {
            return code as i64;
        }
        let id = {
            let mut net = lock(&c.net);
            if net.waiting.iter().filter(|w| w.feature == c.id).count() >= MAX_WAITING {
                return abi::BUSY as i64;
            }
            net.last_id += 1;
            let id = net.last_id;
            net.waiting.push(Waiting { feature: c.id.clone(), id, since: Instant::now() });
            id
        };
        match c.backend.http_get(&c.id, id, url) {
            Ok(()) => id as i64,
            Err(e) => {
                lock(&c.net).waiting.retain(|w| w.id != id);
                e.code() as i64
            }
        }
    }))
    .unwrap_or(abi::FAILED as i64)
}

unsafe extern "C" fn log_cb(c: *mut c_void, text: *const u8, len: usize) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let (Some(c), Some(t)) = (ctx(c), abi::text(text, len)) {
            c.backend.log(&format!("[{}] {}", c.id, &t[..t.len().min(1000)]));
        }
    }));
}

/// One user files call: the capability, then files::Service (the list, this game only, the
/// backend's user folder). A refusal or a failure goes to the log with its reason.
fn user_files<T>(c: &Ctx, key: *const u8, len: usize, f: impl FnOnce(&crate::files::Service, &str) -> Result<T, (i32, String)>) -> Result<T, i32> {
    if !c.caps.contains(&Capability::UserFiles) {
        return Err(abi::REFUSED);
    }
    let key = unsafe { abi::text(key, len) }.ok_or(abi::BAD_ARGS)?;
    let root = |game: &str| c.backend.user_root(game);
    let service = crate::files::Service { list: &c.user_files, scope: Some(&c.game), root: &root };
    f(&service, key).map_err(|(code, why)| {
        c.backend.log(&format!("[{}] user file: {why}", c.id));
        code
    })
}

unsafe extern "C" fn file_info_cb(c: *mut c_void, key: *const u8, len: usize, out: *mut abi::AbiFileInfo) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let (Some(c), Some(out)) = (ctx(c), out.as_mut()) else { return abi::BAD_ARGS };
        match user_files(c, key, len, |s, k| s.info(k)) {
            Ok(i) => {
                *out = i;
                abi::OK
            }
            Err(code) => code,
        }
    }))
    .unwrap_or(abi::FAILED)
}

unsafe extern "C" fn file_read_cb(c: *mut c_void, key: *const u8, len: usize, offset: u64, buf: *mut u8, cap: usize) -> i64 {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(c) = ctx(c) else { return abi::BAD_ARGS as i64 };
        if buf.is_null() && cap > 0 {
            return abi::BAD_ARGS as i64;
        }
        let buf: &mut [u8] = if cap == 0 { &mut [] } else { std::slice::from_raw_parts_mut(buf, cap) };
        user_files(c, key, len, |s, k| s.read(k, offset, buf)).map_or_else(|code| code as i64, |n| n as i64)
    }))
    .unwrap_or(abi::FAILED as i64)
}

unsafe extern "C" fn file_write_cb(c: *mut c_void, key: *const u8, len: usize, bytes: *const u8, bytes_len: usize) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(c) = ctx(c) else { return abi::BAD_ARGS };
        if bytes.is_null() && bytes_len > 0 {
            return abi::BAD_ARGS;
        }
        let bytes: &[u8] = if bytes_len == 0 { &[] } else { std::slice::from_raw_parts(bytes, bytes_len) };
        user_files(c, key, len, |s, k| s.write(k, bytes)).map_or_else(|code| code, |()| abi::OK)
    }))
    .unwrap_or(abi::FAILED)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{Feature, Frame, Host};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Rec {
        ui: Mutex<Vec<(String, String, String)>>,
        inputs: Mutex<Vec<(String, Input)>>,
        logs: Mutex<Vec<String>>,
        stopped: Mutex<Vec<String>>,
        commands: Mutex<Vec<(String, String)>>,
        http: Mutex<Vec<(String, u64, String)>>,
        watch: Mutex<Vec<(String, bool)>>,
        sim_events: Mutex<Vec<(String, String, i32)>>,
        cleanups: Mutex<Vec<String>>,
        /// The ids whose cleanup fails (a host that cannot restore a held value).
        bad_cleanup: Mutex<Vec<String>>,
        /// Some: admit gives Wait with this reason (no app link, a resource that waits).
        wait: Mutex<Option<String>>,
        /// True: cleanup only starts the work (Completion::Pending, a game-thread restore).
        pending: Mutex<bool>,
        /// Each admission: (id, activation).
        admits: Mutex<Vec<(String, u64)>>,
        /// The user folder of every game; None: the backend refuses it (a -homedir game).
        root: Mutex<Option<std::path::PathBuf>>,
    }

    struct B(Arc<Rec>);
    impl Backend for B {
        fn ui(&self, f: &str, t: &str, j: &str) {
            self.0.ui.lock().unwrap().push((f.into(), t.into(), j.into()));
        }
        fn input(&self, _f: &str, n: &str, i: Input) -> Result<(), String> {
            self.0.inputs.lock().unwrap().push((n.into(), i));
            Ok(())
        }
        fn log(&self, l: &str) {
            self.0.logs.lock().unwrap().push(l.into());
        }
        fn stopped(&self, f: &str) {
            self.0.stopped.lock().unwrap().push(f.into());
        }
        // In the command list, so a test sees what came before and after the drop.
        fn stopping(&self, f: &str) {
            self.0.commands.lock().unwrap().push((f.into(), "<stopping>".into()));
        }
        fn game_command(&self, f: &str, t: &str) -> Result<(), String> {
            self.0.commands.lock().unwrap().push((f.into(), t.into()));
            Ok(())
        }
        fn http_get(&self, f: &str, id: u64, url: &str) -> Result<(), HostError> {
            self.0.http.lock().unwrap().push((f.into(), id, url.into()));
            Ok(())
        }
        fn watch(&self, n: &str, on: bool) -> Result<(), String> {
            self.0.watch.lock().unwrap().push((n.into(), on));
            Ok(())
        }
        fn send_event(&self, f: &str, n: &str, d: i32) -> Result<(), String> {
            self.0.sim_events.lock().unwrap().push((f.into(), n.into(), d));
            Ok(())
        }
        fn cleanup(&self, f: &str, _: u64) -> Result<(), String> {
            self.0.cleanups.lock().unwrap().push(f.into());
            if self.0.bad_cleanup.lock().unwrap().iter().any(|b| b == f) { return Err("cannot restore".into()); }
            Ok(())
        }
        // The rules of prepare stay; a set reason gives Wait first.
        fn admit(&self, f: &ActiveFeature, activation: u64) -> Admit {
            self.0.admits.lock().unwrap().push((f.id.clone(), activation));
            if let Some(why) = self.0.wait.lock().unwrap().clone() { return Admit::Wait(why); }
            match self.prepare(f) { Ok(()) => Admit::Ok, Err(e) => Admit::Refuse(e) }
        }
        fn completion(&self) -> Completion {
            if *self.0.pending.lock().unwrap() { Completion::Pending } else { Completion::Sync }
        }
        fn user_root(&self, _game: &str) -> Result<std::path::PathBuf, String> {
            self.0.root.lock().unwrap().clone().ok_or_else(|| "the game uses -homedir".into())
        }
        fn game_started(&self) -> Option<std::time::SystemTime> {
            Some(std::time::UNIX_EPOCH + Duration::from_secs(1_000))
        }
    }

    /// Test feature: sends the speed to its page, horn above 10 m/s, panics on "boom". Events call
    /// one host function and log the result: "cmd:<text>" ("." for " "), "get:<url>", "dir",
    /// "stopcmd:<text>" (sent at stop), "sub:", "unsub:", "subn:<n>", "send:<name>=<data>",
    /// "onready:<key>|<event>", "uf:<key>" (info and read), "ufw:<key>=<text>", "since". Frames
    /// log the subscribed values, events their attributes. Its lifecycle hook is in hook.rs.
    pub(crate) struct Probe {
        host: Host,
        on_stop: Option<String>,
        subs: Vec<String>,
        on_ready: Vec<(String, String)>,
    }
    impl Feature for Probe {
        fn start(host: Host) -> Self {
            host.log(&format!("start on {}", host.game()));
            let s = host.settings();
            if s.revision() > 0 { host.log(&format!("start settings {} {:?}", s.revision(), s.f64("strength"))); }
            Probe { host, on_stop: None, subs: vec![], on_ready: vec![] }
        }
        fn stop(&mut self) {
            if let Some(t) = self.on_stop.take() {
                let r = self.host.command(&t);
                self.host.log(&format!("stop: {r:?}"));
            }
        }
        fn frame(&mut self, f: &Frame) {
            let speed = f.f64(crate::names::SPEED_MPS);
            let _ = self.host.ui("state", &serde_json::json!({ "speed": speed }));
            if speed.unwrap_or(0.0) > 10.0 {
                if let Err(e) = self.host.press("horn", 150) {
                    self.host.log(&format!("press: {e:?}"));
                }
            }
            for k in &self.subs {
                if let Some(v) = f.var(k) {
                    self.host.log(&format!("var {k}={v:?}"));
                }
            }
        }
        fn game_event(&mut self, e: &crate::GameEvent) {
            if !e.attrs.is_empty() {
                self.host.log(&format!("event {} {:?}", e.id, e.attrs));
            }
            if e.id == "var.ready" {
                let key = e.attrs.iter().find_map(|(n, v)| match v { crate::Var::Text(k) if *n == "name" => Some(k.clone()), _ => None });
                let run: Vec<String> = self.on_ready.iter().filter(|(k, _)| Some(k) == key.as_ref()).map(|(_, a)| a.clone()).collect();
                run.iter().for_each(|a| self.event(a));
            }
            self.event(e.id)
        }
        fn event(&mut self, id: &str) {
            if id == "boom" {
                panic!("feature bug");
            }
            if let Some(t) = id.strip_prefix("stopcmd:") {
                self.on_stop = Some(t.replace('.', " "));
            }
            if let Some((k, a)) = id.strip_prefix("onready:").and_then(|t| t.split_once('|')) {
                self.on_ready.push((k.to_string(), a.to_string()));
            }
            if let Some(t) = id.strip_prefix("cmd:") {
                let r = self.host.command(&t.replace('.', " "));
                self.host.log(&format!("{id}: {r:?}"));
            }
            if let Some(url) = id.strip_prefix("get:") {
                let r = self.host.http_get(url);
                self.host.log(&format!("{id}: {r:?}"));
            }
            if let Some(k) = id.strip_prefix("sub:") {
                let r = self.host.subscribe(k);
                self.subs.push(k.to_string());
                self.host.log(&format!("{id}: {r:?}"));
            }
            if let Some(k) = id.strip_prefix("unsub:") {
                let r = self.host.unsubscribe(k);
                self.host.log(&format!("{id}: {r:?}"));
            }
            if let Some(n) = id.strip_prefix("subn:") {
                let r: Result<Vec<()>, _> = (0..n.parse::<usize>().unwrap()).map(|i| self.host.subscribe(&format!("ats:v{i}"))).collect();
                self.host.log(&format!("{id}: {:?}", r.map(|v| v.len())));
            }
            if let Some((n, d)) = id.strip_prefix("send:").and_then(|t| t.split_once('=')) {
                let r = self.host.send_event(n, d.parse().unwrap());
                self.host.log(&format!("{id}: {r:?}"));
            }
            if id == "dir" {
                self.host.log(&format!("dir: {:?}", self.host.data_dir()));
            }
            if let Some(k) = id.strip_prefix("uf:") {
                use crate::UserFiles;
                let i = self.host.file_info(k).map(|i| (i.kind, i.size));
                let r = self.host.file_read(k, 0, 64).map(|b| String::from_utf8_lossy(&b).into_owned());
                self.host.log(&format!("{id}: {i:?} {r:?}"));
            }
            if let Some((k, t)) = id.strip_prefix("ufw:").and_then(|t| t.split_once('=')) {
                use crate::UserFiles;
                let r = self.host.file_write(k, t.as_bytes());
                self.host.log(&format!("{id}: {r:?}"));
            }
            if id == "since" {
                self.host.log(&format!("since: {:?}", self.host.game_started()));
            }
        }
        fn http(&mut self, id: u64, answer: Result<crate::HttpAnswer, crate::HostError>) {
            let shown = match &answer {
                Ok(a) => format!("{} {}", a.status, String::from_utf8_lossy(&a.body)),
                Err(e) => format!("{e:?}"),
            };
            self.host.log(&format!("http {id}: {shown}"));
            if answer.is_ok_and(|a| a.body == b"boom") {
                panic!("feature bug in http");
            }
        }
        /// Logs each settings change; a "strength" of 13 panics.
        fn settings(&mut self, s: &crate::Settings) {
            self.host.log(&format!("settings {} {:?}", s.revision(), s.f64("strength")));
            if s.f64("strength") == Some(13.0) {
                panic!("feature bug in settings");
            }
        }
        /// Logs each page message; topic "boom" panics.
        fn page(&mut self, topic: &str, data: &str) {
            self.host.log(&format!("page {topic} {data}"));
            if topic == "boom" {
                panic!("feature bug in page");
            }
        }
        /// Logs the control; the release of "boom" panics (a feature bug at its stop release).
        fn control(&mut self, id: &str, value: crate::Control) {
            self.host.log(&format!("control {id} {value:?}"));
            if id == "boom" && value == crate::Control::Button(false) { panic!("feature bug in control"); }
        }
        /// Logs the event and its folder; Updated from "fail" fails, from "boom" panics (hook.rs).
        /// GameExited of "files" tries the game check and the user files (hook.rs test).
        fn lifecycle(event: crate::Lifecycle, host: &crate::LifecycleHost) -> Result<(), String> {
            if event == (crate::Lifecycle::GameExited { game: "files" }) {
                use crate::UserFiles;
                host.log(&format!("running ats: {:?}, msfs2024: {:?}", host.game_running("ats"), host.game_running("msfs2024")));
                let cfg = "ats:profiles/41/config.cfg";
                host.log(&format!("read: {:?}", host.file_read(cfg, 0, 100).map(|b| String::from_utf8_lossy(&b).into_owned())));
                host.log(&format!("write: {:?}, other: {:?}", host.file_write(cfg, b"uset a \"0.3\"\n"), host.file_info("ats:game.log.txt")));
                let on_way = host.file_info("ats:profiles/notes.txt").map(|i| (i.kind, i.size, i.modified));
                host.log(&format!("on the way: {on_way:?}"));
                return Ok(());
            }
            host.log(&format!("{event:?} in {}", host.data_dir().display()));
            match event {
                crate::Lifecycle::Updated { from_version: "fail" } => Err("cannot repair".into()),
                crate::Lifecycle::Updated { from_version: "boom" } => panic!("hook bug"),
                _ => Ok(()),
            }
        }
    }
    crate::feature!(Probe);

    fn meta(id: &str, caps: &[Capability], mp: Multiplayer) -> ActiveFeature {
        ActiveFeature { id: id.into(), version: "1.0.0".into(), library: "x".into(), games: vec!["ats".into()], capabilities: caps.to_vec(), multiplayer: mp, game_commands: vec![], network_hosts: vec![], memory_maps: vec![], data_dir: None, user_files: vec![], requires: vec![], settings: None }
    }

    /// A user file needs the capability and its declaration, in this game only; a refusal and a
    /// failure are logged with the reason; a backend that refuses the folder (-homedir) fails
    /// every call. The game start reaches the feature.
    #[test]
    fn user_files_need_the_capability_and_the_declaration() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("profiles/41")).unwrap();
        std::fs::write(t.path().join("game.log.txt"), "log line").unwrap();
        std::fs::write(t.path().join("profiles/notes.txt"), "private").unwrap();
        let rec = Arc::new(Rec::default());
        *rec.root.lock().unwrap() = Some(t.path().to_path_buf());
        let mut s = set(&rec);
        let list = vec![
            crate::UserFile { game: "ats".into(), path: "game.log.txt".into(), access: crate::Access::Read },
            crate::UserFile { game: "ats".into(), path: "profiles/*/config.cfg".into(), access: crate::Access::Write },
        ];
        s.add(ActiveFeature { user_files: list.clone(), ..meta("weather", &[Capability::UserFiles], Multiplayer::Allowed) }, Ok(entry()));
        s.add(ActiveFeature { user_files: list, ..meta("no-cap", &[], Multiplayer::Allowed) }, Ok(entry()));
        for ev in ["uf:ats:game.log.txt", "ufw:ats:profiles/41/config.cfg=uset x", "ufw:ats:game.log.txt=x", "uf:ats:profiles/41/profile.sii", "uf:msfs2024:game.log.txt", "uf:ats:profiles/notes.txt", "since"] {
            s.event(ev);
        }
        assert!(logged(&rec, "[weather] uf:ats:profiles/notes.txt: Ok((None, 0)) Err(Refused)"), "review r2 B2: a file on the way shows nothing");
        assert!(logged(&rec, r#"[weather] uf:ats:game.log.txt: Ok((File, 8)) Ok("log line")"#), "{:?}", logs(&rec));
        assert!(logged(&rec, "[weather] ufw:ats:profiles/41/config.cfg=uset x: Ok(())"));
        assert_eq!(std::fs::read_to_string(t.path().join("profiles/41/config.cfg")).unwrap(), "uset x");
        assert!(logged(&rec, "[weather] ufw:ats:game.log.txt=x: Err(Refused)"), "a read entry");
        assert!(logged(&rec, "[weather] uf:ats:profiles/41/profile.sii: Err(Refused) Err(Refused)"));
        assert!(logged(&rec, "[weather] uf:msfs2024:game.log.txt: Err(Refused) Err(Refused)"), "another game");
        assert!(logs(&rec).iter().any(|l| l.starts_with("[weather] user file: \"ats:profiles/41/profile.sii\" is not declared")), "the reason is logged");
        assert!(logged(&rec, "[no-cap] uf:ats:game.log.txt: Err(Refused) Err(Refused)"));
        assert!(logged(&rec, &format!("[weather] since: {:?}", Some(std::time::UNIX_EPOCH + Duration::from_secs(1_000)))));
        assert_eq!(std::fs::read_to_string(t.path().join("game.log.txt")).unwrap(), "log line");
        *rec.root.lock().unwrap() = None;
        s.event("uf:ats:game.log.txt");
        assert!(logged(&rec, "[weather] uf:ats:game.log.txt: Err(Failed) Err(Failed)"));
        assert!(logs(&rec).iter().any(|l| l.contains("the game uses -homedir")));
    }

    /// -homedir: another user folder gives a user files feature its own data folder and goes in
    /// the report (the app keeps it for the exit hook). The default folder and a refused one keep
    /// the base folder and report nothing, as before; a feature with no user files never moves.
    #[test]
    fn another_user_folder_moves_the_data_folder_and_goes_in_the_report() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().join("features/weather/data");
        let home = t.path().join("ATSData/American Truck Simulator");
        let run = |root: Option<std::path::PathBuf>| {
            let rec = Arc::new(Rec::default());
            *rec.root.lock().unwrap() = root;
            let mut s = set(&rec);
            for (id, caps) in [("weather", vec![Capability::UserFiles]), ("plain", vec![])] {
                s.add(ActiveFeature { data_dir: Some(base.clone()), ..meta(id, &caps, Multiplayer::Allowed) }, Ok(entry()));
            }
            s.event("dir");
            (logs(&rec), s.report().user_root)
        };
        let dir = |id: &str, p: &std::path::Path| format!("[{id}] dir: {:?}", Some(p));
        let (logs, report) = run(Some(home.clone()));
        let own = base.join(crate::files::USER_FOLDERS).join(crate::files::root_id("ats", &home));
        assert!(logs.contains(&dir("weather", &own)), "{logs:?}");
        assert!(logs.contains(&dir("plain", &base)), "no user files: the base folder");
        assert_eq!(report.as_deref(), home.to_str());
        // Review r2: an alias of the default folder (other separators, an end separator, a lower
        // case drive letter) is the default folder too: no report, the base folder.
        let default = crate::files::default_user_root("ats").unwrap();
        let mut alias = default.to_string_lossy().replace('\\', "/") + "/";
        alias.get_mut(0..1).unwrap().make_ascii_lowercase();
        for root in [Some(default), Some(alias.into()), None] {
            let (logs, report) = run(root);
            assert!(logs.contains(&dir("weather", &base)) && report.is_none(), "{logs:?}");
        }
        // Review r2: Documents through a junction. The plugin gives the canonical folder (the
        // target), the default text names the junction: still the default folder.
        #[cfg(windows)]
        {
            let (real, link) = (t.path().join("real"), t.path().join("link"));
            std::fs::create_dir_all(real.join("American Truck Simulator/profiles")).unwrap();
            let made = std::process::Command::new("cmd").args(["/d", "/c", "mklink", "/J"]).arg(&link).arg(&real).output().unwrap();
            assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
            crate::files::TEST_DOCUMENTS.with(|d| *d.borrow_mut() = Some(link));
            let target = crate::files::canonical_root(&real.join("American Truck Simulator")).unwrap();
            assert_ne!(Some(&target), crate::files::default_user_root("ats").as_ref(), "the raw texts differ");
            let (logs, report) = run(Some(target));
            crate::files::TEST_DOCUMENTS.with(|d| *d.borrow_mut() = None);
            assert!(logs.contains(&dir("weather", &base)) && report.is_none(), "{logs:?} {report:?}");
        }
    }

    fn entry() -> Entry {
        Entry { start: fm_feature_start, api: abi::API_VERSION }
    }

    fn set(rec: &Arc<Rec>) -> FeatureSet {
        FeatureSet::new("ats", Arc::new(B(rec.clone())), false)
    }

    #[test]
    fn a_feature_gets_frames_and_only_its_declared_calls() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.add(meta("full", &[Capability::VarsRead, Capability::UiApp, Capability::InputInject], Multiplayer::Allowed), Ok(entry()));
        s.add(meta("read-only", &[Capability::UiApp], Multiplayer::Allowed), Ok(entry()));
        s.frame::<&str>(1, 10, &[(crate::names::SPEED_MPS, Value::F64(12.0))], &[]);
        let ui = rec.ui.lock().unwrap().clone();
        assert_eq!(ui[0], ("full".into(), "state".into(), r#"{"speed":12.0}"#.into()));
        assert_eq!(ui[1], ("read-only".into(), "state".into(), r#"{"speed":null}"#.into()), "no vars_read: no values");
        assert_eq!(rec.inputs.lock().unwrap().as_slice(), [("horn".to_string(), Input::Press { ms: 150 })], "only the feature with input_inject");
        assert!(rec.logs.lock().unwrap().iter().any(|l| l == "[full] start on ats"));
    }

    // A page message reaches its own feature only, with ui_app, as Feature::page (not as a game
    // event). A game event with the page prefix is dropped, and a panic in page stops the feature.
    #[test]
    fn page_messages_reach_their_own_feature_only() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.add(meta("paged", &[Capability::UiApp], Multiplayer::Allowed), Ok(entry()));
        s.add(meta("other", &[Capability::UiApp], Multiplayer::Allowed), Ok(entry()));
        s.add(meta("no-page", &[Capability::VarsRead], Multiplayer::Allowed), Ok(entry()));
        let logged = |l: &str| rec.logs.lock().unwrap().iter().any(|x| x == l);
        assert_eq!(s.page("paged", "look", r#"{"look":"strong"}"#), Ok(()));
        assert!(logged(r#"[paged] page look {"look":"strong"}"#));
        assert!(!rec.logs.lock().unwrap().iter().any(|l| l.starts_with("[other] page")), "another feature does not see it");
        assert!(s.page("no-page", "look", "1").unwrap_err().contains("ui_app"));
        assert!(s.page("absent", "look", "1").unwrap_err().contains("does not run"));
        for (topic, json) in [("", "1"), ("Look", "1"), ("a b", "1"), ("look", "not json")] {
            assert!(s.page("paged", topic, json).is_err(), "{topic:?} {json:?}");
        }
        assert!(s.page("paged", "look", &format!("\"{}\"", "x".repeat(PAGE_LIMIT))).is_err(), "too large");
        s.event("ui:look");
        assert!(!logged("[paged] page look null") && !logged("[other] page look null"), "a game event cannot pose as the page");
        assert_eq!(s.page("paged", "boom", "{}"), Ok(()));
        assert_eq!(s.statuses()[0].state, State::Failed);
        assert!(s.page("paged", "look", "{}").is_err(), "a failed feature gets no more page messages");
    }

    // Surfaces plan, section 4: a message with a target goes only to that start of the feature
    // (this host instance and that activation); a restart or another host refuses it.
    #[test]
    fn a_page_target_names_one_start() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.add(meta("paged", &[Capability::UiApp], Multiplayer::Allowed), Ok(entry()));
        let activation = s.statuses()[0].activation.unwrap();
        let msg = |instance: u64, activation: u64| AppMessage::Page { topic: "look".into(), data: "{}".into(), target: Some(PageTarget { instance, activation }) };
        assert!(s.app_message("paged", &msg(s.instance(), activation)).is_ok());
        assert_eq!(s.app_message("paged", &msg(s.instance(), activation + 1)), Err("this start of paged ended".into()));
        assert_eq!(s.app_message("paged", &msg(s.instance() ^ 1, activation)), Err("this start of paged ended".into()));
        assert!(s.app_message("absent", &msg(s.instance(), activation)).is_err());
        assert!(s.app_message("paged", &AppMessage::Page { topic: "look".into(), data: "{}".into(), target: None }).is_ok(), "a message with no target goes to any start");
    }

    // Feature surfaces plan, section 3: a version 6 feature starts with the projection of its
    // version from the settings file (a missing file: revision 0), gets a newer revision once
    // as Feature::settings, and reports the revision that it runs. A resend of the same revision,
    // another version, bad values, an API 5 feature and a game event with the prefix get nothing.
    #[test]
    fn settings_reach_a_version_6_feature_at_start_and_on_change() {
        let t = tempfile::tempdir().unwrap();
        let file = crate::settings::path(t.path(), "rain");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, r#"{"revision":3,"values":{"strength":0.7},"versions":{"1.0.0":{"strength":0.7}}}"#).unwrap();
        unsafe extern "C" fn frame(_: *mut c_void, _: *const AbiFrame) -> i32 { abi::OK }
        unsafe extern "C" fn event(_: *mut c_void, _: *const u8, _: usize) -> i32 { abi::OK }
        unsafe extern "C" fn http(_: *mut c_void, _: u64, _: i32, _: *const u8, _: usize) -> i32 { abi::OK }
        unsafe extern "C" fn game_event(_: *mut c_void, _: *const AbiEvent) -> i32 { abi::OK }
        unsafe extern "C" fn stop(_: *mut c_void) {}
        unsafe extern "C" fn start_v5(host: *const HostApi, out: *mut FeatureApi) -> i32 {
            if (*host).api_version != 5 { return abi::BAD_ARGS; }
            *out = FeatureApi { api_version: 5, instance: std::ptr::dangling_mut::<c_void>(), on_frame: Some(frame), on_event: Some(event), stop: Some(stop), on_http: Some(http), on_game_event: Some(game_event) };
            abi::OK
        }
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.add(ActiveFeature { settings: Some(file.clone()), ..meta("rain", &[], Multiplayer::Allowed) }, Ok(entry()));
        s.add(ActiveFeature { settings: Some(t.path().join("none/settings.json")), ..meta("fresh", &[], Multiplayer::Allowed) }, Ok(entry()));
        s.add(ActiveFeature { settings: Some(file.clone()), ..meta("old", &[], Multiplayer::Allowed) }, Ok(Entry { start: start_v5, api: 5 }));
        let revisions: Vec<_> = s.statuses().into_iter().map(|f| (f.id, f.settings_revision)).collect();
        assert_eq!(revisions, [("rain".into(), Some(3)), ("fresh".into(), Some(0)), ("old".into(), None)]);
        assert!(logged(&rec, "[rain] start settings 3 Some(0.7)"));
        assert_eq!(s.settings("rain", "1.0.0", 4, r#"{"strength":0.9}"#), Ok("settings rain 4 delivered".into()));
        assert!(logged(&rec, "[rain] settings 4 Some(0.9)"));
        assert_eq!(s.statuses()[0].settings_revision, Some(4));
        assert!(s.settings("rain", "1.0.0", 4, r#"{"strength":0.1}"#).unwrap().contains("4 runs already"), "a resend is not a change");
        assert!(s.settings("rain", "1.0.0", 2, r#"{"strength":0.1}"#).unwrap().contains("runs already"));
        assert!(!logged(&rec, "[rain] settings 4 Some(0.1)") && !logged(&rec, "[rain] settings 2 Some(0.1)"));
        assert!(s.settings("rain", "0.9.0", 5, "{}").unwrap_err().contains("runs 1.0.0"));
        for bad in ["[]", "1", "not json"] { assert!(s.settings("rain", "1.0.0", 5, bad).is_err(), "{bad}"); }
        assert!(s.settings("old", "1.0.0", 5, "{}").unwrap_err().contains("API 5"));
        assert!(s.settings("absent", "1.0.0", 5, "{}").unwrap_err().contains("does not run"));
        assert_eq!(s.app_message("fresh", &AppMessage::Settings { version: "1.0.0".into(), revision: 1, values: r#"{"strength":0.2}"#.into() }), Ok("settings fresh 1 delivered".into()));
        s.event("settings:changed");
        assert_eq!(rec.logs.lock().unwrap().iter().filter(|l| l.contains("] settings ")).count(), 2, "a game event cannot pose as the settings");
        // A restart reads the file again: a new activation runs its current revision.
        std::fs::write(&file, r#"{"revision":6,"versions":{"1.0.0":{"strength":0.4}}}"#).unwrap();
        s.add(ActiveFeature { id: "rain2".into(), settings: Some(file.clone()), ..meta("rain2", &[], Multiplayer::Allowed) }, Ok(entry()));
        assert!(logged(&rec, "[rain2] start settings 6 Some(0.4)"));
        assert!(s.settings("rain", "1.0.0", 7, r#"{"strength":13}"#).unwrap_err().contains("failed"));
        assert_eq!(s.statuses()[0].state, State::Failed);
    }

    // A control event reaches its own feature only, with the controls capability, as
    // Feature::control. A game event with the prefix is dropped; an app loss releases what is held.
    #[test]
    fn controls_reach_their_own_feature_and_release_at_app_loss() {
        use crate::Control;
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.add(meta("ctl", &[Capability::Controls], Multiplayer::Allowed), Ok(entry()));
        s.add(meta("other", &[Capability::Controls], Multiplayer::Allowed), Ok(entry()));
        s.add(meta("no-ctl", &[Capability::VarsRead], Multiplayer::Allowed), Ok(entry()));
        let logged = |l: &str| rec.logs.lock().unwrap().iter().any(|x| x == l);
        assert_eq!(s.control("ctl", "toggle", Control::Button(true)), Ok(()));
        assert_eq!(s.control("ctl", "zoom", Control::Delta(-2.0)), Ok(()));
        assert_eq!(s.control("ctl", "lever", Control::Axis(0.5)), Ok(()));
        assert!(logged("[ctl] control toggle Button(true)") && logged("[ctl] control zoom Delta(-2.0)"));
        assert!(!rec.logs.lock().unwrap().iter().any(|l| l.starts_with("[other] control")), "another feature does not see it");
        assert!(s.control("no-ctl", "toggle", Control::Button(true)).unwrap_err().contains("controls"));
        assert!(s.control("ctl", "Bad id", Control::Button(true)).is_err());
        s.event("input:toggle");
        assert_eq!(rec.logs.lock().unwrap().iter().filter(|l| l.starts_with("[ctl] control toggle")).count(), 1, "a game event cannot pose as a control");
        s.disconnect();
        assert!(logged("[ctl] control toggle Button(false)") && logged("[ctl] control lever Axis(0.0)"));
        assert!(!logged("[ctl] control zoom Delta(0.0)"), "a delta is never held");
    }

    #[test]
    fn control_queue_merges_values_but_never_buttons() {
        use crate::Control::*;
        let mut q = ControlQueue::default();
        for (c, v) in [("a", Axis(0.1)), ("d", Delta(1.0)), ("a", Axis(0.3)), ("d", Delta(2.0)), ("b", Button(true)), ("b", Button(false)), ("a", Axis(0.4))] {
            q.push(1, "f".into(), c.into(), v).unwrap();
        }
        let got: Vec<_> = q.take().events.into_iter().map(|(_, _, c, v)| (c, v)).collect();
        assert_eq!(got, [("a".into(), Axis(0.4)), ("d".into(), Delta(3.0)), ("b".into(), Button(true)), ("b".into(), Button(false))]);
        q.push(1, "f".into(), "b".into(), Button(true)).unwrap();
        q.push(1, "f".into(), "b".into(), Axis(1.0)).unwrap();
        assert_eq!(q.take().events.len(), 2, "a value after a button of the same control is not merged into it");
        q.push(1, "f".into(), "a".into(), Axis(0.1)).unwrap();
        q.push(2, "f".into(), "a".into(), Axis(0.2)).unwrap();
        assert_eq!(q.take().events.len(), 2, "events of two app links are not merged");
        // Review: a full queue must not drop a release alone. It drops all that waits and asks
        // for a release first; the event that found it full stays.
        for i in 0..ControlQueue::MAX / 2 {
            q.push(1, "f".into(), format!("b{i}"), Button(true)).unwrap();
            q.push(1, "f".into(), format!("b{i}"), Button(false)).unwrap();
        }
        assert!(q.push(1, "f".into(), "b0".into(), Button(true)).is_err());
        assert_eq!(q.take(), ControlBatch { release: true, events: vec![(1, "f".into(), "b0".into(), Button(true))] });
        assert!(!q.take().release, "one release per overflow");
    }

    // Review: a stop (turned off, failed, blocked) gives held controls their rest value first,
    // and forgets them, so a later app loss sends nothing to a new instance.
    #[test]
    fn a_feature_stop_releases_its_held_controls_first() {
        use crate::Control;
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.add(meta("ctl", &[Capability::Controls], Multiplayer::Allowed), Ok(entry()));
        s.add(meta("other", &[Capability::Controls], Multiplayer::Allowed), Ok(entry()));
        s.control("ctl", "toggle", Control::Button(true)).unwrap();
        s.control("other", "lever", Control::Axis(0.5)).unwrap();
        assert!(s.stop("ctl", State::Stopped, None));
        let logs = rec.logs.lock().unwrap().clone();
        let up = logs.iter().position(|l| l == "[ctl] control toggle Button(false)").expect("released");
        let stop = logs.iter().position(|l| l == "feature ctl: Stopped").expect("stopped");
        assert!(up < stop, "the release comes before the stop call: {logs:?}");
        assert!(!logs.iter().any(|l| l == "[other] control lever Axis(0.0)"), "another feature keeps its controls");
        s.disconnect();
        assert_eq!(rec.logs.lock().unwrap().iter().filter(|l| l.starts_with("[ctl] control")).count(), 2, "nothing more for the stopped feature");
        assert!(rec.logs.lock().unwrap().iter().any(|l| l == "[other] control lever Axis(0.0)"));
        // Review round 2: a feature that fails in its stop release is Failed, and stop still
        // returns true (the ATS host publishes a new report on true).
        s.control("other", "boom", Control::Button(true)).unwrap();
        assert!(s.stop("other", State::Stopped, None), "it ran, so the stop changed it");
        let st = s.statuses().into_iter().find(|f| f.id == "other").unwrap();
        assert_eq!(st.state, State::Failed, "{st:?}");
        assert!(!s.stop("other", State::Stopped, None), "a second stop changes nothing");
    }

    #[test]
    fn refused_calls_and_panics_stay_inside() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.add(meta("no-input", &[Capability::VarsRead], Multiplayer::Allowed), Ok(entry()));
        s.frame::<&str>(1, 10, &[(crate::names::SPEED_MPS, Value::F64(20.0))], &[]);
        assert!(rec.ui.lock().unwrap().is_empty(), "no ui_app: refused");
        assert!(rec.logs.lock().unwrap().iter().any(|l| l.contains("press: Refused")));
        s.event("boom");
        let st = &s.statuses()[0];
        assert_eq!((st.state, st.detail.as_deref()), (State::Failed, Some("stopped: the feature failed in event")), "a panic is caught and stops the feature");
        assert_eq!(rec.stopped.lock().unwrap().as_slice(), ["no-input".to_string()], "the host releases its inputs");
        s.frame::<&str>(2, 20, &[], &[]);
        assert_eq!(rec.logs.lock().unwrap().iter().filter(|l| l.contains("press: Refused")).count(), 1, "a failed feature gets no more frames");
    }

    #[test]
    fn multiplayer_blocks_and_stops_flagged_features() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        let caps = [Capability::UiApp];
        s.add(meta("ok", &caps, Multiplayer::Allowed), Ok(entry()));
        s.add(meta("risky", &caps, Multiplayer::Risk), Ok(entry()));
        s.add(meta("blocked", &caps, Multiplayer::Blocked), Ok(entry()));
        assert!(s.set_multiplayer("truckersmp"));
        let st: Vec<_> = s.statuses().into_iter().map(|f| (f.id, f.state)).collect();
        assert_eq!(st, [("ok".into(), State::Running), ("risky".into(), State::Running), ("blocked".into(), State::Blocked)]);
        assert!(!s.set_multiplayer("truckersmp"), "only once");
        // At load in multiplayer: a blocked feature is not even loaded.
        let list = ActiveList { features: vec![meta("blocked", &caps, Multiplayer::Blocked)], ..Default::default() };
        let s2 = FeatureSet::load("ats", &list, Arc::new(B(rec.clone())), true);
        assert_eq!(s2.statuses()[0].state, State::Blocked);
    }

    #[test]
    fn a_missing_or_foreign_library_fails_alone() {
        let rec = Arc::new(Rec::default());
        let mut list = ActiveList { features: vec![meta("gone", &[], Multiplayer::Allowed)], ..Default::default() };
        list.features[0].library = std::env::temp_dir().join("fm-no-such-feature.dll");
        list.features.push(ActiveFeature { games: vec!["msfs2024".into()], ..meta("other-game", &[], Multiplayer::Allowed) });
        let s = FeatureSet::load("ats", &list, Arc::new(B(rec.clone())), false);
        let st = s.statuses();
        assert_eq!(st.len(), 1, "only features for this game");
        assert_eq!(st[0].state, State::Failed);
        assert!(st[0].detail.as_deref().unwrap().contains("cannot load"));
        // Prepare succeeded, so the host releases what it reserved there (plan 7.5 review).
        assert_eq!(*rec.cleanups.lock().unwrap(), vec!["gone".to_string()]);
    }

    /// The rules of game_command_cb: the capability, a declared first word, one plain command per
    /// call. And a blocked feature sends no command after multiplayer starts.
    #[test]
    fn a_command_needs_the_capability_and_a_declared_first_word() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        let cmds = vec!["g_set_weather".to_string()];
        s.add(ActiveFeature { game_commands: cmds.clone(), ..meta("weather", &[Capability::GameCommand], Multiplayer::Blocked) }, Ok(entry()));
        s.add(ActiveFeature { game_commands: cmds, ..meta("no-cap", &[], Multiplayer::Allowed) }, Ok(entry()));
        for ev in ["cmd:g_set_weather.1.i", "cmd:g_set_time.12", "cmd:g_set_weather.1;quit"] {
            s.event(ev);
        }
        assert_eq!(rec.commands.lock().unwrap().as_slice(), [("weather".to_string(), "g_set_weather 1 i".to_string())]);
        let logs = rec.logs.lock().unwrap().clone();
        assert!(logs.iter().any(|l| l == "[weather] cmd:g_set_time.12: Err(Refused)"), "not declared: {logs:?}");
        assert!(logs.iter().any(|l| l == "[weather] cmd:g_set_weather.1;quit: Err(BadArgs)"), "no second command");
        assert!(logs.iter().any(|l| l == "[no-cap] cmd:g_set_weather.1.i: Err(Refused)"), "no game_command");
        assert!(s.set_multiplayer("truckersmp"));
        s.event("cmd:g_set_weather.0.i");
        let sent = rec.commands.lock().unwrap().iter().filter(|(_, t)| t != "<stopping>").count();
        assert_eq!(sent, 1, "multiplayer: the blocked feature stopped and sent nothing");
    }

    /// The waiting commands of a feature are dropped BEFORE its own stop runs, so a command that
    /// stop sends (the release of a held game value) is kept and reaches the game.
    #[test]
    fn the_commands_that_stop_sends_are_kept() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        let cmds = vec!["g_bad_weather_factor".to_string(), "g_set_weather".to_string()];
        s.add(ActiveFeature { game_commands: cmds, ..meta("weather", &[Capability::GameCommand], Multiplayer::Blocked) }, Ok(entry()));
        s.event("cmd:g_set_weather.1.i");
        s.event("stopcmd:g_bad_weather_factor.1");
        assert!(s.stop("weather", State::Stopped, None));
        let order: Vec<String> = rec.commands.lock().unwrap().iter().map(|(_, t)| t.clone()).collect();
        assert_eq!(order, ["g_set_weather 1 i", "<stopping>", "g_bad_weather_factor 1"]);
        assert_eq!(rec.stopped.lock().unwrap().as_slice(), ["weather"]);
    }

    /// The data folder of active.json reaches the feature (Host::data_dir); none gives None.
    #[test]
    fn a_feature_gets_its_data_folder() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        let dir = std::env::temp_dir().join("fm-features").join("weather").join("data");
        s.add(ActiveFeature { data_dir: Some(dir.clone()), ..meta("weather", &[], Multiplayer::Allowed) }, Ok(entry()));
        s.add(meta("old-app", &[], Multiplayer::Allowed), Ok(entry()));
        s.event("dir");
        assert!(logged(&rec, &format!("[weather] dir: {:?}", Some(dir))), "{:?}", rec.logs.lock().unwrap());
        assert!(logged(&rec, "[old-app] dir: None"));
    }

    /// active.json with a capability that this build does not know (the removed "keys_read", or a
    /// newer one) and the old "keys" field: the list loads, and only that feature fails, before
    /// its library loads. The other feature goes on to its own load.
    #[test]
    fn an_unknown_capability_fails_only_its_feature() {
        let gone = std::env::temp_dir().join("fm-no-such-feature.dll");
        let list: ActiveList = serde_json::from_value(serde_json::json!({ "features": [
            { "id": "old-keys", "version": "0.1.0", "library": gone, "games": ["ats"], "capabilities": ["ui_app", "keys_read"], "keys": [{ "id": "rain", "key": "R" }] },
            { "id": "fine", "version": "1.0.0", "library": gone, "games": ["ats"], "capabilities": ["ui_app"] }
        ] }))
        .unwrap();
        assert_eq!(list.features[0].capabilities, [Capability::UiApp, Capability::Unknown]);
        let s = FeatureSet::load("ats", &list, Arc::new(B(Arc::new(Rec::default()))), false);
        let st = s.statuses();
        assert_eq!((st[0].state, st[0].detail.as_deref()), (State::Failed, Some(UNKNOWN_CAPABILITY)));
        assert!(st[1].detail.as_deref().unwrap().contains("cannot load"), "{:?}", st[1]);
    }

    /// API 1 and 2 features (built before http_get): each gets a host that says its version, and
    /// writes its shorter table (no on_http), as the SDK of that time did. Both load and run; a
    /// v3 feature refuses an older host.
    #[test]
    fn api_1_and_2_features_still_load() {
        unsafe extern "C" fn frame(_: *mut c_void, _: *const AbiFrame) -> i32 {
            abi::OK
        }
        unsafe extern "C" fn event(_: *mut c_void, _: *const u8, _: usize) -> i32 {
            abi::OK
        }
        unsafe extern "C" fn stop(_: *mut c_void) {}
        /// FeatureApi of API 1 and 2: the fields before on_http.
        #[repr(C)]
        struct OldTable {
            api_version: u32,
            instance: *mut c_void,
            on_frame: Option<unsafe extern "C" fn(*mut c_void, *const AbiFrame) -> i32>,
            on_event: Option<unsafe extern "C" fn(*mut c_void, *const u8, usize) -> i32>,
            stop: Option<unsafe extern "C" fn(*mut c_void)>,
        }
        unsafe fn start_old(v: u32, host: *const HostApi, out: *mut FeatureApi) -> i32 {
            if (*host).api_version != v {
                return abi::BAD_ARGS;
            }
            *(out as *mut OldTable) = OldTable { api_version: v, instance: std::ptr::dangling_mut::<c_void>(), on_frame: Some(frame), on_event: Some(event), stop: Some(stop) };
            abi::OK
        }
        unsafe extern "C" fn start_v1(host: *const HostApi, out: *mut FeatureApi) -> i32 {
            start_old(1, host, out)
        }
        unsafe extern "C" fn start_v2(host: *const HostApi, out: *mut FeatureApi) -> i32 {
            start_old(2, host, out)
        }
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.add(meta("v1", &[], Multiplayer::Allowed), Ok(Entry { start: start_v1, api: 1 }));
        s.add(meta("v2", &[Capability::Network], Multiplayer::Allowed), Ok(Entry { start: start_v2, api: 2 }));
        s.add(meta("new-as-old", &[], Multiplayer::Allowed), Ok(Entry { start: fm_feature_start, api: 2 }));
        let st: Vec<_> = s.statuses().into_iter().map(|f| (f.id, f.state)).collect();
        assert_eq!(st, [("v1".into(), State::Running), ("v2".into(), State::Running), ("new-as-old".into(), State::Failed)], "a v3 feature refuses an older host");
        s.frame::<&str>(1, 10, &[], &[]);
        s.event("paused");
        assert!(!s.http_answer("v2", 1, Err(HostError::Failed)), "an old feature has no request to answer");
        assert!(s.statuses().iter().take(2).all(|f| f.state == State::Running));
    }

    const METAR: &str = "https://tgftp.nws.noaa.gov/data/observations/metar/stations/KCYS.TXT";

    fn net_meta(id: &str) -> ActiveFeature {
        ActiveFeature { network_hosts: vec!["tgftp.nws.noaa.gov".into()], ..meta(id, &[Capability::Network], Multiplayer::Blocked) }
    }

    fn logged(rec: &Rec, line: &str) -> bool {
        rec.logs.lock().unwrap().iter().any(|l| l == line)
    }

    /// The URL rule of http_get_cb: the capability, https, a declared host and no port or user
    /// info. Only good requests reach the backend, each with its own id.
    #[test]
    fn http_get_needs_the_capability_https_and_a_declared_host() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.add(net_meta("weather"), Ok(entry()));
        s.add(ActiveFeature { network_hosts: vec!["tgftp.nws.noaa.gov".into()], ..meta("no-cap", &[], Multiplayer::Allowed) }, Ok(entry()));
        let cases = [
            (METAR.to_string(), "Ok(1)"),
            ("https://TGFTP.nws.noaa.gov/data/x.TXT?a=1#b".into(), "Ok(2)"),
            ("https://example.com/x".into(), "Err(Refused)"),
            ("https://tgftp.nws.noaa.gov.example.com/x".into(), "Err(Refused)"),
            (METAR.replace("https:", "http:"), "Err(Refused)"),
            ("ftp://tgftp.nws.noaa.gov/x".into(), "Err(Refused)"),
            ("https://user@tgftp.nws.noaa.gov/x".into(), "Err(Refused)"),
            ("https://user:pw@tgftp.nws.noaa.gov/x".into(), "Err(Refused)"),
            ("https://tgftp.nws.noaa.gov:8443/x".into(), "Err(Refused)"),
            ("https://tgftp.nws.noaa.gov:443/x".into(), "Err(Refused)"),
            ("https://[::1]/x".into(), "Err(Refused)"),
            ("https://tgftp%2enws.noaa.gov/x".into(), "Err(Refused)"),
            ("https:///x".into(), "Err(BadArgs)"),
            ("https://tgftp.nws.noaa.gov/a b".into(), "Err(BadArgs)"),
            ("https://tgftp.nws.noaa.gov\\.example.com/".into(), "Err(BadArgs)"),
        ];
        // The feature log cuts lines at 1000 characters, so the length rule is checked directly.
        let hosts = ["tgftp.nws.noaa.gov".to_string()];
        assert_eq!(check_url(&format!("https://tgftp.nws.noaa.gov/{}", "a".repeat(MAX_URL)), &hosts), Err(abi::BAD_ARGS));
        assert_eq!(check_url(&format!("https://tgftp.nws.noaa.gov/{}", "a".repeat(MAX_URL - 30)), &hosts), Ok(()));
        for (url, _) in &cases {
            s.event(&format!("get:{url}"));
        }
        for (url, want) in &cases {
            assert!(logged(&rec, &format!("[weather] get:{url}: {want}")), "{url}: want {want}, logs {:?}", rec.logs.lock().unwrap());
            assert!(logged(&rec, &format!("[no-cap] get:{url}: Err(Refused)")), "no capability: {url}");
        }
        let sent: Vec<(String, u64)> = rec.http.lock().unwrap().iter().map(|(f, id, _)| (f.clone(), *id)).collect();
        assert_eq!(sent, [("weather".to_string(), 1), ("weather".to_string(), 2)], "only the good requests reach the backend");
        assert_eq!(s.waiting(), 2);
    }

    /// MAX_WAITING per feature, answers to the right feature and id only, any HTTP status is an
    /// answer, and a panic in http stops that feature.
    #[test]
    fn answers_reach_the_right_feature_and_too_many_waiting_is_busy() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.add(net_meta("a"), Ok(entry()));
        s.add(net_meta("b"), Ok(entry()));
        for _ in 0..MAX_WAITING {
            s.event(&format!("get:{METAR}"));
        }
        s.event(&format!("get:{METAR}"));
        assert!(logged(&rec, &format!("[a] get:{METAR}: Err(Busy)")) && logged(&rec, &format!("[b] get:{METAR}: Err(Busy)")));
        let ids = |f: &str| -> Vec<u64> { rec.http.lock().unwrap().iter().filter(|(x, _, _)| x == f).map(|(_, id, _)| *id).collect() };
        let (a, b) = (ids("a"), ids("b"));
        assert_eq!((a.len(), b.len()), (MAX_WAITING, MAX_WAITING));
        assert!(!s.http_answer("a", b[0], Ok(HttpAnswer { status: 200, body: b"x".to_vec() })), "an id of b is not an answer for a");
        assert!(s.http_answer("b", b[0], Ok(HttpAnswer { status: 200, body: b"METAR KCYS".to_vec() })));
        assert!(logged(&rec, &format!("[b] http {}: 200 METAR KCYS", b[0])));
        assert!(!rec.logs.lock().unwrap().iter().any(|l| l.starts_with("[a] http ")), "a got nothing");
        assert!(!s.http_answer("b", b[0], Ok(HttpAnswer { status: 200, body: vec![] })), "one answer per request");
        assert!(s.http_answer("a", a[0], Ok(HttpAnswer { status: 404, body: b"not found".to_vec() })));
        assert!(logged(&rec, &format!("[a] http {}: 404 not found", a[0])), "a 404 is an answer");
        assert!(s.http_answer("a", a[1], Err(HostError::Refused)));
        assert!(logged(&rec, &format!("[a] http {}: Refused", a[1])));
        s.event(&format!("get:{METAR}"));
        assert_eq!(ids("a").len(), MAX_WAITING + 1, "an answer frees a place");
        assert!(s.http_answer("b", b[1], Ok(HttpAnswer { status: 200, body: b"boom".to_vec() })));
        let st = s.statuses();
        assert_eq!((st[1].state, st[1].detail.as_deref()), (State::Failed, Some("stopped: the feature failed in http")));
        assert_eq!(st[0].state, State::Running);
    }

    /// No answer in HTTP_TIMEOUT: the host answers TIMEOUT and a late answer is dropped. A stop
    /// (turned off, multiplayer) drops the waiting requests of that feature only.
    #[test]
    fn a_request_times_out_and_a_stop_drops_its_waiting_requests() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.add(net_meta("slow"), Ok(entry()));
        s.add(ActiveFeature { multiplayer: Multiplayer::Allowed, ..net_meta("other") }, Ok(entry()));
        let t0 = Instant::now();
        s.event(&format!("get:{METAR}"));
        s.expire_at(t0 + HTTP_TIMEOUT - Duration::from_secs(2));
        assert_eq!(s.waiting(), 2, "not yet");
        s.expire_at(t0 + HTTP_TIMEOUT + Duration::from_secs(1));
        assert_eq!(s.waiting(), 0);
        assert!(logged(&rec, "[slow] http 1: Timeout") && logged(&rec, "[other] http 2: Timeout"));
        assert!(!s.http_answer("slow", 1, Ok(HttpAnswer { status: 200, body: vec![] })), "a late answer is dropped");

        s.event(&format!("get:{METAR}"));
        assert_eq!(s.waiting(), 2);
        assert!(s.set_multiplayer("truckersmp"), "network is allowed in multiplayer, but a blocked feature stops");
        assert_eq!(s.waiting(), 1, "the stop dropped the request of slow only");
        assert!(!s.http_answer("slow", 3, Ok(HttpAnswer { status: 200, body: vec![] })));
        assert!(s.http_answer("other", 4, Ok(HttpAnswer { status: 200, body: b"ok".to_vec() })), "network still runs in multiplayer");
        s.event(&format!("get:{METAR}"));
        s.stop("other", State::Stopped, None);
        s.expire_at(Instant::now() + HTTP_TIMEOUT * 2);
        assert!(!rec.logs.lock().unwrap().iter().any(|l| l == "[slow] http 3: Timeout" || l == "[other] http 5: Timeout"), "a stopped feature gets no answer");
    }

    /// A host with no network path (the default Backend::http_get, the MSFS host today): the
    /// feature gets FAILED at once and nothing waits.
    #[test]
    fn a_host_with_no_network_fails_at_once() {
        struct NoNet(Arc<Rec>);
        impl Backend for NoNet {
            fn ui(&self, _: &str, _: &str, _: &str) {}
            fn input(&self, _: &str, _: &str, _: Input) -> Result<(), String> {
                Ok(())
            }
            fn log(&self, l: &str) {
                self.0.logs.lock().unwrap().push(l.into());
            }
            fn stopped(&self, _: &str) {}
        }
        let rec = Arc::new(Rec::default());
        let mut s = FeatureSet::new("msfs2024", Arc::new(NoNet(rec.clone())), false);
        s.add(net_meta("weather"), Ok(entry()));
        s.event(&format!("get:{METAR}"));
        assert!(logged(&rec, &format!("[weather] get:{METAR}: Err(Failed)")));
        assert!(logged(&rec, "[weather] http_get: this host has no network yet"));
        assert_eq!(s.waiting(), 0);
    }

    /// The review case: a start that sends a command, then fails. The backend must release it,
    /// or the queued command runs for a feature that never ran.
    #[test]
    fn a_failed_start_releases_what_it_took() {
        unsafe extern "C" fn start_fail(host: *const HostApi, _: *mut FeatureApi) -> i32 {
            let t = "g_set_weather 1 i";
            ((*host).game_command)((*host).ctx, t.as_ptr(), t.len());
            abi::FAILED
        }
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        let m = ActiveFeature { game_commands: vec!["g_set_weather".into()], ..meta("bad", &[Capability::GameCommand], Multiplayer::Allowed) };
        s.add(m, Ok(Entry { start: start_fail, api: abi::API_VERSION }));
        let order: Vec<String> = rec.commands.lock().unwrap().iter().map(|(_, t)| t.clone()).collect();
        assert_eq!(order, ["g_set_weather 1 i", "<stopping>"], "the command reached the backend, then the drop");
        assert_eq!(rec.stopped.lock().unwrap().as_slice(), ["bad".to_string()], "and the backend was told to release it");
        assert_eq!(s.statuses()[0].state, State::Failed);
    }

    fn logs(rec: &Rec) -> Vec<String> {
        rec.logs.lock().unwrap().clone()
    }

    fn list(features: &[ActiveFeature]) -> ActiveList {
        ActiveList { features: features.to_vec(), ..Default::default() }
    }

    fn version(id: &str, v: &str) -> ActiveFeature {
        ActiveFeature { version: v.into(), ..meta(id, &[Capability::Network, Capability::VarsRead], Multiplayer::Blocked) }
    }

    fn state(s: &FeatureSet, id: &str) -> (State, String, Option<String>) {
        let st = s.statuses().into_iter().find(|f| f.id == id).unwrap();
        (st.state, st.version, st.detail)
    }

    fn starts(rec: &Rec, id: &str) -> usize {
        let line = format!("[{id}] start on ats");
        rec.logs.lock().unwrap().iter().filter(|l| **l == line).count()
    }

    fn load_ok(_: &ActiveFeature) -> Result<Entry, String> {
        Ok(entry())
    }

    const NONE: [String; 0] = [];

    /// Feature session sync plan, slice 1: the running set follows the list. On, off, on again
    /// (a new instance of the same record), and a link loss (stop_all) and return.
    #[test]
    fn sync_starts_and_stops_features_with_the_list() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        let both = list(&[version("a", "1.0.0"), version("b", "1.0.0")]);
        assert_eq!(s.sync_with(&both, load_ok), ["a", "b"]);
        assert_eq!(s.sync_with(&both, load_ok), NONE, "no change, no start");
        assert_eq!(s.sync_with(&list(&[version("a", "1.0.0")]), load_ok), NONE);
        assert_eq!(state(&s, "b"), (State::Stopped, "1.0.0".into(), Some(TURNED_OFF.into())));
        assert_eq!(*rec.cleanups.lock().unwrap(), ["b"], "a turned off feature is cleaned up");
        assert_eq!(s.sync_with(&both, load_ok), ["b"]);
        assert_eq!((state(&s, "b").0, starts(&rec, "b")), (State::Running, 2));
        // FM closes: the host stops all; FM comes back: all start again.
        s.stop_all();
        assert_eq!(s.sync_with(&both, load_ok), ["a", "b"]);
        assert_eq!((starts(&rec, "a"), starts(&rec, "b")), (2, 3));
    }

    /// Game policies plan, slice 2: a policy for this game keeps a feature off. "now" stops a
    /// running feature within one sync, "next start" lets it run and refuses its next start, a
    /// multiplayer policy waits for its client, and a policy of another game does nothing.
    #[test]
    fn game_policies_stop_and_refuse_features() {
        use crate::active::{Apply, PolicyStop, When};
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        let open = |id: &str| ActiveFeature { multiplayer: Multiplayer::Allowed, ..version(id, "1.0.0") };
        let stop = |id: &str, game: &str, when: When, apply: Apply| PolicyStop { feature: id.into(), version: None, game: game.into(), when, apply, policy: format!("p-{id}"), reason: "Asked by the game developer.".into() };
        let ids = ["now", "later", "mp", "other"].map(open);
        assert_eq!(s.sync_with(&list(&ids), load_ok).len(), 4);
        let mut l = list(&ids);
        l.stops = vec![
            stop("now", "ats", When::Always, Apply::NextStart),
            stop("now", "ats", When::Always, Apply::Now),
            stop("later", "ats", When::Always, Apply::NextStart),
            stop("mp", "ats", When::Multiplayer(vec!["truckersmp".into()]), Apply::Now),
            stop("other", "msfs2024", When::Always, Apply::Now),
        ];
        assert_eq!(s.sync_with(&l, load_ok), NONE);
        let detail = |id: &str| format!("application policy p-{id}: Asked by the game developer.");
        assert_eq!(state(&s, "now"), (State::Blocked, "1.0.0".into(), Some(format!("stopped: {}", detail("now")))));
        assert_eq!(*rec.cleanups.lock().unwrap(), ["now"], "a stopped feature is cleaned up");
        assert!(["later", "mp", "other"].iter().all(|id| state(&s, id).0 == State::Running), "next start, no client, other game");
        // The member turns "later" off and on: its next start is refused with the policy.
        assert_eq!(s.sync_with(&ActiveList { features: ["now", "mp", "other"].map(open).to_vec(), ..l.clone() }, load_ok), NONE);
        assert_eq!(s.sync_with(&l, load_ok), NONE);
        assert_eq!(state(&s, "later"), (State::Blocked, "1.0.0".into(), Some(detail("later"))));
        // The client starts: the multiplayer policy stops "mp"; the name compares with no case.
        assert!(s.set_multiplayer("TruckersMP"));
        assert_eq!(state(&s, "mp").0, State::Blocked);
        assert_eq!(state(&s, "other").0, State::Running);
        // A new set with the client from the start: the policy refuses the first start.
        let mut s = set(&rec);
        s.set_multiplayer("truckersmp");
        s.sync_with(&l, load_ok);
        assert_eq!(state(&s, "mp"), (State::Blocked, "1.0.0".into(), Some(detail("mp"))));
        assert_eq!(state(&s, "other").0, State::Running);
        // Sol r1: a stop names a version. The session runs 1.0.0 while the list has 2.0.0; a stop
        // for 2.0.0 does nothing to it, a stop for 1.0.0 stops it.
        let mut s = set(&rec);
        assert_eq!(s.sync_with(&list(&[open("v")]), load_ok), ["v"]);
        let mut l = list(&[ActiveFeature { version: "2.0.0".into(), ..open("v") }]);
        l.stops = vec![PolicyStop { version: Some("2.0.0".into()), ..stop("v", "ats", When::Always, Apply::Now) }];
        s.sync_with(&l, load_ok);
        assert_eq!(state(&s, "v").0, State::Running, "another version");
        l.stops[0].version = Some("1.0.0".into());
        s.sync_with(&l, load_ok);
        assert_eq!(state(&s, "v"), (State::Blocked, "1.0.0".into(), Some(format!("stopped: {}", detail("v")))));
        // Sol r1: the app leaves a feature that a policy stops everywhere out of the list, with
        // its stop. It stops as Blocked (not turned off), and a lifted policy waits for the next
        // game start.
        let mut s = set(&rec);
        assert_eq!(s.sync_with(&list(&[open("x")]), load_ok), ["x"]);
        s.sync_with(&ActiveList { stops: vec![stop("x", "ats", When::Always, Apply::Now)], ..list(&[]) }, load_ok);
        assert_eq!(state(&s, "x"), (State::Blocked, "1.0.0".into(), Some(format!("stopped: {}", detail("x")))));
        assert_eq!(s.sync_with(&list(&[open("x")]), load_ok), NONE, "no start in this session");
        // Sol r3: the same for a host whose first list already left the feature out.
        let mut s = set(&rec);
        s.sync_with(&ActiveList { stops: vec![stop("y", "ats", When::Always, Apply::Now)], ..list(&[]) }, load_ok);
        assert_eq!(s.sync_with(&list(&[open("y")]), load_ok), NONE, "a lifted policy waits for the next game start");
        assert_eq!(state(&s, "y"), (State::Blocked, "1.0.0".into(), Some(detail("y"))));
    }

    /// Decisions 2 and 5: an update in the session waits for the next game start, for a running
    /// feature and for one that was off at the start (pinned from the store). A new install starts
    /// and keeps its first version. A stopped old version starts again after an update.
    #[test]
    fn sync_keeps_the_session_version_when_an_update_drops_the_game() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.pin([("a".to_string(), "1.0.0".to_string())].into());
        assert_eq!(s.sync_with(&list(&[version("a", "1.0.0")]), load_ok), ["a"]);
        let msfs_only = ActiveFeature { games: vec!["msfs2024".into()], ..version("a", "2.0.0") };
        assert_eq!(s.sync_with(&list(std::slice::from_ref(&msfs_only)), load_ok), NONE);
        assert_eq!(state(&s, "a"), (State::Running, "1.0.0".into(), None), "the session version runs on");
        // Off and on: the session version starts again from its own record.
        s.sync_with(&list(&[]), load_ok);
        assert_eq!(s.sync_with(&list(std::slice::from_ref(&msfs_only)), load_ok), ["a"]);
        // A feature of another game that never started here does not start, and no wait shows.
        let other = ActiveFeature { games: vec!["msfs2024".into()], ..version("b", "1.0.0") };
        assert_eq!(s.sync_with(&list(&[msfs_only, other]), load_ok), NONE);
        assert!(s.statuses().iter().all(|f| f.id != "b"));
    }

    #[test]
    fn sync_keeps_the_session_version() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.pin([("a".to_string(), "1.0.0".to_string()), ("off".to_string(), "1.0.0".to_string())].into());
        assert_eq!(s.sync_with(&list(&[version("a", "1.0.0")]), load_ok), ["a"]);
        // Updates of a running feature and of a feature that was off at the start.
        assert_eq!(s.sync_with(&list(&[version("a", "2.0.0"), version("off", "2.0.0")]), load_ok), NONE);
        assert_eq!(state(&s, "a"), (State::Running, "1.0.0".into(), None), "1.0.0 runs on");
        assert_eq!(state(&s, "off"), (State::Stopped, "2.0.0".into(), Some(UPDATE_WAITS.into())));
        // Off and on, or an FM restart, after the update: the session version starts again.
        s.sync_with(&list(&[]), load_ok);
        assert_eq!(s.sync_with(&list(&[version("a", "2.0.0")]), load_ok), ["a"]);
        assert_eq!(state(&s, "a"), (State::Running, "1.0.0".into(), None));
        // A new install (not in the store at the start): its first start pins it.
        assert_eq!(s.sync_with(&list(&[version("a", "2.0.0"), version("new", "3.0.0")]), load_ok), ["new"]);
        s.sync_with(&list(&[version("a", "2.0.0"), version("new", "4.0.0")]), load_ok);
        assert_eq!(state(&s, "new"), (State::Running, "3.0.0".into(), None));
        // A list version that is the session version again (a roll back) starts the waiting one.
        assert_eq!(s.sync_with(&list(&[version("off", "1.0.0")]), load_ok), ["off"]);
        assert_eq!(state(&s, "off"), (State::Running, "1.0.0".into(), None));
    }

    fn activation(s: &FeatureSet, id: &str) -> Option<u64> {
        s.statuses().into_iter().find(|f| f.id == id).unwrap().activation
    }

    /// Plan 12.2, contract 1: Wait keeps the item stopped with the reason, `waits` asks for a
    /// retry, and the next sync starts it with a new activation. A restart that waits keeps its
    /// own entry: the loader is not called again (the session version starts).
    #[test]
    fn a_wait_keeps_the_item_stopped_and_a_later_sync_starts_it() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        let one = list(&[version("a", "1.0.0")]);
        *rec.wait.lock().unwrap() = Some("no app link".into());
        assert_eq!(s.sync_with(&one, load_ok), NONE);
        assert_eq!(state(&s, "a"), (State::Stopped, "1.0.0".into(), Some("no app link".into())));
        assert!(s.waits() && starts(&rec, "a") == 0);
        *rec.wait.lock().unwrap() = None;
        assert_eq!(s.sync_with(&one, load_ok), ["a"]);
        assert!(!s.waits());
        assert_eq!(activation(&s, "a"), Some(2), "each try has a new activation");
        // Off, then a restart that waits, then one that starts: the kept entry, no new load.
        s.sync_with(&list(&[]), load_ok);
        *rec.wait.lock().unwrap() = Some("cleanup".into());
        let no_load = |_: &ActiveFeature| -> Result<Entry, String> { Err("loader called".into()) };
        assert_eq!(s.sync_with(&one, no_load), NONE);
        assert!(s.waits());
        *rec.wait.lock().unwrap() = None;
        assert_eq!(s.sync_with(&one, no_load), ["a"]);
        assert_eq!((starts(&rec, "a"), activation(&s, "a")), (2, Some(4)));
        let admits: Vec<u64> = rec.admits.lock().unwrap().iter().map(|(_, n)| *n).collect();
        assert_eq!(admits, [1, 2, 3, 4], "activations only grow");
    }

    /// 7f defect A: the report carries the session versions, also a pin made later in the
    /// session (a first install during the game), and an update does not change a pin.
    #[test]
    fn the_report_carries_the_session_versions_with_later_pins() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.pin([("a".to_string(), "1.0.0".to_string())].into());
        assert_eq!(s.report().session_versions, [("a".to_string(), "1.0.0".to_string())].into());
        s.sync_with(&list(&[version("a", "2.0.0"), version("b", "1.0.0")]), load_ok);
        s.sync_with(&list(&[version("a", "2.0.0"), version("b", "1.1.0")]), load_ok);
        let want: BTreeMap<String, String> = [("a".to_string(), "1.0.0".to_string()), ("b".to_string(), "1.0.0".to_string())].into();
        assert_eq!(s.report().session_versions, want, "b was first installed during the game");
    }

    /// 7f finding: a start that waits is tried each second while the app is gone. The log gets
    /// one line per state or reason, not one per try; a new reason and the start still log.
    #[test]
    fn a_wait_that_repeats_logs_once_until_its_reason_changes() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        let one = list(&[version("a", "1.0.0")]);
        let count = |line: &str| rec.logs.lock().unwrap().iter().filter(|l| *l == line).count();
        *rec.wait.lock().unwrap() = Some("no app link".into());
        for _ in 0..3 { s.sync_with(&one, load_ok); }
        assert_eq!(count("feature a 1.0.0: Stopped (no app link)"), 1, "{:?}", rec.logs.lock().unwrap());
        *rec.wait.lock().unwrap() = Some("no sim".into());
        for _ in 0..2 { s.sync_with(&one, load_ok); }
        assert_eq!(count("feature a 1.0.0: Stopped (no sim)"), 1, "a new reason logs once");
        *rec.wait.lock().unwrap() = None;
        assert_eq!(s.sync_with(&one, load_ok), ["a"]);
        assert_eq!(count("feature a 1.0.0: Running"), 1);
    }

    /// Completion::Pending: no new start until `cleaned` with the same activation. A result of
    /// another activation, or a second one, is ignored; a failed cleanup is final.
    #[test]
    fn a_pending_cleanup_blocks_a_new_start_until_it_is_cleaned() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        *rec.pending.lock().unwrap() = true;
        let both = list(&[version("a", "1.0.0"), version("b", "1.0.0")]);
        assert_eq!(s.sync_with(&both, load_ok), ["a", "b"]);
        let (a, b) = (activation(&s, "a").unwrap(), activation(&s, "b").unwrap());
        s.stop_all();
        assert_eq!(s.sync_with(&both, load_ok), NONE, "both clean up");
        s.cleaned("a", a + 100, Ok(()));
        assert_eq!(s.sync_with(&both, load_ok), NONE, "a result of another activation is ignored");
        assert!(logged(&rec, &format!("feature a: cleanup result for activation {} ignored (not pending)", a + 100)));
        s.cleaned("a", a, Ok(()));
        s.cleaned("a", a, Ok(()));
        assert!(logged(&rec, &format!("feature a: cleanup result for activation {a} ignored (not pending)")), "a second result is ignored");
        s.cleaned("b", b, Err("cannot restore".into()));
        assert_eq!(s.sync_with(&both, load_ok), ["a"]);
        assert_eq!(state(&s, "b"), (State::Failed, "1.0.0".into(), Some("cleanup failed; restart the application session: cannot restore".into())));
        assert_eq!(s.sync_with(&both, load_ok), NONE, "a failed cleanup is final");
    }

    /// A load that fails after admission gives back what admit took (cleanup), and the item is
    /// Failed. Each host instance has its own id (Hello.instance).
    #[test]
    fn a_failed_load_after_admission_runs_cleanup() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        let fail = |_: &ActiveFeature| -> Result<Entry, String> { Err("cannot load".into()) };
        assert_eq!(s.sync_with(&list(&[version("a", "1.0.0")]), fail), NONE);
        assert_eq!(state(&s, "a"), (State::Failed, "1.0.0".into(), Some("cannot load".into())));
        assert_eq!(*rec.cleanups.lock().unwrap(), ["a"]);
        assert!(!s.waits());
        assert_ne!(s.instance(), set(&rec).instance());
        // 7c: a Pending host completes that cleanup later; its failure reaches the status.
        *rec.pending.lock().unwrap() = true;
        assert_eq!(s.sync_with(&list(&[version("b", "1.0.0")]), fail), NONE);
        let b = activation(&s, "b").unwrap();
        s.cleaned("b", b, Err("inputs held".into()));
        assert_eq!(state(&s, "b").2, Some("cleanup failed; restart the application session: inputs held".into()));
    }

    /// No start loop: failed, blocked and a cleanup failure stay until the next game start.
    #[test]
    fn sync_does_not_start_failed_or_blocked_features_again() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        let load = |m: &ActiveFeature| if m.id == "gone" { Err("cannot load".to_string()) } else { Ok(entry()) };
        let all = list(&[version("boom", "1.0.0"), version("gone", "1.0.0"), version("held", "1.0.0"), version("mp", "1.0.0")]);
        s.sync_with(&all, load);
        s.game_event_to::<&str>("boom", "boom", &[]);
        rec.bad_cleanup.lock().unwrap().push("held".into());
        s.sync_with(&list(&[version("boom", "1.0.0"), version("gone", "1.0.0"), version("mp", "1.0.0")]), load);
        assert!(s.set_multiplayer("truckersmp"));
        assert_eq!(s.sync_with(&all, load), NONE);
        assert_eq!(state(&s, "boom").0, State::Failed);
        assert_eq!(state(&s, "gone").0, State::Failed);
        assert_eq!(state(&s, "held"), (State::Failed, "1.0.0".into(), Some("cleanup failed; restart the application session: cannot restore".into())));
        assert_eq!(state(&s, "mp").0, State::Blocked);
        assert_eq!((starts(&rec, "boom"), starts(&rec, "held"), starts(&rec, "mp")), (1, 1, 1));
        // A new feature that is blocked in multiplayer does not start either.
        s.sync_with(&list(&[version("late", "1.0.0")]), load);
        assert_eq!(state(&s, "late").0, State::Blocked);
    }

    /// A new instance is a fresh start: its subscriptions are watched again, a late answer for the
    /// old instance does not reach it, and a lost link gives Busy at once to what waits.
    #[test]
    fn a_restarted_feature_is_a_new_instance() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        let one = list(&[ActiveFeature { network_hosts: vec!["tgftp.nws.noaa.gov".into()], ..version("w", "1.0.0") }]);
        s.sync_with(&one, load_ok);
        s.event("sub:ats:truck.fuel.amount");
        s.event(&format!("get:{METAR}"));
        s.sync_with(&list(&[]), load_ok);
        assert_eq!(s.waiting(), 0);
        s.sync_with(&one, load_ok);
        assert!(!s.http_answer("w", 1, Ok(HttpAnswer { status: 200, body: b"old".to_vec() })), "an answer of the old instance is dropped");
        assert!(!logs(&rec).iter().any(|l| l.contains("200 old")));
        s.event("sub:ats:truck.fuel.amount");
        let on = |b| ("truck.fuel.amount".to_string(), b);
        assert_eq!(*rec.watch.lock().unwrap(), [on(true), on(false), on(true)]);
        s.event(&format!("get:{METAR}"));
        s.fail_waiting(HostError::Busy);
        assert!(logged(&rec, "[w] http 2: Busy") && s.waiting() == 0, "{:?}", logs(&rec));
    }

    /// The configuration replay for a feature that starts late reaches that feature only.
    #[test]
    fn a_game_event_to_one_feature_reaches_it_only() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.add(meta("late", &[Capability::VarsRead], Multiplayer::Allowed), Ok(entry()));
        s.add(meta("old", &[Capability::VarsRead], Multiplayer::Allowed), Ok(entry()));
        s.game_event_to("late", "configuration.truck", &[("fuel.capacity", Var::F64(400.0))]);
        assert!(logs(&rec).iter().any(|l| l.starts_with("[late] event configuration.truck")));
        assert!(!logs(&rec).iter().any(|l| l.starts_with("[old] event")));
        s.game_event_to::<&str>("late", "ui:look", &[]);
        assert!(!logs(&rec).iter().any(|l| l.starts_with("[late] page")), "no page pose");
    }

    /// Any game name, no list: the backend watches a name once for all readers and stops after
    /// the last one; only readers get its value, keyed "<game>:<native>"; a feature without
    /// vars_read or a key of another game is refused before the backend sees it.
    #[test]
    fn native_values_are_watched_once_and_reach_only_their_readers() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.add(meta("a", &[Capability::VarsRead], Multiplayer::Allowed), Ok(entry()));
        s.add(meta("b", &[Capability::VarsRead], Multiplayer::Allowed), Ok(entry()));
        s.add(meta("c", &[], Multiplayer::Allowed), Ok(entry()));
        s.event("sub:ats:truck.fuel.amount");
        s.event("sub:ats:truck.fuel.amount");
        for bad in ["sub:msfs2024:TITLE,string", "sub:ats:", "sub:ats: x", "sub:truck.speed"] {
            s.event(bad);
        }
        let l = logs(&rec);
        assert_eq!(rec.watch.lock().unwrap().as_slice(), [("truck.fuel.amount".to_string(), true)], "one watch for two readers, none for bad keys");
        for f in ["a", "b"] {
            assert_eq!(l.iter().filter(|x| **x == format!("[{f}] sub:ats:truck.fuel.amount: Ok(())")).count(), 2, "{f}: again is OK");
            for bad in ["sub:msfs2024:TITLE,string", "sub:ats:", "sub:ats: x", "sub:truck.speed"] {
                assert!(l.contains(&format!("[{f}] {bad}: Err(BadArgs)")), "{f} {bad}: {l:?}");
            }
        }
        assert!(l.contains(&"[c] sub:ats:truck.fuel.amount: Err(Refused)".to_string()));

        let job = ("job.cargo", Var::Text("Logs".into()));
        s.frame(1, 10, &[], &[("truck.fuel.amount", Var::F64(312.5)), job]);
        let l = logs(&rec);
        for f in ["a", "b"] {
            assert!(l.contains(&format!("[{f}] var ats:truck.fuel.amount=F64(312.5)")), "{l:?}");
        }
        assert!(!l.iter().any(|x| x.starts_with("[c] var") || x.contains("job.cargo")), "no reader: no value");

        s.event("unsub:ats:truck.fuel.amount");
        assert_eq!(rec.watch.lock().unwrap().last(), Some(&("truck.fuel.amount".to_string(), false)), "after the last reader");
        s.event("sub:ats:truck.speed");
        s.stop("a", State::Stopped, None);
        assert_eq!(rec.watch.lock().unwrap().last(), Some(&("truck.speed".to_string(), true)), "b still reads truck.speed");
        s.stop("b", State::Stopped, None);
        assert_eq!(rec.watch.lock().unwrap().last(), Some(&("truck.speed".to_string(), false)), "a stop releases its names");
    }

    /// The game's answer reaches the readers as an event with the key; an unknown name leaves
    /// every reader's list, so the next subscribe asks the game again.
    #[test]
    fn the_game_answer_reaches_the_readers_and_an_unknown_name_is_dropped() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.add(meta("a", &[Capability::VarsRead], Multiplayer::Allowed), Ok(entry()));
        s.add(meta("b", &[Capability::VarsRead], Multiplayer::Allowed), Ok(entry()));
        s.event("sub:ats:truck.fuel.amount");
        s.event("sub:ats:truck.nope");
        s.var_result("truck.fuel.amount", Ok(()));
        s.var_result("truck.nope", Err("not found".into()));
        let l = logs(&rec);
        for f in ["a", "b"] {
            assert!(l.contains(&format!("[{f}] event var.ready [(\"name\", Text(\"ats:truck.fuel.amount\"))]")), "{l:?}");
            assert!(l.contains(&format!("[{f}] event var.unknown [(\"name\", Text(\"ats:truck.nope\")), (\"reason\", Text(\"not found\"))]")), "{l:?}");
        }
        s.frame(1, 10, &[], &[("truck.nope", Var::F64(1.0))]);
        assert!(!logs(&rec).iter().any(|x| x.contains("var ats:truck.nope")), "dropped from the lists");
        let watched = rec.watch.lock().unwrap().len();
        s.event("sub:ats:truck.nope");
        assert_eq!(rec.watch.lock().unwrap().len(), watched + 1, "asked again");
    }

    /// A later reader of a confirmed name gets its own "var.ready" at the next frame (no second
    /// watch, so the game sends no new answer); after the last reader the name is asked again.
    #[test]
    fn a_later_reader_of_a_ready_name_gets_var_ready() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.add(meta("a", &[Capability::VarsRead], Multiplayer::Allowed), Ok(entry()));
        s.event("sub:ats:truck.fuel.amount");
        s.var_result("truck.fuel.amount", Ok(()));
        s.add(meta("b", &[Capability::VarsRead], Multiplayer::Allowed), Ok(entry()));
        s.event("sub:ats:truck.fuel.amount");
        assert_eq!(rec.watch.lock().unwrap().len(), 1, "watched once");
        let ready = |f: &str| format!("[{f}] event var.ready [(\"name\", Text(\"ats:truck.fuel.amount\"))]");
        assert!(!logs(&rec).contains(&ready("b")), "not inside b's own call");
        s.frame::<&str>(1, 10, &[], &[]);
        let l = logs(&rec);
        assert_eq!(l.iter().filter(|x| **x == ready("a")).count(), 1, "a only once: {l:?}");
        assert_eq!(l.iter().filter(|x| **x == ready("b")).count(), 1, "b at the next frame: {l:?}");

        s.event("unsub:ats:truck.fuel.amount");
        s.event("sub:ats:truck.fuel.amount");
        s.frame::<&str>(2, 20, &[], &[]);
        assert_eq!(rec.watch.lock().unwrap().len(), 3, "watch off, then on again");
        assert_eq!(logs(&rec).iter().filter(|x| **x == ready("b")).count(), 1, "a new watch waits for the game");
    }

    /// A "var.ready" call can drop a name that waits in the same frame's queue: B drops Y in its
    /// X call and gets no Y; C drops Y and reads it again, and gets Y once, at the next frame
    /// (review round 3).
    #[test]
    fn a_reader_dropped_in_a_var_ready_call_gets_no_old_var_ready() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.add(meta("a", &[Capability::VarsRead], Multiplayer::Allowed), Ok(entry()));
        for k in ["x", "y"] {
            s.event(&format!("sub:ats:{k}"));
            s.var_result(k, Ok(()));
        }
        s.add(meta("b", &[Capability::VarsRead], Multiplayer::Allowed), Ok(entry()));
        s.add(meta("c", &[Capability::VarsRead], Multiplayer::Allowed), Ok(entry()));
        let to = |s: &mut FeatureSet, f: &str, id: &str| s.send(|i| i.meta.id == f, id, &[] as &[(&str, Var)], false);
        to(&mut s, "b", "onready:ats:x|unsub:ats:y");
        to(&mut s, "c", "onready:ats:x|unsub:ats:y");
        to(&mut s, "c", "onready:ats:x|sub:ats:y");
        for f in ["b", "c"] {
            to(&mut s, f, "sub:ats:x");
            to(&mut s, f, "sub:ats:y");
        }
        let ready = |f: &str, k: &str| logs(&rec).iter().filter(|x| **x == format!("[{f}] event var.ready [(\"name\", Text(\"ats:{k}\"))]")).count();
        s.frame::<&str>(1, 10, &[], &[]);
        assert_eq!((ready("b", "x"), ready("b", "y")), (1, 0), "b dropped y: no var.ready");
        assert_eq!((ready("c", "x"), ready("c", "y")), (1, 0), "c's new read waits for the next frame");
        s.frame::<&str>(2, 20, &[], &[]);
        assert_eq!(ready("b", "y"), 0);
        assert_eq!(ready("c", "y"), 1, "once, for the new read");
        assert_eq!(rec.watch.lock().unwrap().len(), 2, "a still reads x and y: no new watch");
    }

    /// Old answers: one that comes after the last reader left is dropped (a new reader waits for
    /// the new watch), and a second Ok while the name stays read gives no second "var.ready".
    #[test]
    fn an_old_answer_gives_no_second_var_ready() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.add(meta("a", &[Capability::VarsRead], Multiplayer::Allowed), Ok(entry()));
        let ready = || logs(&rec).iter().filter(|x| x.starts_with("[a] event var.ready")).count();
        s.event("sub:ats:truck.fuel.amount");
        s.event("unsub:ats:truck.fuel.amount");
        s.var_result("truck.fuel.amount", Ok(()));
        assert_eq!(ready(), 0, "no reader: dropped");
        s.event("sub:ats:truck.fuel.amount");
        s.frame::<&str>(1, 10, &[], &[]);
        assert_eq!(ready(), 0, "not cached from the old watch");
        s.var_result("truck.fuel.amount", Ok(()));
        assert_eq!(ready(), 1, "the new watch answers once");
        // Unsubscribe and subscribe again before the old answer: two Ok, one event.
        s.event("unsub:ats:truck.fuel.amount");
        s.event("sub:ats:truck.fuel.amount");
        s.var_result("truck.fuel.amount", Ok(()));
        s.var_result("truck.fuel.amount", Ok(()));
        s.frame::<&str>(2, 20, &[], &[]);
        assert_eq!(ready(), 2, "one more for the new watch, not two");
        s.var_result("truck.nope", Err("not found".into()));
        assert!(!logs(&rec).iter().any(|x| x.contains("truck.nope")), "no reader: no event");
    }

    /// One feature reads at most MAX_VARS_PER_FEATURE names; one more is BUSY.
    #[test]
    fn a_feature_reads_a_limited_number_of_names() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.add(meta("a", &[Capability::VarsRead], Multiplayer::Allowed), Ok(entry()));
        s.event(&format!("subn:{MAX_VARS_PER_FEATURE}"));
        s.event(&format!("subn:{}", MAX_VARS_PER_FEATURE + 1));
        let l = logs(&rec);
        assert!(l.contains(&format!("[a] subn:{MAX_VARS_PER_FEATURE}: Ok({MAX_VARS_PER_FEATURE})")), "{l:?}");
        assert!(l.contains(&format!("[a] subn:{}: Err(Busy)", MAX_VARS_PER_FEATURE + 1)), "{l:?}");
        assert_eq!(rec.watch.lock().unwrap().len(), MAX_VARS_PER_FEATURE);
    }

    /// Event attributes go to a version 4 feature with vars_read; one without gets the id only.
    #[test]
    fn event_attributes_need_vars_read() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.add(meta("a", &[Capability::VarsRead], Multiplayer::Allowed), Ok(entry()));
        s.add(meta("c", &[], Multiplayer::Allowed), Ok(entry()));
        s.game_event("gameplay.job.delivered", &[("revenue", Var::U64(52_000)), ("cargo", Var::Text("Logs".into())), ("cargo.mass", Var::F64(20_000.0))]);
        s.game_event("dir", &[("x", Var::Bool(true))]);
        let l = logs(&rec);
        assert!(l.contains(&r#"[a] event gameplay.job.delivered [("revenue", U64(52000)), ("cargo", Text("Logs")), ("cargo.mass", F64(20000.0))]"#.to_string()), "{l:?}");
        assert!(!l.iter().any(|x| x.starts_with("[c] event")), "no attributes without vars_read");
        assert!(l.iter().any(|x| x.starts_with("[c] dir:")), "the id still comes");
    }

    /// Any sim event name with data, with input_inject only; the name is plain ASCII with no space.
    #[test]
    fn a_sim_event_needs_input_inject_and_a_plain_name() {
        let rec = Arc::new(Rec::default());
        let mut s = set(&rec);
        s.add(meta("a", &[Capability::InputInject], Multiplayer::Allowed), Ok(entry()));
        s.add(meta("c", &[], Multiplayer::Allowed), Ok(entry()));
        s.event("send:HEADING_BUG_SET=-90");
        s.event("send:BAD NAME=1");
        assert_eq!(rec.sim_events.lock().unwrap().as_slice(), [("a".to_string(), "HEADING_BUG_SET".to_string(), -90)]);
        let l = logs(&rec);
        assert!(l.contains(&"[c] send:HEADING_BUG_SET=-90: Err(Refused)".to_string()));
        assert!(l.contains(&"[a] send:BAD NAME=1: Err(BadArgs)".to_string()));
    }
}
