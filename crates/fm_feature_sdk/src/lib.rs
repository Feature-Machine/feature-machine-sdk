// Feature SDK (plan 2026-10-18-feature-packages.md). A feature implements Feature and calls
// feature!(Type) once; the macro writes the C exports (abi.rs) and catches every panic (none may
// cross into the game). Host gives UI, inputs, sim events, engine commands, HTTPS, a data folder,
// declared game files (UserFiles), logs, and any native game value by the game's name. Neutral
// names stay the portable layer. Loaders: host.rs, hook.rs.
pub mod abi;
pub mod active;
pub mod files;
pub mod hook;
#[cfg(feature = "testing")]
pub mod testing;
#[cfg(feature = "host")]
pub mod follow;
#[cfg(feature = "host")]
pub mod host;
pub mod page;
pub mod render;
pub mod settings;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use abi::{API_VERSION, MIN_API_VERSION};
pub use files::{Access, UserFile};

/// Neutral value names that hosts give (the same names as fm_adapter).
pub mod names {
    pub const SPEED_MPS: &str = "vehicle.speed_mps";
    pub const HEADING_DEG: &str = "vehicle.heading_deg";
    /// Game world meters: X east, Y up, Z south (ATS).
    pub const POSITION_WORLD_M: &str = "vehicle.position_world_m";
    /// Latitude and longitude in degrees, altitude in meters (MSFS).
    pub const POSITION_GEO: &str = "vehicle.position_geo";
    /// Indicated airspeed (MSFS).
    pub const AIRSPEED_MPS: &str = "vehicle.airspeed_mps";
    pub const ON_GROUND: &str = "vehicle.on_ground";
    pub const PARKING_BRAKE: &str = "vehicle.parking_brake";
    pub const ENGINE_RUNNING: &str = "engine.running";
    pub const ENGINE_RPM: &str = "engine.rpm";
    pub const ENGINE_GEAR: &str = "engine.gear";
    /// Speed limit of the current road from the game's navigation (ATS); 0 when none.
    pub const NAV_SPEED_LIMIT_MPS: &str = "nav.speed_limit_mps";
    pub const NAV_DISTANCE_M: &str = "nav.distance_m";
    pub const PAUSED: &str = "sim.paused";
}

/// What a feature declares in feature.json; the host refuses a call outside the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Game values, read only: the neutral names, any native value by its game name
    /// (Host::subscribe) and the attributes of game events.
    VarsRead,
    /// Mapped game-memory reads in addition to vars_read; no raw addresses reach the feature.
    VarsMemory,
    /// Reversible mapped writes. Also needs vars_memory and a qualified host execution point.
    VarsWrite,
    /// A host-reviewed color effect over the game image (version 5). No GPU objects reach the
    /// feature; stop and app disconnect remove the pass.
    RenderPass,
    /// The game's depth (distance per pixel) for a host-reviewed effect that needs it, such as
    /// RenderEffect::Mirage (version 5). Needs render_pass. The depth never reaches the feature.
    RenderDepth,
    /// The feature's own pixel shaders, compiled into its package (version 5, Host::shader_acquire).
    /// Needs render_pass; the scene depth stage also needs render_depth. No GPU object or game
    /// image reaches the feature; stop and app disconnect remove its passes.
    RenderShader,
    /// Inputs into the game (ATS input devices, MSFS events).
    InputInject,
    /// A page in the app and messages to it.
    UiApp,
    /// Named controls in the feature's "controls" list (version 5). The member binds each one
    /// to a key, mouse or joystick input in the app; the feature gets only its own controls
    /// (Feature::control), and only while the game window is in front.
    Controls,
    /// Files in the game's mod or Community folder.
    GameFiles,
    /// The game engine commands in the feature's "game_commands" list (version 2, ATS).
    GameCommand,
    /// HTTPS GET requests to the hosts in the feature's "network_hosts" list (version 3). The
    /// app does the fetch and checks the host again against the installed manifest.
    Network,
    /// The game files in the feature's "user_files" list, read or changed through UserFiles
    /// (version 5): files the game keeps in its user folder (ATS: game.log.txt, profiles).
    UserFiles,
    /// Direct powers (submission review plan R1, R2): the feature calls the operating system
    /// itself, not an SDK service, so no host can limit the call. The review checks the shipped
    /// code end to end, and consent names each power. They grant nothing in the hosts; an older
    /// app reads them as Unknown and refuses the feature.
    /// Files on the PC outside its data folder, without the user_files service.
    DirectFiles,
    /// Other processes: list them, read them or control them.
    DirectProcesses,
    /// The Windows registry.
    DirectRegistry,
    /// Network connections that the feature makes itself, not through Host::http_get.
    DirectNetwork,
    /// Other code at run time: a library that it loads (from its package or the PC) or a
    /// program that it starts.
    LoadCode,
    /// Any name that this build does not know (newer, or removed like "keys_read"). It makes
    /// only that feature fail: the manifest check and the host refuse it, while the other
    /// features of the same catalog or active.json still load (an unknown name in a strict
    /// enum failed the whole document). It grants nothing.
    #[serde(other)]
    Unknown,
}

/// Review result for multiplayer (user decision 12); until the review service exists, the
/// package states it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Multiplayer {
    /// No effect on multiplayer.
    #[default]
    Allowed,
    /// Runs, and the member is told about the risk first.
    Risk,
    /// Not loaded while a multiplayer client runs.
    Blocked,
}

/// One neutral value, as a feature reads it. Untagged in JSON (number, bool, array of 3), so the
/// host wire carries frames with no extra type names.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    F64(f64),
    Bool(bool),
    Vec3([f64; 3]),
}

