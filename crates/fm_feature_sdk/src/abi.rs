// The C ABI between a host (in the game, or the fm_feature_hook helper for lifecycle calls) and a
// feature library (plan decision 7). Only C types cross it, so a feature built with another Rust
// version still loads. Strings are UTF-8 pointer and length; each struct lives for the whole call.
// Change rule: a new field or function goes at the end of its struct with a new API_VERSION; a
// host gives each feature the version it was built for, so old features still load.
use std::ffi::c_void;

/// The feature API version of this SDK. 2: game_command. 3: http_get, data_dir, on_http, the
/// lifecycle export. 4: game values (subscribe, AbiFrame::vars), send_event, on_game_event.
/// 5: memory leases and the color pass. 5 is not released, so the render calls, page events,
/// shaders, control events and the user files service (game_started_us, file_info, file_read,
/// file_write, LifecycleCall::game_running) extend it at the end instead of starting 6.
/// 6: feature surfaces (page, art, widgets and settings in feature.json) and the settings path.
pub const API_VERSION: u32 = 6;
/// The oldest feature API that a host of this SDK still loads.
pub const MIN_API_VERSION: u32 = 1;

/// Symbols that every feature library exports (the feature! macro writes them).
pub const SYM_API_VERSION: &[u8] = b"fm_feature_api_version\0";
pub const SYM_START: &[u8] = b"fm_feature_start\0";
/// Version 3, optional (LifecycleFn). Feature Machine calls it outside the game, in a helper
/// process of its own (fm_feature_hook), never in the app or the game. A library without it
/// (API 1 and 2) has no lifecycle work: the helper skips it.
pub const SYM_LIFECYCLE: &[u8] = b"fm_feature_lifecycle\0";

pub const KIND_F64: u32 = 0;
pub const KIND_BOOL: u32 = 1;
pub const KIND_VEC3: u32 = 2;
// Version 4, native values only (AbiVar). A host never puts them in AbiFrame::values.
/// Signed integer in AbiVar::int.
pub const KIND_I64: u32 = 3;
/// Unsigned integer: the bits of AbiVar::int.
pub const KIND_U64: u32 = 4;
/// Position (num 0 to 2) and orientation (num 3 to 5: heading, pitch, roll in degrees).
pub const KIND_PLACEMENT: u32 = 5;
/// UTF-8 text in AbiVar::text.
pub const KIND_TEXT: u32 = 6;

/// One neutral value. F64 and Bool use v[0] (Bool: 0 or 1).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiValue {
    pub name: *const u8,
    pub name_len: usize,
    pub kind: u32,
    pub v: [f64; 3],
}

/// Version 4. One native game value or event attribute, any kind. F64 and Bool use num[0],
/// Vec3 num[0..3], Placement num[0..6]; I64 and U64 use int; Text uses text and text_len.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiVar {
    pub name: *const u8,
    pub name_len: usize,
    pub kind: u32,
    pub num: [f64; 6],
    pub int: i64,
    pub text: *const u8,
    pub text_len: usize,
}

/// The values of one game frame. Valid only during the on_frame call.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiFrame {
    pub seq: u64,
    /// Unix microseconds when the game made the values.
    pub time_us: u64,
    pub values: *const AbiValue,
    pub len: usize,
    /// Version 4. The native values that this feature subscribed to, named "<game>:<native>".
    pub vars: *const AbiVar,
    pub vars_len: usize,
}

/// Version 4. A game event with its attributes (native names, for example the revenue of an ATS
/// job delivery). Valid only during the on_game_event call.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiEvent {
    pub id: *const u8,
    pub id_len: usize,
    pub attrs: *const AbiVar,
    pub attrs_len: usize,
}

/// Version 5. An on_game_event id that starts with PAGE_EVENT is a message from the feature's own
/// page in the app (capability ui_app): the rest of the id is the topic, and the text attribute
/// PAGE_DATA holds the JSON. A host never gives a game event an id with this prefix, so a game
/// cannot pose as the page.
pub const PAGE_EVENT: &str = "ui:";
pub const PAGE_DATA: &str = "data";
/// The largest page message (JSON bytes) that a host gives a feature; the app checks it first.
pub const PAGE_LIMIT: usize = 16 * 1024;

