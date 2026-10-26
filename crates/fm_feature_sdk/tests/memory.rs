#![cfg(feature = "host")]
// One byte range per readable list is intended in these fixtures.
#![allow(clippy::single_range_in_vec_init)]
// Real feature ABI calls reach the production map runtime. These checks protect ownership,
// stop ordering and compatibility without installing a hook in a game.
use fm_feature_sdk::{abi, active::{ActiveFeature, State}, host::{Backend, Entry, FeatureSet, Input}, Capability, Feature, Frame, Host, HostError, Multiplayer, Var, WriteHandle};
use fm_memory::{Error, Image, Map, Memory, ModuleId, Runtime, Scalar};
use std::sync::{Arc, Mutex};

struct Storage { bytes: Vec<u8> }
fn identity() -> ModuleId { ModuleId { game: "msfs2024".into(), module: "fixture.exe".into(), sha256: "a".repeat(64), session: 7 } }
impl Memory for Storage {
    fn identity(&self) -> Result<ModuleId, Error> { Ok(identity()) }
    fn can_write(&self) -> bool { true }
    fn read(&self, address: u64, n: usize) -> Result<Vec<u8>, Error> { Ok(self.bytes[address as usize..address as usize + n].to_vec()) }
    fn write(&mut self, address: u64, bytes: &[u8]) -> Result<(), Error> { self.bytes[address as usize..address as usize + bytes.len()].copy_from_slice(bytes); Ok(()) }
}

struct Mapped { runtime: Mutex<Runtime<Storage>>, lines: Mutex<Vec<String>>, fail_cleanup: bool }
impl Mapped {
    fn new() -> Self {
        let mut bytes = b"FM_VALUE".to_vec();
        bytes.extend(2.5f32.to_le_bytes());
        let m: Map = serde_json::from_value(serde_json::json!({
            "schema": 1, "id": "test-map", "version": "1.0.0", "game": "msfs2024", "module": "fixture.exe", "sha256": "a".repeat(64),
            "symbols": [{ "name": "value", "pattern": "46 4D 5F 56 41 4C 55 45", "offset": 8, "kind": "f32", "access": "write", "min": 0.0, "max": 10.0 }]
        })).unwrap();
        let resolved = m.resolve(&Image { base: 0, bytes: bytes.clone(), id: identity(), readable: vec![0..12] }).unwrap();
        Self { runtime: Mutex::new(Runtime::new(Storage { bytes }, vec![resolved]).unwrap()), lines: Mutex::new(vec![]), fail_cleanup: false }
    }
    fn value(&self) -> Scalar { self.runtime.lock().unwrap().read("test-map/value").unwrap() }
    fn logged(&self, s: &str) -> bool { self.lines.lock().unwrap().iter().any(|line| line.contains(s)) }
}
impl Backend for Mapped {
    fn ui(&self, _: &str, _: &str, _: &str) {}
    fn input(&self, _: &str, _: &str, _: Input) -> Result<(), String> { Ok(()) }
    fn stopped(&self, _: &str) {}
    fn log(&self, line: &str) { self.lines.lock().unwrap().push(line.into()); }
    fn memory_watch(&self, key: &str, on: bool) -> Result<(), String> { self.log(&format!("watch {key} {on}")); Ok(()) }
    fn memory_acquire(&self, f: &str, k: &str) -> Result<u64, HostError> { self.runtime.lock().unwrap().acquire(f, k.strip_prefix("mem:").unwrap()).map_err(|_| HostError::Refused) }
    fn memory_set(&self, f: &str, h: u64, v: Var) -> Result<(), HostError> {
        let Var::F64(v) = v else { return Err(HostError::BadArgs) };
        self.runtime.lock().unwrap().set(f, h, Scalar::F32(v as f32)).map_err(|_| HostError::Failed)
    }
    fn memory_release(&self, f: &str, h: u64) -> Result<(), HostError> { self.runtime.lock().unwrap().release(f, h).map_err(|_| HostError::Failed) }
    fn cleanup(&self, f: &str, _: u64) -> Result<(), String> {
        if self.fail_cleanup { return Err("fixture cleanup failure".into()); }
        let errors = self.runtime.lock().unwrap().stop(f);
        if errors.is_empty() { Ok(()) } else { Err(format!("{errors:?}")) }
    }
}

struct Writer { host: Host, handle: Option<WriteHandle> }
impl Feature for Writer {
    fn start(host: Host) -> Self { Self { host, handle: None } }
    fn frame(&mut self, f: &Frame) { if let Some(v) = f.var("mem:test-map/value") { self.host.log(&format!("mapped {v:?}")); } }
    fn event(&mut self, id: &str) {
        if id == "read" { let r = self.host.subscribe("mem:test-map/value"); self.host.log(&format!("subscribe {r:?}")); }
        if id == "write" {
            let r = self.host.memory_acquire("mem:test-map/value");
            self.host.log(&format!("acquire {r:?}"));
            if let Ok(h) = r { self.handle = Some(h); let r = self.host.memory_set(h, Var::F64(4.0)); self.host.log(&format!("set {r:?}")); }
        }
        if id == "panic" { panic!("feature failure"); }
    }
    fn stop(&mut self) {
        let r = self.host.memory_acquire("mem:test-map/value");
        self.host.log(&format!("stop acquire {r:?}"));
        if let Some(h) = self.handle { let r = self.host.memory_set(h, Var::F64(5.0)); self.host.log(&format!("stop set {r:?}")); }
    }
}