impl Value {
    /// The ABI form; `name` must stay alive while the frame is in use.
    pub fn to_abi(self, name: &str) -> abi::AbiValue {
        let (kind, v) = match self {
            Value::F64(x) => (abi::KIND_F64, [x, 0.0, 0.0]),
            Value::Bool(b) => (abi::KIND_BOOL, [if b { 1.0 } else { 0.0 }, 0.0, 0.0]),
            Value::Vec3(v) => (abi::KIND_VEC3, v),
        };
        abi::AbiValue { name: name.as_ptr(), name_len: name.len(), kind, v }
    }

    fn from_abi(a: &abi::AbiValue) -> Option<Value> {
        match a.kind {
            abi::KIND_F64 => Some(Value::F64(a.v[0])),
            abi::KIND_BOOL => Some(Value::Bool(a.v[0] != 0.0)),
            abi::KIND_VEC3 => Some(Value::Vec3(a.v)),
            _ => None,
        }
    }
}

/// One native game value or event attribute (API 4), in the game's own type: ATS u32, s64,
/// placement or string channels, MSFS SimVars in their unit (unit "bool" gives Bool, "string"
/// gives Text). Placement: x, y, z, then heading, pitch, roll in degrees.
#[derive(Clone, Debug, PartialEq)]
pub enum Var {
    F64(f64),
    Bool(bool),
    I64(i64),
    U64(u64),
    Vec3([f64; 3]),
    Placement([f64; 6]),
    Text(String),
}

impl Var {
    /// The ABI form; `name` and the text must stay alive while it is in use.
    pub fn to_abi(&self, name: &str) -> abi::AbiVar {
        let mut a = abi::AbiVar { name: name.as_ptr(), name_len: name.len(), kind: abi::KIND_F64, num: [0.0; 6], int: 0, text: std::ptr::null(), text_len: 0 };
        match self {
            Var::F64(x) => a.num[0] = *x,
            Var::Bool(b) => (a.kind, a.num[0]) = (abi::KIND_BOOL, if *b { 1.0 } else { 0.0 }),
            Var::I64(i) => (a.kind, a.int) = (abi::KIND_I64, *i),
            Var::U64(u) => (a.kind, a.int) = (abi::KIND_U64, *u as i64),
            Var::Vec3(v) => {
                a.kind = abi::KIND_VEC3;
                a.num[..3].copy_from_slice(v);
            }
            Var::Placement(p) => (a.kind, a.num) = (abi::KIND_PLACEMENT, *p),
            Var::Text(t) => (a.kind, a.text, a.text_len) = (abi::KIND_TEXT, t.as_ptr(), t.len()),
        }
        a
    }

    /// The name and value; None for a kind that this SDK does not know (a newer host).
    ///
    /// # Safety
    /// The name and text pointers of `a` must be valid (the host guarantees it during the call).
    pub unsafe fn from_abi(a: &abi::AbiVar) -> Option<(&str, Var)> {
        let name = abi::text(a.name, a.name_len)?;
        let v = match a.kind {
            abi::KIND_F64 => Var::F64(a.num[0]),
            abi::KIND_BOOL => Var::Bool(a.num[0] != 0.0),
            abi::KIND_I64 => Var::I64(a.int),
            abi::KIND_U64 => Var::U64(a.int as u64),
            abi::KIND_VEC3 => Var::Vec3([a.num[0], a.num[1], a.num[2]]),
            abi::KIND_PLACEMENT => Var::Placement(a.num),
            abi::KIND_TEXT => Var::Text(abi::text(a.text, a.text_len)?.to_string()),
            _ => return None,
        };
        Some((name, v))
    }

    /// The number of an F64, Bool (0 or 1), I64 or U64 value.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Var::F64(x) => Some(*x),
            Var::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Var::I64(i) => Some(*i as f64),
            Var::U64(u) => Some(*u as f64),
            _ => None,
        }
    }
}

/// The value of a feature control (Feature::control). Wire form, as the input reader and the
/// hosts send it: `"kind":"button","value":true`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Control {
    /// Down (true) or up.
    Button(bool),
    /// -1..1, or 0..1 for a binding with half range (a trigger, a throttle).
    Axis(f64),
    /// Relative travel since the last event: mouse counts or wheel notches.
    Delta(f64),
}

impl Control {
    /// The event attribute that carries this value (abi::CONTROL_EVENT).
    pub fn to_attr(self) -> (&'static str, Var) {
        match self {
            Control::Button(b) => (abi::CONTROL_STATE, Var::Bool(b)),
            Control::Axis(v) => (abi::CONTROL_VALUE, Var::F64(v)),
            Control::Delta(v) => (abi::CONTROL_DELTA, Var::F64(v)),
        }
    }

    fn from_attrs(attrs: &[(&str, Var)]) -> Option<Control> {
        attrs.iter().find_map(|(n, v)| match (*n, v) {
            (abi::CONTROL_STATE, Var::Bool(b)) => Some(Control::Button(*b)),
            (abi::CONTROL_VALUE, Var::F64(x)) => Some(Control::Axis(*x)),
            (abi::CONTROL_DELTA, Var::F64(x)) => Some(Control::Delta(*x)),
            _ => None,
        })
    }
}

/// A game event with its attributes, for Feature::game_event. Attributes have the game's own
/// names (ATS: "revenue", "cargo"; an indexed one as "name[2]"). Host events: "var.ready" and
/// "var.unknown" with "name" (the subscribe key) and, for unknown, "reason".
#[derive(Clone, Debug, PartialEq)]
pub struct GameEvent<'a> {
    pub id: &'a str,
    pub attrs: Vec<(&'a str, Var)>,
}