/// Version 5. An on_game_event id that starts with CONTROL_EVENT is a control of the feature
/// (capability controls) that the member bound in the app: the rest of the id is the control id.
/// One attribute holds the value: CONTROL_STATE (bool, button), CONTROL_VALUE (f64, axis) or
/// CONTROL_DELTA (f64, delta). A host never gives a game event an id with this prefix.
pub const CONTROL_EVENT: &str = "input:";
pub const CONTROL_STATE: &str = "state";
pub const CONTROL_VALUE: &str = "value";
pub const CONTROL_DELTA: &str = "delta";

/// Version 6. The id of the event that gives a feature its new settings (feature surfaces plan,
/// section 3): the text attribute SETTINGS_DATA holds the full projection (a JSON object) and
/// the U64 attribute SETTINGS_REVISION its revision. A host gives it only to a version 6 table.
/// A host never gives a game event an id that starts with SETTINGS_PREFIX.
pub const SETTINGS_PREFIX: &str = "settings:";
pub const SETTINGS_EVENT: &str = "settings:changed";
pub const SETTINGS_DATA: &str = "data";
pub const SETTINGS_REVISION: &str = "revision";

/// An event id that only the app gives (a page message, a control, settings): a game event with
/// one of these prefixes would pose as the member.
pub fn app_event(id: &str) -> bool {
    id.starts_with(PAGE_EVENT) || id.starts_with(CONTROL_EVENT) || id.starts_with(SETTINGS_PREFIX)
}

pub const INPUT_PRESS: u32 = 0;
pub const INPUT_AXIS: u32 = 1;

/// Result codes of host calls.
pub const OK: i32 = 0;
/// The feature did not declare the capability for this call.
pub const REFUSED: i32 = -1;
/// Bad text or value.
pub const BAD_ARGS: i32 = -2;
/// The game side failed (the reason goes to the feature log).
pub const FAILED: i32 = -3;
/// Version 3. Too many requests wait (http_get), or the app limits them: try again later.
pub const BUSY: i32 = -4;
/// Version 3. No answer in time (http_get: 60 s).
pub const TIMEOUT: i32 = -5;

