// Frozen C ABI through API 6. Keep this independent of the current SDK.
// The fixed layout detects changes that would break an existing feature library.
use std::ffi::c_void;

pub const API_VERSION: u32 = 6;
pub const MIN_API_VERSION: u32 = 1;

pub const SYM_API_VERSION: &[u8] = b"fm_feature_api_version\0";
pub const SYM_START: &[u8] = b"fm_feature_start\0";
pub const SYM_LIFECYCLE: &[u8] = b"fm_feature_lifecycle\0";

pub const KIND_F64: u32 = 0;
pub const KIND_BOOL: u32 = 1;
pub const KIND_VEC3: u32 = 2;
pub const KIND_I64: u32 = 3;
pub const KIND_U64: u32 = 4;
pub const KIND_PLACEMENT: u32 = 5;
pub const KIND_TEXT: u32 = 6;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiValue {
    pub name: *const u8,
    pub name_len: usize,
    pub kind: u32,
    pub v: [f64; 3],
}

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

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiFrame {
    pub seq: u64,
    pub time_us: u64,
    pub values: *const AbiValue,
    pub len: usize,
    pub vars: *const AbiVar,
    pub vars_len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiEvent {
    pub id: *const u8,
    pub id_len: usize,
    pub attrs: *const AbiVar,
    pub attrs_len: usize,
}

pub const PAGE_EVENT: &str = "ui:";
pub const PAGE_DATA: &str = "data";
pub const PAGE_LIMIT: usize = 16 * 1024;

pub const CONTROL_EVENT: &str = "input:";
pub const CONTROL_STATE: &str = "state";
pub const CONTROL_VALUE: &str = "value";
pub const CONTROL_DELTA: &str = "delta";

pub const SETTINGS_PREFIX: &str = "settings:";
pub const SETTINGS_EVENT: &str = "settings:changed";
pub const SETTINGS_DATA: &str = "data";
pub const SETTINGS_REVISION: &str = "revision";

pub fn app_event(id: &str) -> bool {
    id.starts_with(PAGE_EVENT) || id.starts_with(CONTROL_EVENT) || id.starts_with(SETTINGS_PREFIX)
}

pub const INPUT_PRESS: u32 = 0;
pub const INPUT_AXIS: u32 = 1;

pub const OK: i32 = 0;
pub const REFUSED: i32 = -1;
pub const BAD_ARGS: i32 = -2;
pub const FAILED: i32 = -3;
pub const BUSY: i32 = -4;
pub const TIMEOUT: i32 = -5;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct HostApi {
    pub api_version: u32,
    pub ctx: *mut c_void,
    pub game: *const u8,
    pub game_len: usize,
    pub ui_send: unsafe extern "C" fn(ctx: *mut c_void, topic: *const u8, topic_len: usize, json: *const u8, json_len: usize) -> i32,
    pub input: unsafe extern "C" fn(ctx: *mut c_void, name: *const u8, name_len: usize, kind: u32, value: f32, ms: u32) -> i32,
    pub log: unsafe extern "C" fn(ctx: *mut c_void, text: *const u8, len: usize),
    pub game_command: unsafe extern "C" fn(ctx: *mut c_void, text: *const u8, len: usize) -> i32,
    pub http_get: unsafe extern "C" fn(ctx: *mut c_void, url: *const u8, len: usize) -> i64,
    pub data_dir: *const u8,
    pub data_dir_len: usize,
    pub subscribe: unsafe extern "C" fn(ctx: *mut c_void, key: *const u8, len: usize) -> i32,
    pub unsubscribe: unsafe extern "C" fn(ctx: *mut c_void, key: *const u8, len: usize) -> i32,
    pub send_event: unsafe extern "C" fn(ctx: *mut c_void, name: *const u8, len: usize, data: i32) -> i32,
    pub memory_acquire: unsafe extern "C" fn(ctx: *mut c_void, key: *const u8, len: usize) -> i64,
    pub memory_set: unsafe extern "C" fn(ctx: *mut c_void, handle: u64, value: *const AbiVar) -> i32,
    pub memory_release: unsafe extern "C" fn(ctx: *mut c_void, handle: u64) -> i32,
    pub render_acquire: unsafe extern "C" fn(ctx: *mut c_void, params: *const AbiRender) -> i64,
    pub render_set: unsafe extern "C" fn(ctx: *mut c_void, handle: u64, params: *const AbiRender) -> i32,
    pub render_release: unsafe extern "C" fn(ctx: *mut c_void, handle: u64) -> i32,
    pub shader_acquire: unsafe extern "C" fn(ctx: *mut c_void, params: *const AbiShader) -> i64,
    pub shader_set: unsafe extern "C" fn(ctx: *mut c_void, handle: u64, params: *const AbiShader) -> i32,
    pub game_started_us: u64,
    pub file_info: FileInfoFn,
    pub file_read: FileReadFn,
    pub file_write: FileWriteFn,
    pub settings: *const u8,
    pub settings_len: usize,
    pub settings_revision: u64,
}

pub const FILE_NONE: u32 = 0;
pub const FILE_FILE: u32 = 1;
pub const FILE_FOLDER: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AbiFileInfo {
    pub kind: u32,
    pub size: u64,
    pub modified_us: u64,
}

pub type FileInfoFn = unsafe extern "C" fn(ctx: *mut c_void, key: *const u8, len: usize, out: *mut AbiFileInfo) -> i32;
pub type FileReadFn = unsafe extern "C" fn(ctx: *mut c_void, key: *const u8, len: usize, offset: u64, buf: *mut u8, cap: usize) -> i64;
pub type FileWriteFn = unsafe extern "C" fn(ctx: *mut c_void, key: *const u8, len: usize, bytes: *const u8, bytes_len: usize) -> i32;

pub const RENDER_IDENTITY: u32 = 0;
pub const RENDER_TINT: u32 = 1;
pub const RENDER_GRAYSCALE: u32 = 2;
pub const RENDER_MIRAGE: u32 = 3;
pub const RENDER_MIRAGE_STRONG: u32 = 4;

pub const SHADER_FINAL: u32 = 0;
pub const SHADER_SCENE: u32 = 1;
pub const SHADER_SCENE_DEPTH: u32 = 2;
pub const SHADER_PARAMS: usize = 32;
pub const SHADER_NAME_MAX: usize = 64;
pub const SHADER_SIZE_MAX: usize = 1024 * 1024;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiShader {
    pub stage: u32,
    pub name: *const u8,
    pub name_len: usize,
    pub params: [f32; SHADER_PARAMS],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AbiRender {
    pub effect: u32,
    pub amount: f32,
    pub tint: [f32; 3],
}

pub const LIFECYCLE_INSTALLED: u32 = 1;
pub const LIFECYCLE_UPDATED: u32 = 2;
pub const LIFECYCLE_UNINSTALLING: u32 = 3;
pub const LIFECYCLE_GAME_EXITED: u32 = 4;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LifecycleCall {
    pub api_version: u32,
    pub event: u32,
    pub ctx: *mut c_void,
    pub from_version: *const u8,
    pub from_version_len: usize,
    pub game: *const u8,
    pub game_len: usize,
    pub data_dir: *const u8,
    pub data_dir_len: usize,
    pub log: unsafe extern "C" fn(ctx: *mut c_void, text: *const u8, len: usize),
    pub file_info: FileInfoFn,
    pub file_read: FileReadFn,
    pub file_write: FileWriteFn,
    pub game_running: unsafe extern "C" fn(ctx: *mut c_void, game: *const u8, len: usize) -> i32,
}

pub type LifecycleFn = unsafe extern "C" fn(call: *const LifecycleCall) -> i32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FeatureApi {
    pub api_version: u32,
    pub instance: *mut c_void,
    pub on_frame: Option<unsafe extern "C" fn(instance: *mut c_void, frame: *const AbiFrame) -> i32>,
    pub on_event: Option<unsafe extern "C" fn(instance: *mut c_void, id: *const u8, len: usize) -> i32>,
    pub stop: Option<unsafe extern "C" fn(instance: *mut c_void)>,
    pub on_http: Option<unsafe extern "C" fn(instance: *mut c_void, id: u64, status: i32, body: *const u8, len: usize) -> i32>,
    pub on_game_event: Option<unsafe extern "C" fn(instance: *mut c_void, event: *const AbiEvent) -> i32>,
}

pub type ApiVersionFn = unsafe extern "C" fn() -> u32;
pub type StartFn = unsafe extern "C" fn(host: *const HostApi, out: *mut FeatureApi) -> i32;

pub unsafe fn text<'a>(p: *const u8, len: usize) -> Option<&'a str> {
    if len == 0 {
        return Some("");
    }
    if p.is_null() {
        return None;
    }
    std::str::from_utf8(std::slice::from_raw_parts(p, len)).ok()
}