impl GameEvent<'_> {
    pub fn attr(&self, name: &str) -> Option<&Var> {
        self.attrs.iter().find(|(n, _)| *n == name).map(|(_, v)| v)
    }
}

/// The values of one game frame, valid during Feature::frame.
pub struct Frame<'a> {
    pub seq: u64,
    pub time_us: u64,
    values: &'a [abi::AbiValue],
    vars: &'a [abi::AbiVar],
}

impl<'a> Frame<'a> {
    /// # Safety
    /// `f` and its values must be valid for 'a (the host guarantees it during on_frame).
    pub unsafe fn from_abi(f: &'a abi::AbiFrame) -> Self {
        let values = if f.values.is_null() || f.len == 0 { &[][..] } else { std::slice::from_raw_parts(f.values, f.len) };
        let vars = if f.vars.is_null() || f.vars_len == 0 { &[][..] } else { std::slice::from_raw_parts(f.vars, f.vars_len) };
        Frame { seq: f.seq, time_us: f.time_us, values, vars }
    }

    /// A native value that Host::subscribe asked for, by the same key ("ats:truck.fuel.amount").
    /// None while the game has no value for it (no job, no trailer) or before the first frame.
    pub fn var(&self, key: &str) -> Option<Var> {
        self.vars.iter().find_map(|a| unsafe { Var::from_abi(a) }.filter(|(n, _)| *n == key).map(|(_, v)| v))
    }

    pub fn get(&self, name: &str) -> Option<Value> {
        self.values.iter().find(|v| unsafe { abi::text(v.name, v.name_len) } == Some(name)).and_then(Value::from_abi)
    }

    pub fn f64(&self, name: &str) -> Option<f64> {
        match self.get(name)? {
            Value::F64(x) => Some(x),
            _ => None,
        }
    }

    pub fn bool(&self, name: &str) -> Option<bool> {
        match self.get(name)? {
            Value::Bool(b) => Some(b),
            _ => None,
        }
    }

    pub fn vec3(&self, name: &str) -> Option<[f64; 3]> {
        match self.get(name)? {
            Value::Vec3(v) => Some(v),
            _ => None,
        }
    }
}

/// A feature control id (Capability::Controls): 1 to 64 characters a-z, 0-9, '.', '_' or '-',
/// like a page topic. The manifest check, the app and the hosts use this one rule.
pub fn valid_control_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b".-_".contains(&b))
}

/// The most hosts that a feature can declare in network_hosts.
pub const MAX_NETWORK_HOSTS: usize = 8;

/// The page message topic of Host::settings_import. Topics with the "fm:" prefix are the app's:
/// the app takes this one and never gives an "fm:" topic of a feature to a page or a widget.
pub const SETTINGS_IMPORT_TOPIC: &str = "fm:settings_import";

/// A host name for network_hosts: a lower-case DNS name with two labels or more (no scheme,
/// port, wildcard, IP address or "localhost"), so the consent line names one exact server
/// group. One rule for the manifest check (fm_install) and the hosts.
pub fn valid_host(name: &str) -> bool {
    let label = |l: &&str| (1..=63).contains(&l.len()) && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !l.starts_with('-') && !l.ends_with('-');
    let labels: Vec<&str> = name.split('.').collect();
    name.len() <= 253 && labels.len() >= 2 && labels.iter().all(label) && labels.last().is_some_and(|tld| tld.bytes().any(|b| b.is_ascii_lowercase()))
}

/// Why a host call failed. Serde names are the wire names of an app answer (fm_ats_wire).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostError {
    /// The feature did not declare the capability, or the rule refuses the call (for example a
    /// host that is not in network_hosts).
    Refused,
    BadArgs,
    Failed,
    /// Version 3. Too many requests wait, or the app rate limit: try again later.
    Busy,
    /// Version 3. No answer in time.
    Timeout,
}

impl HostError {
    /// The ABI code (always negative).
    pub fn code(self) -> i32 {
        match self {
            HostError::Refused => abi::REFUSED,
            HostError::BadArgs => abi::BAD_ARGS,
            HostError::Failed => abi::FAILED,
            HostError::Busy => abi::BUSY,
            HostError::Timeout => abi::TIMEOUT,
        }
    }

    /// The error of a negative ABI code; an unknown code is Failed.
    pub fn from_code(code: i64) -> HostError {
        match code {
            c if c == abi::REFUSED as i64 => HostError::Refused,
            c if c == abi::BAD_ARGS as i64 => HostError::BadArgs,
            c if c == abi::BUSY as i64 => HostError::Busy,
            c if c == abi::TIMEOUT as i64 => HostError::Timeout,
            _ => HostError::Failed,
        }
    }
}

fn result(code: i32) -> Result<(), HostError> {
    if code == abi::OK {
        Ok(())
    } else {
        Err(HostError::from_code(code as i64))
    }
}

/// The answer of a server to Host::http_get: any HTTP status (also 404 or 500) with its body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpAnswer {
    pub status: u16,
    pub body: Vec<u8>,
}

/// What kind of thing a declared game file is now (UserFiles::file_info).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    None,
    File,
    Folder,
}

/// One file_info answer. `modified`: None when the OS does not give it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileInfo {
    pub kind: FileKind,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

/// The declared game files (capability user_files): the same calls in the game (Host) and in a
/// lifecycle hook (LifecycleHost), and on a test folder (testing::DirFiles). A key is
/// "<game>:<path>", for example "ats:profiles/52756E/config.cfg"; the host refuses a key that
/// the feature's user_files list does not match (Refused) and a bad key (BadArgs).
pub trait UserFiles {
    fn file_info(&self, key: &str) -> Result<FileInfo, HostError>;
    /// At most `max` bytes (and at most files::MAX_BYTES) from `offset`; empty at the end.
    fn file_read(&self, key: &str, offset: u64, max: usize) -> Result<Vec<u8>, HostError>;
    /// Replaces the file atomically (access write). The folder must exist.
    fn file_write(&self, key: &str, bytes: &[u8]) -> Result<(), HostError>;
}