fn meta(id: &str, caps: Vec<Capability>) -> ActiveFeature {
    ActiveFeature { id: id.into(), version: "1.0.0".into(), library: "not-loaded".into(), games: vec!["msfs2024".into()], capabilities: caps, multiplayer: Multiplayer::Allowed, game_commands: vec![], network_hosts: vec![], memory_maps: vec![fm_feature_sdk::active::MapDependency { id: "test-map".into(), version: "1.0.0".into(), sha256: "a".repeat(64) }], data_dir: None, user_files: vec![] }
}
unsafe extern "C" fn start(host: *const abi::HostApi, out: *mut abi::FeatureApi) -> i32 { fm_feature_sdk::glue::start::<Writer>(host, out) }
fn entry() -> Entry { Entry { api: abi::API_VERSION, start } }
fn caps() -> Vec<Capability> { vec![Capability::VarsRead, Capability::VarsMemory, Capability::VarsWrite] }

#[test]
fn mapped_values_keep_their_key_and_require_memory_consent() {
    let backend = Arc::new(Mapped::new());
    let mut set = FeatureSet::new("msfs2024", backend.clone(), false);
    set.add(meta("reader", caps()), Ok(entry()));
    set.event("read");
    set.var_result("mem:test-map/value", Ok(()));
    set.frame(1, 1, &[], &[("mem:test-map/value", Var::F64(2.5))]);
    assert!(backend.logged("mapped F64(2.5)"));
    assert!(backend.logged("watch mem:test-map/value true"));
    let backend = Arc::new(Mapped::new());
    let mut set = FeatureSet::new("msfs2024", backend.clone(), false);
    set.add(meta("reader", vec![Capability::VarsRead]), Ok(entry()));
    set.event("read");
    assert!(backend.logged("subscribe Err(Refused)"));
    assert!(!backend.logged("watch"));
    let backend = Arc::new(Mapped::new());
    let mut set = FeatureSet::new("msfs2024", backend.clone(), false);
    let mut undeclared = meta("reader", caps());
    undeclared.memory_maps.clear();
    set.add(undeclared, Ok(entry()));
    set.event("read");
    set.event("write");
    assert!(backend.logged("subscribe Err(Refused)"));
    assert!(backend.logged("acquire Err(Refused)"));
    assert!(!backend.logged("watch"));
}

#[test]
fn disconnect_restores_and_does_not_stop_existing_read_only_features() {
    let backend = Arc::new(Mapped::new());
    let mut set = FeatureSet::new("msfs2024", backend.clone(), false);
    set.add(meta("writer", caps()), Ok(entry()));
    set.add(meta("old", vec![Capability::VarsRead]), Ok(entry()));
    set.event("write");
    assert_eq!(backend.value(), Scalar::F32(4.0));
    set.disconnect();
    assert_eq!(backend.value(), Scalar::F32(2.5));
    assert_eq!(set.statuses()[0].state, State::Stopped);
    assert_eq!(set.statuses()[1].state, State::Running);
    assert!(backend.logged("stop acquire Err(Refused)"));
    assert!(backend.logged("stop set Err(Refused)"));
}

#[test]
fn feature_panic_restores_and_failed_restore_is_reported() {
    let backend = Arc::new(Mapped::new());
    let mut set = FeatureSet::new("msfs2024", backend.clone(), false);
    set.add(meta("writer", caps()), Ok(entry()));
    set.event("write");
    set.event("panic");
    assert_eq!(backend.value(), Scalar::F32(2.5));
    assert_eq!(set.statuses()[0].state, State::Failed);
    let mut backend = Mapped::new();
    backend.fail_cleanup = true;
    let backend = Arc::new(backend);
    let mut set = FeatureSet::new("msfs2024", backend, false);
    set.add(meta("writer", caps()), Ok(entry()));
    set.event("write");
    set.stop("writer", State::Stopped, None);
    assert_eq!(set.statuses()[0].state, State::Failed);
    assert!(set.statuses()[0].detail.as_ref().unwrap().contains("cleanup failed"));
}

// Startup can mutate through the ABI before it returns an error. Host cleanup owns that lease.
#[test]
fn failed_start_restores_a_write_without_a_feature_instance() {
    unsafe extern "C" fn fail_start(host: *const abi::HostApi, _: *mut abi::FeatureApi) -> i32 {
        let h = &*host;
        let key = b"mem:test-map/value";
        let lease = (h.memory_acquire)(h.ctx, key.as_ptr(), key.len());
        assert!(lease > 0);
        let value = Var::F64(4.0).to_abi("");
        assert_eq!((h.memory_set)(h.ctx, lease as u64, &value), abi::OK);
        abi::FAILED
    }
    let backend = Arc::new(Mapped::new());
    let mut set = FeatureSet::new("msfs2024", backend.clone(), false);
    set.add(meta("writer", caps()), Ok(Entry { api: abi::API_VERSION, start: fail_start }));
    assert_eq!(backend.value(), Scalar::F32(2.5));
    assert_eq!(set.statuses()[0].state, State::Failed);
    assert!(backend.runtime.lock().unwrap().acquire("writer", "test-map/value").is_err());
}