/// What the host gives a feature. Its address stays the same until the feature stops.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct HostApi {
    pub api_version: u32,
    pub ctx: *mut c_void,
    /// Game id: "ats" or "msfs2024".
    pub game: *const u8,
    pub game_len: usize,
    /// A message for the feature's UI page in the app (capability ui_app).
    pub ui_send: unsafe extern "C" fn(ctx: *mut c_void, topic: *const u8, topic_len: usize, json: *const u8, json_len: usize) -> i32,
    /// An input into the game (capability input_inject): INPUT_PRESS with ms, or INPUT_AXIS with value.
    pub input: unsafe extern "C" fn(ctx: *mut c_void, name: *const u8, name_len: usize, kind: u32, value: f32, ms: u32) -> i32,
    /// A line in the feature log (the host prefixes the feature id).
    pub log: unsafe extern "C" fn(ctx: *mut c_void, text: *const u8, len: usize),
    /// Version 2. A game engine command, for example "g_set_weather 1 i" (capability
    /// game_command; its first word must be in the feature's game_commands).
    pub game_command: unsafe extern "C" fn(ctx: *mut c_void, text: *const u8, len: usize) -> i32,
    /// Version 3. An HTTPS GET of a URL on a declared host (capability network). Returns a
    /// request id > 0 at once; the answer comes later in FeatureApi::on_http on the feature
    /// thread. A negative code means no request: REFUSED, BAD_ARGS, BUSY or FAILED.
    pub http_get: unsafe extern "C" fn(ctx: *mut c_void, url: *const u8, len: usize) -> i64,
    /// Version 3. The feature's own data folder (UTF-8 path; length 0 when the host has none).
    /// The same folder as LifecycleCall::data_dir; it stays over updates and goes at uninstall.
    /// A feature with user_files in a game with another user folder (ATS -homedir) gets a folder
    /// of its own for that user folder (files::data_dir_for), the same in the exit hook.
    pub data_dir: *const u8,
    pub data_dir_len: usize,
    /// Version 4. Read a native game value by the game's own name (capability vars_read), as
    /// "<game>:<native>": "ats:truck.fuel.amount", "ats:truck.wheel.rotation[2]",
    /// "msfs2024:FUEL TOTAL QUANTITY,gallons". The host has no list: the game decides which names
    /// exist. OK means it is watched; the value comes in each frame, and an event "var.ready" or
    /// "var.unknown" (attribute "name") tells the result. BUSY: too many names.
    pub subscribe: unsafe extern "C" fn(ctx: *mut c_void, key: *const u8, len: usize) -> i32,
    /// Version 4. Stop reading a value that subscribe added.
    pub unsubscribe: unsafe extern "C" fn(ctx: *mut c_void, key: *const u8, len: usize) -> i32,
    /// Version 4. A sim event by its native name with one data value (capability input_inject),
    /// for example "HEADING_BUG_SET" 270 in MSFS. Sent once and never reversed. FAILED in a game
    /// with no such events (ATS: use input or game_command).
    pub send_event: unsafe extern "C" fn(ctx: *mut c_void, name: *const u8, len: usize, data: i32) -> i32,
    /// Version 5. Acquire one writer for a mapped symbol. Positive handle or a negative result.
    pub memory_acquire: unsafe extern "C" fn(ctx: *mut c_void, key: *const u8, len: usize) -> i64,
    /// Version 5. Set a scalar through its owned handle. The AbiVar name is ignored.
    pub memory_set: unsafe extern "C" fn(ctx: *mut c_void, handle: u64, value: *const AbiVar) -> i32,
    /// Version 5. Restore the original scalar and release its writer.
    pub memory_release: unsafe extern "C" fn(ctx: *mut c_void, handle: u64) -> i32,
    /// Version 5. Acquire the color pass (capability render_pass) with its first parameters.
    /// Positive handle or a negative result; BUSY when another feature owns the pass.
    pub render_acquire: unsafe extern "C" fn(ctx: *mut c_void, params: *const AbiRender) -> i64,
    /// Version 5. New parameters for the owned pass; the next presented frame uses them.
    pub render_set: unsafe extern "C" fn(ctx: *mut c_void, handle: u64, params: *const AbiRender) -> i32,
    /// Version 5. Remove the pass; the game image is unchanged again from the next frame.
    /// Also removes a shader pass (shader_acquire): both kinds share one handle space.
    pub render_release: unsafe extern "C" fn(ctx: *mut c_void, handle: u64) -> i32,
    /// Version 5. Acquire a pass that runs the feature's own pixel shader (capability
    /// render_shader). Positive handle or a negative result; any number of passes can run.
    pub shader_acquire: unsafe extern "C" fn(ctx: *mut c_void, params: *const AbiShader) -> i64,
    /// Version 5. New parameters, and a new shader when `name` is not empty, for an owned shader
    /// pass; the next frame uses them. The stage of a pass never changes (`stage` is not read).
    pub shader_set: unsafe extern "C" fn(ctx: *mut c_void, handle: u64, params: *const AbiShader) -> i32,
    /// Version 5. Unix microseconds of the game process start; 0 when the host does not know it.
    pub game_started_us: u64,
    /// Version 5. Kind, size and time of a declared game file (capability user_files). The key
    /// is "<game>:<path>" under the game's user folder (files.rs). A missing file is FILE_NONE.
    pub file_info: FileInfoFn,
    /// Version 5. Bytes of a declared file from `offset` into `buf`: the count (0 at the end) or
    /// a negative code. At most files::MAX_BYTES per call.
    pub file_read: FileReadFn,
    /// Version 5. Replaces a declared file with access write, atomically. The folder must exist.
    pub file_write: FileWriteFn,
    /// Version 6. The feature's settings at this start: a JSON object (UTF-8, "{}" when it has
    /// none) and its revision. Later changes come as SETTINGS_EVENT.
    pub settings: *const u8,
    pub settings_len: usize,
    pub settings_revision: u64,
}