/// The three ABI calls and their ctx: one Rust side for HostApi and LifecycleCall.
#[derive(Clone, Copy)]
struct FileCalls {
    ctx: *mut std::ffi::c_void,
    info: abi::FileInfoFn,
    read: abi::FileReadFn,
    write: abi::FileWriteFn,
}

impl UserFiles for FileCalls {
    fn file_info(&self, key: &str) -> Result<FileInfo, HostError> {
        let mut out = abi::AbiFileInfo::default();
        result(unsafe { (self.info)(self.ctx, key.as_ptr(), key.len(), &mut out) })?;
        let kind = match out.kind {
            abi::FILE_NONE => FileKind::None,
            abi::FILE_FILE => FileKind::File,
            abi::FILE_FOLDER => FileKind::Folder,
            _ => return Err(HostError::Failed),
        };
        let modified = (out.modified_us > 0).then(|| UNIX_EPOCH + Duration::from_micros(out.modified_us));
        Ok(FileInfo { kind, size: out.size, modified })
    }

    fn file_read(&self, key: &str, offset: u64, max: usize) -> Result<Vec<u8>, HostError> {
        let mut buf = vec![0u8; max.min(files::MAX_BYTES)];
        let n = unsafe { (self.read)(self.ctx, key.as_ptr(), key.len(), offset, buf.as_mut_ptr(), buf.len()) };
        if n < 0 {
            return Err(HostError::from_code(n));
        }
        buf.truncate((n as usize).min(buf.len()));
        Ok(buf)
    }

    fn file_write(&self, key: &str, bytes: &[u8]) -> Result<(), HostError> {
        result(unsafe { (self.write)(self.ctx, key.as_ptr(), key.len(), bytes.as_ptr(), bytes.len()) })
    }
}

/// The feature's settings (feature.json "settings", API 6): the member's values for this
/// version by setting id, and their revision. A typed read gives None for a missing id or another
/// type, so the feature keeps its own default. Host::settings has the values at start;
/// Feature::settings gets each change.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    revision: u64,
    values: serde_json::Map<String, serde_json::Value>,
}

impl Settings {
    /// The values of a JSON object; other text gives no values.
    pub fn from_json(revision: u64, json: &str) -> Settings {
        let values = match serde_json::from_str(json) { Ok(serde_json::Value::Object(m)) => m, _ => Default::default() };
        Settings { revision, values }
    }
    pub fn revision(&self) -> u64 { self.revision }
    pub fn get(&self, id: &str) -> Option<&serde_json::Value> { self.values.get(id) }
    /// A toggle.
    pub fn bool(&self, id: &str) -> Option<bool> { self.get(id)?.as_bool() }
    /// A range or a number.
    pub fn f64(&self, id: &str) -> Option<f64> { self.get(id)?.as_f64() }
    /// A select value, a text or a color ("#rrggbb").
    pub fn str(&self, id: &str) -> Option<&str> { self.get(id)?.as_str() }
}

/// The feature's handle to its host.
pub struct Host {
    api: abi::HostApi,
}

/// An opaque, host-owned writer. It is valid only for the acquiring feature and game session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteHandle(u64);

/// An opaque, host-owned color pass. It is valid only for the acquiring feature and game session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderHandle(u64);

/// Effects of the host-reviewed pass. Mirage (heat shimmer over hot ground, drawn on the game's
/// scene before the UI) needs the render_depth capability; the others work on the final image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderEffect { Identity, Tint, Grayscale, Mirage(MirageLook) }

/// The looks of the mirage that the host provides (it owns their shader values). Physical: what
/// the eye sees at a low angle, a band of a few pixels. Strong: an exaggerated, wide wet band.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MirageLook { Physical, Strong }

/// Where a feature shader runs (Host::shader_acquire). Final: the presented image with the game
/// UI. Scene: the game's HDR scene before tone mapping and UI. SceneDepth: the scene with the
/// depth in fm_depth (capability render_depth).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShaderStage { Final, Scene, SceneDepth }

impl ShaderStage {
    pub fn to_abi(self) -> u32 {
        match self { ShaderStage::Final => abi::SHADER_FINAL, ShaderStage::Scene => abi::SHADER_SCENE, ShaderStage::SceneDepth => abi::SHADER_SCENE_DEPTH }
    }
    pub fn from_abi(v: u32) -> Option<Self> {
        match v { abi::SHADER_FINAL => Some(ShaderStage::Final), abi::SHADER_SCENE => Some(ShaderStage::Scene), abi::SHADER_SCENE_DEPTH => Some(ShaderStage::SceneDepth), _ => None }
    }
}