#[test]
fn failed_start_reports_failed_cleanup_in_the_host_status() {
    unsafe extern "C" fn fail(_: *const abi::HostApi, _: *mut abi::FeatureApi) -> i32 { abi::FAILED }
    let mut backend = Mapped::new();
    backend.fail_cleanup = true;
    let mut set = FeatureSet::new("msfs2024", Arc::new(backend), false);
    set.add(meta("writer", caps()), Ok(Entry { api: abi::API_VERSION, start: fail }));
    assert!(set.statuses()[0].detail.as_ref().unwrap().contains("cleanup failed"));
}

// Old binaries write only their original table prefix. The new host must preserve HTTP and
// native subscriptions while it appends the memory functions for API 5 features.
#[test]
fn api_3_and_4_tables_keep_their_original_behavior() {
    #[repr(C)]
    struct V3 {
        api_version: u32, instance: *mut std::ffi::c_void,
        on_frame: Option<unsafe extern "C" fn(*mut std::ffi::c_void, *const abi::AbiFrame) -> i32>,
        on_event: Option<unsafe extern "C" fn(*mut std::ffi::c_void, *const u8, usize) -> i32>,
        stop: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
        on_http: Option<unsafe extern "C" fn(*mut std::ffi::c_void, u64, i32, *const u8, usize) -> i32>,
    }
    unsafe extern "C" fn frame(_: *mut std::ffi::c_void, _: *const abi::AbiFrame) -> i32 { abi::OK }
    unsafe extern "C" fn event(_: *mut std::ffi::c_void, _: *const u8, _: usize) -> i32 { abi::OK }
    unsafe extern "C" fn stop(_: *mut std::ffi::c_void) {}
    unsafe extern "C" fn http(_: *mut std::ffi::c_void, _: u64, _: i32, _: *const u8, _: usize) -> i32 { abi::OK }
    unsafe extern "C" fn native(_: *mut std::ffi::c_void, _: *const abi::AbiEvent) -> i32 { abi::OK }
    unsafe extern "C" fn v3(h: *const abi::HostApi, out: *mut abi::FeatureApi) -> i32 {
        if (*h).api_version != 3 { return abi::BAD_ARGS; }
        let url = b"https://example.com/";
        if ((*h).http_get)((*h).ctx, url.as_ptr(), url.len()) <= 0 { return abi::FAILED; }
        (out as *mut V3).write(V3 { api_version: 3, instance: std::ptr::dangling_mut(), on_frame: Some(frame), on_event: Some(event), stop: Some(stop), on_http: Some(http) });
        abi::OK
    }
    unsafe extern "C" fn v4(h: *const abi::HostApi, out: *mut abi::FeatureApi) -> i32 {
        if (*h).api_version != 4 { return abi::BAD_ARGS; }
        let key = b"msfs2024:SIM ON GROUND,bool";
        if ((*h).subscribe)((*h).ctx, key.as_ptr(), key.len()) != abi::OK { return abi::FAILED; }
        out.write(abi::FeatureApi { api_version: 4, instance: std::ptr::dangling_mut(), on_frame: Some(frame), on_event: Some(event), stop: Some(stop), on_http: Some(http), on_game_event: Some(native) });
        abi::OK
    }
    struct OldBackend;
    impl Backend for OldBackend {
        fn ui(&self, _: &str, _: &str, _: &str) {}
        fn input(&self, _: &str, _: &str, _: Input) -> Result<(), String> { Ok(()) }
        fn log(&self, _: &str) {}
        fn stopped(&self, _: &str) {}
        fn watch(&self, _: &str, _: bool) -> Result<(), String> { Ok(()) }
        fn http_get(&self, _: &str, _: u64, _: &str) -> Result<(), fm_feature_sdk::HostError> { Ok(()) }
    }
    let mut set = FeatureSet::new("msfs2024", Arc::new(OldBackend), false);
    let mut m = meta("v3", vec![Capability::Network]);
    m.network_hosts.push("example.com".into());
    set.add(m, Ok(Entry { api: 3, start: v3 }));
    set.add(meta("v4", vec![Capability::VarsRead]), Ok(Entry { api: 4, start: v4 }));
    assert!(set.statuses().iter().all(|s| s.state == State::Running));
    assert_eq!(set.waiting(), 1);
    assert!(set.http_answer("v3", 1, Err(HostError::Failed)));
    set.var_result("SIM ON GROUND,bool", Ok(()));
    set.frame(1, 1, &[], &[("SIM ON GROUND,bool", Var::Bool(true))]);
    set.disconnect();
    assert!(set.statuses().iter().all(|s| s.state == State::Running));
}