/// Version 5. AbiFileInfo::kind.
pub const FILE_NONE: u32 = 0;
pub const FILE_FILE: u32 = 1;
pub const FILE_FOLDER: u32 = 2;

/// Version 5. One file_info answer. modified_us: Unix microseconds, 0 when not known.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AbiFileInfo {
    pub kind: u32,
    pub size: u64,
    pub modified_us: u64,
}

/// The user files calls, the same in HostApi and LifecycleCall (each with its own ctx).
pub type FileInfoFn = unsafe extern "C" fn(ctx: *mut c_void, key: *const u8, len: usize, out: *mut AbiFileInfo) -> i32;
pub type FileReadFn = unsafe extern "C" fn(ctx: *mut c_void, key: *const u8, len: usize, offset: u64, buf: *mut u8, cap: usize) -> i64;
pub type FileWriteFn = unsafe extern "C" fn(ctx: *mut c_void, key: *const u8, len: usize, bytes: *const u8, bytes_len: usize) -> i32;

/// Version 5. Effects of the host-reviewed color pass (AbiRender::effect).
pub const RENDER_IDENTITY: u32 = 0;
pub const RENDER_TINT: u32 = 1;
pub const RENDER_GRAYSCALE: u32 = 2;
/// Version 5. Needs the render_depth capability; amount is the intensity, tint is not used.
/// RENDER_MIRAGE is the physical look (a thin band at a low sun angle); RENDER_MIRAGE_STRONG is an
/// exaggerated look (a wide wet band). The host owns the values of each look.
pub const RENDER_MIRAGE: u32 = 3;
pub const RENDER_MIRAGE_STRONG: u32 = 4;

/// Version 5. Stages of a shader pass (AbiShader::stage). FINAL: the presented image with the
/// game UI. SCENE: the game's HDR scene before tone mapping and UI. SCENE_DEPTH: the scene with
/// the depth in fm_depth (also needs the render_depth capability).
pub const SHADER_FINAL: u32 = 0;
pub const SHADER_SCENE: u32 = 1;
pub const SHADER_SCENE_DEPTH: u32 = 2;
/// Version 5. The parameters of a shader (fm_params of shader contract v1), the longest shader
/// name and the largest compiled shader file that a host loads.
pub const SHADER_PARAMS: usize = 32;
pub const SHADER_NAME_MAX: usize = 64;
pub const SHADER_SIZE_MAX: usize = 1024 * 1024;

/// Version 5. A shader pass: the stage, the shader by its name (the package file
/// shaders/<name>.cso; 1 to SHADER_NAME_MAX characters a-z, 0-9, '_' or '-') and its finite
/// parameters. The compiled shader stays in the host; no GPU object reaches the feature.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiShader {
    pub stage: u32,
    pub name: *const u8,
    pub name_len: usize,
    pub params: [f32; SHADER_PARAMS],
}

/// Version 5. Color pass parameters: effect, amount 0 to 1 and tint (each channel 0 to 1).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AbiRender {
    pub effect: u32,
    pub amount: f32,
    pub tint: [f32; 3],
}

// Lifecycle events (LifecycleCall::event). Uninstalling and GameExited come when the app saw the
// feature's games closed; Installed and Updated do not wait for them. A game can start again at
// any time, so a hook that needs it closed checks the game process itself.
/// The package is installed for the first time (this version is active now).
pub const LIFECYCLE_INSTALLED: u32 = 1;
/// A new version is active; `from_version` is the version before.
pub const LIFECYCLE_UPDATED: u32 = 2;
/// The feature is removed next (the feature or the whole app); its files and data go after.
pub const LIFECYCLE_UNINSTALLING: u32 = 3;
/// The game process `game` ended (or the app found it closed at its start).
pub const LIFECYCLE_GAME_EXITED: u32 = 4;