/// A valid shader name: 1 to abi::SHADER_NAME_MAX characters a-z, 0-9, '_' or '-'. The name is
/// a file name in the package (shaders/<name>.cso), so no path can leave that folder.
pub fn shader_name_valid(name: &str) -> bool {
    (1..=abi::SHADER_NAME_MAX).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Pass parameters: `amount` 0 to 1 blends from the original image to the effect (for Mirage,
/// its intensity); `tint` multiplies each channel (0 to 1) for RenderEffect::Tint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderParams { pub effect: RenderEffect, pub amount: f32, pub tint: [f32; 3] }

impl RenderParams {
    pub fn to_abi(self) -> abi::AbiRender {
        let effect = match self.effect {
            RenderEffect::Identity => abi::RENDER_IDENTITY, RenderEffect::Tint => abi::RENDER_TINT,
            RenderEffect::Grayscale => abi::RENDER_GRAYSCALE, RenderEffect::Mirage(MirageLook::Physical) => abi::RENDER_MIRAGE,
            RenderEffect::Mirage(MirageLook::Strong) => abi::RENDER_MIRAGE_STRONG,
        };
        abi::AbiRender { effect, amount: self.amount, tint: self.tint }
    }

    /// The checked parameters of an ABI value, or None when a field is out of range.
    pub fn from_abi(p: &abi::AbiRender) -> Option<Self> {
        let effect = match p.effect {
            abi::RENDER_IDENTITY => RenderEffect::Identity, abi::RENDER_TINT => RenderEffect::Tint,
            abi::RENDER_GRAYSCALE => RenderEffect::Grayscale, abi::RENDER_MIRAGE => RenderEffect::Mirage(MirageLook::Physical),
            abi::RENDER_MIRAGE_STRONG => RenderEffect::Mirage(MirageLook::Strong), _ => return None,
        };
        let unit = |v: f32| (0.0..=1.0).contains(&v);
        (unit(p.amount) && p.tint.iter().all(|t| unit(*t))).then_some(Self { effect, amount: p.amount, tint: p.tint })
    }
}

impl Host {
    /// # Safety
    /// `api` must come from the host in fm_feature_start; it stays valid until stop.
    pub unsafe fn from_abi(api: *const abi::HostApi) -> Option<Host> {
        api.as_ref().filter(|a| a.api_version == API_VERSION).map(|a| Host { api: *a })
    }

    /// "ats" or "msfs2024".
    pub fn game(&self) -> &str {
        unsafe { abi::text(self.api.game, self.api.game_len) }.unwrap_or("")
    }

    /// A message for the feature's page in the app (capability ui_app).
    pub fn ui<T: Serialize>(&self, topic: &str, message: &T) -> Result<(), HostError> {
        let json = serde_json::to_string(message).map_err(|_| HostError::BadArgs)?;
        result(unsafe { (self.api.ui_send)(self.api.ctx, topic.as_ptr(), topic.len(), json.as_ptr(), json.len()) })
    }

    /// Press an input for `ms` milliseconds (capability input_inject).
    pub fn press(&self, input: &str, ms: u32) -> Result<(), HostError> {
        result(unsafe { (self.api.input)(self.api.ctx, input.as_ptr(), input.len(), abi::INPUT_PRESS, 0.0, ms) })
    }

    /// Set an analog input (capability input_inject).
    pub fn axis(&self, input: &str, value: f32) -> Result<(), HostError> {
        result(unsafe { (self.api.input)(self.api.ctx, input.as_ptr(), input.len(), abi::INPUT_AXIS, value, 0) })
    }

    /// Read a native game value by the game's own name (capability vars_read), for example
    /// "ats:truck.fuel.amount" or "msfs2024:FUEL TOTAL QUANTITY,gallons". Frame::var gives the
    /// value; the event "var.ready" or "var.unknown" (attribute "name") tells if the game has it.
    pub fn subscribe(&self, key: &str) -> Result<(), HostError> {
        result(unsafe { (self.api.subscribe)(self.api.ctx, key.as_ptr(), key.len()) })
    }

    pub fn unsubscribe(&self, key: &str) -> Result<(), HostError> {
        result(unsafe { (self.api.unsubscribe)(self.api.ctx, key.as_ptr(), key.len()) })
    }

    /// Acquire an exclusive writer for mem:<map-id>/<symbol>. Stop always revokes the writer.
    pub fn memory_acquire(&self, key: &str) -> Result<WriteHandle, HostError> {
        let n = unsafe { (self.api.memory_acquire)(self.api.ctx, key.as_ptr(), key.len()) };
        if n > 0 { Ok(WriteHandle(n as u64)) } else { Err(HostError::from_code(n)) }
    }

    /// Set a mapped scalar. The map checks its exact type and range; an unsupported type fails.
    pub fn memory_set(&self, handle: WriteHandle, value: Var) -> Result<(), HostError> {
        let value = value.to_abi("");
        result(unsafe { (self.api.memory_set)(self.api.ctx, handle.0, &value) })
    }

    pub fn memory_release(&self, handle: WriteHandle) -> Result<(), HostError> {
        result(unsafe { (self.api.memory_release)(self.api.ctx, handle.0) })
    }

    /// Acquire the color pass (capability render_pass). BUSY: another feature owns it.
    pub fn render_acquire(&self, params: RenderParams) -> Result<RenderHandle, HostError> {
        let n = unsafe { (self.api.render_acquire)(self.api.ctx, &params.to_abi()) };
        if n > 0 { Ok(RenderHandle(n as u64)) } else { Err(HostError::from_code(n)) }
    }

    pub fn render_set(&self, handle: RenderHandle, params: RenderParams) -> Result<(), HostError> {
        result(unsafe { (self.api.render_set)(self.api.ctx, handle.0, &params.to_abi()) })
    }

    /// Remove the pass (a color pass or a shader pass). Stop removes it too when a feature does not.
    pub fn render_release(&self, handle: RenderHandle) -> Result<(), HostError> {
        result(unsafe { (self.api.render_release)(self.api.ctx, handle.0) })
    }

    /// Run the package shader shaders/<name>.cso (capability render_shader) at `stage` with its
    /// 32 parameters (fm_params). Each call adds one pass; render_release removes it. BAD_ARGS:
    /// a bad name or value, or no such shader in the package. FAILED: the GPU refused the shader.
    pub fn shader_acquire(&self, stage: ShaderStage, name: &str, params: &[f32; abi::SHADER_PARAMS]) -> Result<RenderHandle, HostError> {
        let p = abi::AbiShader { stage: stage.to_abi(), name: name.as_ptr(), name_len: name.len(), params: *params };
        let n = unsafe { (self.api.shader_acquire)(self.api.ctx, &p) };
        if n > 0 { Ok(RenderHandle(n as u64)) } else { Err(HostError::from_code(n)) }
    }

    /// New parameters for a held shader pass, and a swap to another package shader when `name`
    /// is Some (the pass keeps its stage and its place in the order).
    pub fn shader_set(&self, handle: RenderHandle, name: Option<&str>, params: &[f32; abi::SHADER_PARAMS]) -> Result<(), HostError> {
        let name = name.unwrap_or("");
        let p = abi::AbiShader { stage: 0, name: name.as_ptr(), name_len: name.len(), params: *params };
        result(unsafe { (self.api.shader_set)(self.api.ctx, handle.0, &p) })
    }

    /// A sim event by its native name with one data value (capability input_inject), for example
    /// ("HEADING_BUG_SET", 270) in MSFS. Sent once; the host never reverses it.
    pub fn send_event(&self, name: &str, data: i32) -> Result<(), HostError> {
        result(unsafe { (self.api.send_event)(self.api.ctx, name.as_ptr(), name.len(), data) })
    }

    pub fn log(&self, text: &str) {
        unsafe { (self.api.log)(self.api.ctx, text.as_ptr(), text.len()) }
    }

    /// A game engine command (capability game_command), for example "g_set_weather 1 i". The
    /// host runs it on the game thread soon after; Ok means it is queued.
    pub fn command(&self, text: &str) -> Result<(), HostError> {
        result(unsafe { (self.api.game_command)(self.api.ctx, text.as_ptr(), text.len()) })
    }

    /// An HTTPS GET (capability network), for example
    /// "https://tgftp.nws.noaa.gov/data/observations/metar/stations/KCYS.TXT". The host must be in
    /// network_hosts; no port, user name or password. Returns the request id at once; the answer
    /// comes later in Feature::http with this id (at the latest after 60 s, as Timeout).
    pub fn http_get(&self, url: &str) -> Result<u64, HostError> {
        let r = unsafe { (self.api.http_get)(self.api.ctx, url.as_ptr(), url.len()) };
        if r > 0 {
            Ok(r as u64)
        } else {
            Err(HostError::from_code(r))
        }
    }

    /// The feature's own data folder, the same one that its lifecycle hooks get (for example a
    /// journal that a hook reads after the game ended). It stays over updates and goes at
    /// uninstall. None when the host has none (an older app). The folder may not exist yet.
    pub fn data_dir(&self) -> Option<PathBuf> {
        unsafe { abi::text(self.api.data_dir, self.api.data_dir_len) }.filter(|p| !p.is_empty()).map(PathBuf::from)
    }

    /// The settings at this start (API 6). Later changes come to Feature::settings.
    pub fn settings(&self) -> Settings {
        Settings::from_json(self.api.settings_revision, unsafe { abi::text(self.api.settings, self.api.settings_len) }.unwrap_or("{}"))
    }

    /// Offers the values that an older version of the feature saved on its own (a JSON object
    /// by setting id), so a member keeps them after the move to settings (API 6, capability
    /// ui_app). The app takes a value only for a setting that the member never changed and that
    /// no import filled, and only when it fits; a taken value comes back to Feature::settings.
    /// It is safe to offer at each start: the app takes each id once.
    pub fn settings_import<T: Serialize>(&self, values: &T) -> Result<(), HostError> {
        self.ui(SETTINGS_IMPORT_TOPIC, values)
    }

    /// When the game process started; None when the host does not know it.
    pub fn game_started(&self) -> Option<SystemTime> {
        (self.api.game_started_us > 0).then(|| UNIX_EPOCH + Duration::from_micros(self.api.game_started_us))
    }

    fn files(&self) -> FileCalls {
        FileCalls { ctx: self.api.ctx, info: self.api.file_info, read: self.api.file_read, write: self.api.file_write }
    }
}

impl UserFiles for Host {
    fn file_info(&self, key: &str) -> Result<FileInfo, HostError> {
        self.files().file_info(key)
    }
    fn file_read(&self, key: &str, offset: u64, max: usize) -> Result<Vec<u8>, HostError> {
        self.files().file_read(key, offset, max)
    }
    fn file_write(&self, key: &str, bytes: &[u8]) -> Result<(), HostError> {
        self.files().file_write(key, bytes)
    }
}

/// A lifecycle point outside the game (abi.rs LIFECYCLE_*), for Feature::lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifecycle<'a> {
    Installed,
    Updated { from_version: &'a str },
    Uninstalling,
    GameExited { game: &'a str },
}

/// What a lifecycle hook gets from Feature Machine: the data folder, a log, the declared game
/// files (UserFiles) and the game-running check.
pub struct LifecycleHost {
    call: abi::LifecycleCall,
}

impl LifecycleHost {
    /// The feature's own data folder (Host::data_dir in the game). It may not exist yet.
    pub fn data_dir(&self) -> PathBuf {
        PathBuf::from(unsafe { abi::text(self.call.data_dir, self.call.data_dir_len) }.unwrap_or(""))
    }

    pub fn log(&self, text: &str) {
        unsafe { (self.call.log)(self.call.ctx, text.as_ptr(), text.len()) }
    }

    /// True while a process of `game` ("ats") runs on this computer. A game can start at any
    /// time, so check it just before work on files that the game saves.
    pub fn game_running(&self, game: &str) -> Result<bool, HostError> {
        match unsafe { (self.call.game_running)(self.call.ctx, game.as_ptr(), game.len()) } {
            1 => Ok(true),
            0 => Ok(false),
            c => Err(HostError::from_code(c as i64)),
        }
    }

    fn files(&self) -> FileCalls {
        FileCalls { ctx: self.call.ctx, info: self.call.file_info, read: self.call.file_read, write: self.call.file_write }
    }
}

impl UserFiles for LifecycleHost {
    fn file_info(&self, key: &str) -> Result<FileInfo, HostError> {
        self.files().file_info(key)
    }
    fn file_read(&self, key: &str, offset: u64, max: usize) -> Result<Vec<u8>, HostError> {
        self.files().file_read(key, offset, max)
    }
    fn file_write(&self, key: &str, bytes: &[u8]) -> Result<(), HostError> {
        self.files().file_write(key, bytes)
    }
}

/// A feature. The host calls frame on its feature thread (never the game thread), about 20
/// times a second, and stop once at the end. game_event gets game events with their attributes;
/// its default gives the id to event ("paused", "started", ...). http gets the answer to each
/// Host::http_get, on the same thread. lifecycle runs with no game and no instance, in a helper
/// process (abi.rs SYM_LIFECYCLE); the default does nothing.
pub trait Feature: Sized + 'static {
    fn start(host: Host) -> Self;
    fn frame(&mut self, frame: &Frame);
    fn event(&mut self, _id: &str) {}
    fn game_event(&mut self, event: &GameEvent) {
        self.event(event.id)
    }
    fn http(&mut self, _id: u64, _answer: Result<HttpAnswer, HostError>) {}
    /// A message from the feature's own page in the app (capability ui_app): the topic and the
    /// JSON text that the page sent. The page can send any text, so check it like user input.
    fn page(&mut self, _topic: &str, _data: &str) {}
    /// A control of the feature's "controls" list changed (capability controls): `id` is the
    /// control id. A held button always gets its "up" (focus loss, device loss, app exit, and
    /// before stop).
    fn control(&mut self, _id: &str, _value: Control) {}
    /// The member changed the settings (API 6): all values of this version, with the new
    /// revision. Only the app sends it, after its check against the version's schema.
    fn settings(&mut self, _settings: &Settings) {}
    fn stop(&mut self) {}
    /// Err: the reason, which goes to the hook log; Feature Machine goes on (a removal still
    /// removes). Keep it short: the helper ends a hook after 30 s.
    fn lifecycle(_event: Lifecycle, _host: &LifecycleHost) -> Result<(), String> {
        Ok(())
    }
}

/// Glue for feature!: each call catches a panic, so it never crosses into the host.
#[doc(hidden)]
pub mod glue {
    use super::*;
    use std::ffi::c_void;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    pub unsafe fn start<F: Feature>(host: *const abi::HostApi, out: *mut abi::FeatureApi) -> i32 {
        let Some(h) = Host::from_abi(host) else { return abi::BAD_ARGS };
        let logger = Host { api: h.api };
        match catch_unwind(AssertUnwindSafe(|| F::start(h))) {
            Ok(f) => {
                if out.is_null() {
                    return abi::BAD_ARGS;
                }
                *out = abi::FeatureApi {
                    api_version: API_VERSION,
                    instance: Box::into_raw(Box::new(f)) as *mut c_void,
                    on_frame: Some(on_frame::<F>),
                    on_event: Some(on_event::<F>),
                    stop: Some(stop::<F>),
                    on_http: Some(on_http::<F>),
                    on_game_event: Some(on_game_event::<F>),
                };
                abi::OK
            }
            Err(_) => {
                logger.log("panic in start");
                abi::FAILED
            }
        }
    }

    unsafe extern "C" fn on_frame<F: Feature>(instance: *mut c_void, frame: *const abi::AbiFrame) -> i32 {
        let (Some(f), Some(fr)) = ((instance as *mut F).as_mut(), frame.as_ref()) else { return abi::BAD_ARGS };
        let frame = Frame::from_abi(fr);
        catch_unwind(AssertUnwindSafe(|| f.frame(&frame))).map_or(abi::FAILED, |_| abi::OK)
    }

    unsafe extern "C" fn on_event<F: Feature>(instance: *mut c_void, id: *const u8, len: usize) -> i32 {
        let (Some(f), Some(id)) = ((instance as *mut F).as_mut(), abi::text(id, len)) else { return abi::BAD_ARGS };
        catch_unwind(AssertUnwindSafe(|| f.event(id))).map_or(abi::FAILED, |_| abi::OK)
    }

    /// An attribute that this SDK can not read (a newer kind) is left out, not an error.
    unsafe extern "C" fn on_game_event<F: Feature>(instance: *mut c_void, event: *const abi::AbiEvent) -> i32 {
        let (Some(f), Some(e)) = ((instance as *mut F).as_mut(), event.as_ref()) else { return abi::BAD_ARGS };
        let Some(id) = abi::text(e.id, e.id_len) else { return abi::BAD_ARGS };
        let raw = if e.attrs.is_null() || e.attrs_len == 0 { &[][..] } else { std::slice::from_raw_parts(e.attrs, e.attrs_len) };
        let attrs: Vec<(&str, Var)> = raw.iter().filter_map(|a| Var::from_abi(a)).collect();
        // Settings ride on the event call too (abi::SETTINGS_EVENT, API 6).
        if id == abi::SETTINGS_EVENT {
            let data = attrs.iter().find_map(|(n, v)| match v { Var::Text(t) if *n == abi::SETTINGS_DATA => Some(t.as_str()), _ => None }).unwrap_or("{}");
            let revision = attrs.iter().find_map(|(n, v)| match v { Var::U64(r) if *n == abi::SETTINGS_REVISION => Some(*r), _ => None }).unwrap_or(0);
            let s = Settings::from_json(revision, data);
            return catch_unwind(AssertUnwindSafe(|| f.settings(&s))).map_or(abi::FAILED, |_| abi::OK);
        }
        // Page messages ride on the event call (abi::PAGE_EVENT), so the table needs no new field.
        if let Some(topic) = id.strip_prefix(abi::PAGE_EVENT) {
            let data = attrs.iter().find_map(|(n, v)| match v { Var::Text(t) if *n == abi::PAGE_DATA => Some(t.as_str()), _ => None }).unwrap_or("null");
            return catch_unwind(AssertUnwindSafe(|| f.page(topic, data))).map_or(abi::FAILED, |_| abi::OK);
        }
        // Control events ride on the event call too (abi::CONTROL_EVENT).
        if let Some(control) = id.strip_prefix(abi::CONTROL_EVENT) {
            let Some(value) = Control::from_attrs(&attrs) else { return abi::BAD_ARGS };
            return catch_unwind(AssertUnwindSafe(|| f.control(control, value))).map_or(abi::FAILED, |_| abi::OK);
        }
        catch_unwind(AssertUnwindSafe(|| f.game_event(&GameEvent { id, attrs }))).map_or(abi::FAILED, |_| abi::OK)
    }

    unsafe extern "C" fn on_http<F: Feature>(instance: *mut c_void, id: u64, status: i32, body: *const u8, len: usize) -> i32 {
        let Some(f) = (instance as *mut F).as_mut() else { return abi::BAD_ARGS };
        let answer = match status {
            1..=999 if len == 0 || !body.is_null() => {
                let body = if len == 0 { vec![] } else { std::slice::from_raw_parts(body, len).to_vec() };
                Ok(HttpAnswer { status: status as u16, body })
            }
            s if s < 0 => Err(HostError::from_code(s as i64)),
            _ => return abi::BAD_ARGS,
        };
        catch_unwind(AssertUnwindSafe(|| f.http(id, answer))).map_or(abi::FAILED, |_| abi::OK)
    }

    unsafe extern "C" fn stop<F: Feature>(instance: *mut c_void) {
        if instance.is_null() {
            return;
        }
        let mut f = Box::from_raw(instance as *mut F);
        let _ = catch_unwind(AssertUnwindSafe(|| f.stop()));
        let _ = catch_unwind(AssertUnwindSafe(move || drop(f)));
    }

    /// fm_feature_lifecycle: checks the call, maps the event and catches a panic. An unknown
    /// event (a newer helper) is OK and does nothing, so a new event never fails an old feature.
    pub unsafe fn lifecycle<F: Feature>(call: *const abi::LifecycleCall) -> i32 {
        let Some(c) = call.as_ref().filter(|c| c.api_version >= 3) else { return abi::BAD_ARGS };
        let host = LifecycleHost { call: *c };
        let (Some(from), Some(game)) = (abi::text(c.from_version, c.from_version_len), abi::text(c.game, c.game_len)) else { return abi::BAD_ARGS };
        let event = match c.event {
            abi::LIFECYCLE_INSTALLED => Lifecycle::Installed,
            abi::LIFECYCLE_UPDATED => Lifecycle::Updated { from_version: from },
            abi::LIFECYCLE_UNINSTALLING => Lifecycle::Uninstalling,
            abi::LIFECYCLE_GAME_EXITED => Lifecycle::GameExited { game },
            _ => return abi::OK,
        };
        match catch_unwind(AssertUnwindSafe(|| F::lifecycle(event, &host))) {
            Ok(Ok(())) => abi::OK,
            Ok(Err(e)) => {
                host.log(&e);
                abi::FAILED
            }
            Err(_) => {
                host.log("panic in lifecycle");
                abi::FAILED
            }
        }
    }
}

/// Writes the C exports of a feature library for the type `$t` (impl Feature): the version, the
/// start, and the lifecycle hook (Feature::lifecycle, which does nothing by default).
#[macro_export]
macro_rules! feature {
    ($t:ty) => {
        #[no_mangle]
        pub extern "C" fn fm_feature_api_version() -> u32 {
            $crate::API_VERSION
        }

        /// # Safety
        /// Called by the Feature Machine host with valid pointers.
        #[no_mangle]
        pub unsafe extern "C" fn fm_feature_start(host: *const $crate::abi::HostApi, out: *mut $crate::abi::FeatureApi) -> i32 {
            $crate::glue::start::<$t>(host, out)
        }

        /// # Safety
        /// Called by the Feature Machine lifecycle helper with a valid call.
        #[no_mangle]
        pub unsafe extern "C" fn fm_feature_lifecycle(call: *const $crate::abi::LifecycleCall) -> i32 {
            $crate::glue::lifecycle::<$t>(call)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every kind crosses the ABI with no loss: full u64 and i64 range, six placement numbers,
    /// text. A kind from a newer host is left out, not misread.
    #[test]
    fn every_var_kind_crosses_the_abi() {
        let vars = [
            Var::F64(-1.5),
            Var::Bool(true),
            Var::I64(i64::MIN),
            Var::U64(u64::MAX),
            Var::Vec3([1.0, 2.0, 3.0]),
            Var::Placement([1.0, 2.0, 3.0, 90.0, -5.0, 0.5]),
            Var::Text("Logs, 20 t".into()),
        ];
        for v in &vars {
            let a = v.to_abi("ats:x");
            assert_eq!(unsafe { Var::from_abi(&a) }, Some(("ats:x", v.clone())));
        }
        let mut newer = Var::F64(1.0).to_abi("ats:x");
        newer.kind = 99;
        assert_eq!(unsafe { Var::from_abi(&newer) }, None);
    }
}