/// One lifecycle call. Valid during the call only. A new field goes at the end with a new
/// API_VERSION (the change rule above); `api_version` says which fields are there.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LifecycleCall {
    pub api_version: u32,
    pub event: u32,
    pub ctx: *mut c_void,
    /// LIFECYCLE_UPDATED: the version before. Other events: length 0.
    pub from_version: *const u8,
    pub from_version_len: usize,
    /// LIFECYCLE_GAME_EXITED: the game id ("ats"). Other events: length 0.
    pub game: *const u8,
    pub game_len: usize,
    /// The feature's own data folder (UTF-8 path), the same as HostApi::data_dir.
    pub data_dir: *const u8,
    pub data_dir_len: usize,
    /// A line in the hook log (Feature Machine writes it to its own log).
    pub log: unsafe extern "C" fn(ctx: *mut c_void, text: *const u8, len: usize),
    /// Version 5. The user files calls of HostApi, for the files of the version that runs the
    /// hook; a key can name any game of its list (no game runs).
    pub file_info: FileInfoFn,
    pub file_read: FileReadFn,
    pub file_write: FileWriteFn,
    /// Version 5. 1 when a process of `game` ("ats") runs on this computer, 0 when none does, or a
    /// negative code. A hook that changes files that the game saves checks it first.
    pub game_running: unsafe extern "C" fn(ctx: *mut c_void, game: *const u8, len: usize) -> i32,
}

/// Returns OK, or FAILED when the work failed (the feature logged why).
pub type LifecycleFn = unsafe extern "C" fn(call: *const LifecycleCall) -> i32;

/// What a feature gives back from fm_feature_start. stop frees the instance. The functions are
/// Option (same layout as a pointer): a null from a C feature is refused, not undefined behaviour.
/// on_frame, on_event and on_http return OK, or FAILED when the call failed (a panic): the host
/// then stops the feature and releases its inputs.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FeatureApi {
    pub api_version: u32,
    pub instance: *mut c_void,
    pub on_frame: Option<unsafe extern "C" fn(instance: *mut c_void, frame: *const AbiFrame) -> i32>,
    pub on_event: Option<unsafe extern "C" fn(instance: *mut c_void, id: *const u8, len: usize) -> i32>,
    pub stop: Option<unsafe extern "C" fn(instance: *mut c_void)>,
    /// Version 3. The answer to http_get request `id`: `status` is the HTTP status (1 to 999)
    /// with the body, or a negative code (REFUSED, BUSY, TIMEOUT, FAILED) with no body. The body
    /// is valid during the call only. A host reads this field only from a version 3 table, and
    /// a zeroed table from an older feature leaves it None.
    pub on_http: Option<unsafe extern "C" fn(instance: *mut c_void, id: u64, status: i32, body: *const u8, len: usize) -> i32>,
    /// Version 4. A game event with its attributes. A host calls it in place of on_event for a
    /// version 4 table; attributes are there only with capability vars_read.
    pub on_game_event: Option<unsafe extern "C" fn(instance: *mut c_void, event: *const AbiEvent) -> i32>,
}

pub type ApiVersionFn = unsafe extern "C" fn() -> u32;
/// Returns OK and fills `out`, or a negative code (the feature logged why).
pub type StartFn = unsafe extern "C" fn(host: *const HostApi, out: *mut FeatureApi) -> i32;

/// UTF-8 text from a pointer and a length; None for bad text.
///
/// # Safety
/// `p` must point to `len` readable bytes (or `len` is 0).
pub unsafe fn text<'a>(p: *const u8, len: usize) -> Option<&'a str> {
    if len == 0 {
        return Some("");
    }
    if p.is_null() {
        return None;
    }
    std::str::from_utf8(std::slice::from_raw_parts(p, len)).ok()
}
