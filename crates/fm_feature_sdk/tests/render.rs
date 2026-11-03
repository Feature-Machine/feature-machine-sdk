#![cfg(feature = "host")]
// Real feature ABI calls reach a fake color-pass backend. These checks protect the capability
// rule, parameter checks, one owner, stop ordering and disconnect, without a GPU or a game. The
// shader tests do the same for feature shaders read from a package folder on disk.
use fm_feature_sdk::{abi, active::{ActiveFeature, State}, host::{Backend, Entry, FeatureSet, Input, ShaderCode}, Capability, Feature, Frame, Host, HostError, MirageLook, Multiplayer, RenderEffect, RenderHandle, RenderParams, ShaderStage};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Pass { owner: Mutex<Option<(String, u64)>>, lines: Mutex<Vec<String>> }
impl Pass {
    fn logged(&self, s: &str) -> bool { self.lines.lock().unwrap().iter().any(|line| line.contains(s)) }
    fn owner(&self) -> Option<String> { self.owner.lock().unwrap().as_ref().map(|(f, _)| f.clone()) }
}
impl Backend for Pass {
    fn ui(&self, _: &str, _: &str, _: &str) {}
    fn input(&self, _: &str, _: &str, _: Input) -> Result<(), String> { Ok(()) }
    fn stopped(&self, _: &str) {}
    fn log(&self, line: &str) { self.lines.lock().unwrap().push(line.into()); }
    fn render_acquire(&self, f: &str, p: RenderParams) -> Result<u64, HostError> {
        let mut owner = self.owner.lock().unwrap();
        if owner.is_some() { return Err(HostError::Busy); }
        *owner = Some((f.into(), 7));
        self.log(&format!("pass {f} {p:?}"));
        Ok(7)
    }
    fn render_set(&self, f: &str, h: u64, p: RenderParams) -> Result<(), HostError> {
        if *self.owner.lock().unwrap() != Some((f.into(), h)) { return Err(HostError::Refused); }
        self.log(&format!("set {f} {p:?}"));
        Ok(())
    }
    fn render_release(&self, f: &str, h: u64) -> Result<(), HostError> {
        let mut owner = self.owner.lock().unwrap();
        if *owner != Some((f.into(), h)) { return Err(HostError::Refused); }
        *owner = None;
        Ok(())
    }
    fn cleanup(&self, f: &str, _: u64) -> Result<(), String> {
        let mut owner = self.owner.lock().unwrap();
        if owner.as_ref().is_some_and(|(o, _)| o == f) { *owner = None; self.log(&format!("cleanup removed {f}")); }
        Ok(())
    }
}

const GRAY: RenderParams = RenderParams { effect: RenderEffect::Grayscale, amount: 1.0, tint: [1.0; 3] };

/// Events: "paint" acquires, "tint" sets, "bad" sends an out-of-range amount, "mirage" acquires
/// the depth effect, "strong" sets its strong look, "keep" makes stop leave the pass for host
/// cleanup; otherwise stop releases it itself.
struct Painter { host: Host, handle: Option<RenderHandle>, keep: bool }
impl Feature for Painter {
    fn start(host: Host) -> Self { Self { host, handle: None, keep: false } }
    fn frame(&mut self, _: &Frame) {}
    fn event(&mut self, id: &str) {
        match id {
            "paint" => { let r = self.host.render_acquire(GRAY); self.host.log(&format!("acquire {r:?}")); self.handle = r.ok(); }
            "tint" => {
                let p = RenderParams { effect: RenderEffect::Tint, amount: 0.5, tint: [1.0, 0.5, 0.25] };
                let r = self.host.render_set(self.handle.unwrap(), p);
                self.host.log(&format!("tint {r:?}"));
            }
            "bad" => { let r = self.host.render_acquire(RenderParams { amount: 2.0, ..GRAY }); self.host.log(&format!("bad {r:?}")); }
            "mirage" => {
                let r = self.host.render_acquire(RenderParams { effect: RenderEffect::Mirage(MirageLook::Physical), ..GRAY });
                self.host.log(&format!("mirage {r:?}"));
                if let Ok(h) = r { self.handle = Some(h); }
            }
            "strong" => {
                let r = self.host.render_set(self.handle.unwrap(), RenderParams { effect: RenderEffect::Mirage(MirageLook::Strong), ..GRAY });
                self.host.log(&format!("strong {r:?}"));
            }
            "keep" => self.keep = true,
            _ => {}
        }
    }
    fn stop(&mut self) {
        let r = self.host.render_acquire(GRAY);
        self.host.log(&format!("stop acquire {r:?}"));
        if let (Some(h), false) = (self.handle, self.keep) { let r = self.host.render_release(h); self.host.log(&format!("stop release {r:?}")); }
    }
}

fn meta(id: &str, caps: Vec<Capability>) -> ActiveFeature {
    ActiveFeature { id: id.into(), version: "1.0.0".into(), library: "not-loaded".into(), games: vec!["msfs2024".into()], capabilities: caps, multiplayer: Multiplayer::Allowed, game_commands: vec![], network_hosts: vec![], memory_maps: vec![], data_dir: None, user_files: vec![], requires: vec![], settings: None }
}
unsafe extern "C" fn start(host: *const abi::HostApi, out: *mut abi::FeatureApi) -> i32 { fm_feature_sdk::glue::start::<Painter>(host, out) }
fn entry() -> Entry { Entry { api: abi::API_VERSION, start } }

#[test]
fn pass_needs_the_capability_valid_parameters_and_one_owner() {
    let backend = Arc::new(Pass::default());
    let mut set = FeatureSet::new("msfs2024", backend.clone(), false);
    set.add(meta("plain", vec![Capability::VarsRead]), Ok(entry()));
    set.add(meta("first", vec![Capability::RenderPass]), Ok(entry()));
    set.add(meta("second", vec![Capability::RenderPass]), Ok(entry()));
    set.event("bad");
    assert!(backend.logged("bad Err(Refused)"), "no capability is refused before the parameters");
    assert!(backend.logged("bad Err(BadArgs)"));
    set.event("paint");
    assert!(backend.logged("acquire Err(Refused)"));
    assert!(backend.logged("acquire Ok(RenderHandle(7))"));
    assert!(backend.logged("acquire Err(Busy)"));
    assert_eq!(backend.owner().as_deref(), Some("first"));
    assert!(backend.logged("pass first RenderParams { effect: Grayscale, amount: 1.0"));
}

#[test]
fn the_mirage_needs_render_depth() {
    let backend = Arc::new(Pass::default());
    let mut set = FeatureSet::new("msfs2024", backend.clone(), false);
    set.add(meta("color", vec![Capability::RenderPass]), Ok(entry()));
    set.event("mirage");
    assert!(backend.logged("mirage Err(Refused)"), "render_pass alone does not give the depth");
    assert_eq!(backend.owner(), None);

    let backend = Arc::new(Pass::default());
    let mut set = FeatureSet::new("msfs2024", backend.clone(), false);
    set.add(meta("mirage", vec![Capability::RenderPass, Capability::RenderDepth]), Ok(entry()));
    set.event("mirage");
    assert!(backend.logged("mirage Ok(RenderHandle(7))"));
    assert!(backend.logged("pass mirage RenderParams { effect: Mirage(Physical)"));
    set.event("strong");
    assert!(backend.logged("set mirage RenderParams { effect: Mirage(Strong)"), "the look changes on the held pass");
}

// Slice 8: an API 4 binary (old table, no render calls) shares the set with an API 5 mirage.
// The old feature gets its own API 4 host table and keeps running when the mirage stops.
#[test]
fn an_api_4_feature_runs_next_to_the_mirage() {
    unsafe extern "C" fn frame(_: *mut std::ffi::c_void, _: *const abi::AbiFrame) -> i32 { abi::OK }
    unsafe extern "C" fn event(_: *mut std::ffi::c_void, _: *const u8, _: usize) -> i32 { abi::OK }
    unsafe extern "C" fn stop(_: *mut std::ffi::c_void) {}
    unsafe extern "C" fn http(_: *mut std::ffi::c_void, _: u64, _: i32, _: *const u8, _: usize) -> i32 { abi::OK }
    unsafe extern "C" fn native(_: *mut std::ffi::c_void, _: *const abi::AbiEvent) -> i32 { abi::OK }
    unsafe extern "C" fn v4(h: *const abi::HostApi, out: *mut abi::FeatureApi) -> i32 {
        if (*h).api_version != 4 { return abi::BAD_ARGS; }
        out.write(abi::FeatureApi { api_version: 4, instance: std::ptr::dangling_mut(), on_frame: Some(frame), on_event: Some(event), stop: Some(stop), on_http: Some(http), on_game_event: Some(native) });
        abi::OK
    }
    let backend = Arc::new(Pass::default());
    let mut set = FeatureSet::new("msfs2024", backend.clone(), false);
    set.add(meta("old", vec![Capability::VarsRead]), Ok(Entry { api: 4, start: v4 }));
    set.add(meta("mirage", vec![Capability::RenderPass, Capability::RenderDepth]), Ok(entry()));
    assert!(set.statuses().iter().all(|s| s.state == State::Running), "{:?}", set.statuses());
    set.event("mirage");
    assert_eq!(backend.owner().as_deref(), Some("mirage"));
    set.stop("mirage", State::Stopped, None);
    assert_eq!(backend.owner(), None);
    assert_eq!(set.statuses()[0].state, State::Running);
}

#[test]
fn stop_closes_admission_and_keeps_release_then_cleanup_removes_the_rest() {
    let backend = Arc::new(Pass::default());
    let mut set = FeatureSet::new("msfs2024", backend.clone(), false);
    set.add(meta("painter", vec![Capability::RenderPass]), Ok(entry()));
    set.event("paint");
    set.event("tint");
    assert!(backend.logged("tint Ok(())"));
    set.stop("painter", State::Stopped, None);
    assert!(backend.logged("stop acquire Err(Refused)"));
    assert!(backend.logged("stop release Ok(())"));
    assert!(!backend.logged("cleanup removed"));
    assert_eq!(backend.owner(), None);

    let backend = Arc::new(Pass::default());
    let mut set = FeatureSet::new("msfs2024", backend.clone(), false);
    set.add(meta("painter", vec![Capability::RenderPass]), Ok(entry()));
    set.event("paint");
    set.event("keep");
    set.stop("painter", State::Stopped, None);
    assert!(backend.logged("cleanup removed painter"));
    assert_eq!(backend.owner(), None);
}

#[test]
fn disconnect_stops_pass_owners_and_hosts_refuse_by_default() {
    let backend = Arc::new(Pass::default());
    let mut set = FeatureSet::new("msfs2024", backend.clone(), false);
    set.add(meta("painter", vec![Capability::RenderPass]), Ok(entry()));
    set.add(meta("old", vec![Capability::VarsRead]), Ok(entry()));
    set.event("paint");
    set.disconnect();
    assert_eq!(set.statuses()[0].state, State::Stopped);
    assert_eq!(set.statuses()[1].state, State::Running);
    assert_eq!(backend.owner(), None);
    let refused = backend.prepare(&meta("painter", vec![Capability::RenderPass]));
    assert!(refused.unwrap_err().contains("color pass is unavailable"));
}

/// Records each shader call: (feature, stage, name, bytes, first parameter).
#[derive(Default)]
struct Shaders { calls: Mutex<Vec<String>>, held: Mutex<Vec<(String, u64)>>, lines: Mutex<Vec<String>> }
impl Shaders {
    fn called(&self, s: &str) -> bool { self.calls.lock().unwrap().iter().any(|c| c.contains(s)) }
    fn logged(&self, s: &str) -> bool { self.lines.lock().unwrap().iter().any(|c| c.contains(s)) }
}
impl Backend for Shaders {
    fn ui(&self, _: &str, _: &str, _: &str) {}
    fn input(&self, _: &str, _: &str, _: Input) -> Result<(), String> { Ok(()) }
    fn stopped(&self, _: &str) {}
    fn log(&self, line: &str) { self.lines.lock().unwrap().push(line.into()); }
    fn shader_acquire(&self, f: &str, stage: ShaderStage, code: &ShaderCode, p: [f32; abi::SHADER_PARAMS]) -> Result<u64, HostError> {
        let mut held = self.held.lock().unwrap();
        let h = held.len() as u64 + 1;
        held.push((f.into(), h));
        self.calls.lock().unwrap().push(format!("acquire {f} {stage:?} {} {:?} {}", code.name, &code.bytes[..], p[0]));
        Ok(h)
    }
    fn shader_set(&self, f: &str, h: u64, code: Option<&ShaderCode>, p: [f32; abi::SHADER_PARAMS]) -> Result<(), HostError> {
        if !self.held.lock().unwrap().contains(&(f.into(), h)) { return Err(HostError::Refused); }
        self.calls.lock().unwrap().push(format!("set {f} {h} {:?} {}", code.map(|c| (c.name.as_str(), c.bytes.len())), p[0]));
        Ok(())
    }
    fn render_release(&self, f: &str, h: u64) -> Result<(), HostError> {
        let mut held = self.held.lock().unwrap();
        let at = held.iter().position(|x| *x == (f.to_string(), h)).ok_or(HostError::Refused)?;
        held.remove(at);
        Ok(())
    }
    fn cleanup(&self, f: &str, _: u64) -> Result<(), String> {
        let mut held = self.held.lock().unwrap();
        let before = held.len();
        held.retain(|(o, _)| o != f);
        if held.len() != before { self.calls.lock().unwrap().push(format!("cleanup {f} {}", before - held.len())); }
        Ok(())
    }
}

/// Events "<stage> <name>" acquire (stage final, scene or depth), "swap <name>" and "params" set
/// the last pass, "nan" sends a bad value; stop leaves every pass to the host cleanup.
struct Artist { host: Host, handles: Vec<RenderHandle> }
impl Feature for Artist {
    fn start(host: Host) -> Self { Self { host, handles: vec![] } }
    fn frame(&mut self, _: &Frame) {}
    fn event(&mut self, id: &str) {
        let (verb, name) = id.split_once(' ').unwrap_or((id, ""));
        let mut params = [0.0; abi::SHADER_PARAMS];
        params[0] = self.handles.len() as f32 + 0.5;
        let stage = match verb { "final" => Some(ShaderStage::Final), "scene" => Some(ShaderStage::Scene), "depth" => Some(ShaderStage::SceneDepth), _ => None };
        let r = match (verb, stage) {
            (_, Some(stage)) => self.host.shader_acquire(stage, name, &params).map(|h| { self.handles.push(h); }),
            ("swap", _) => self.host.shader_set(*self.handles.last().unwrap(), Some(name), &params),
            ("params", _) => self.host.shader_set(*self.handles.last().unwrap(), None, &[9.0; abi::SHADER_PARAMS]),
            ("nan", _) => self.host.shader_acquire(ShaderStage::Final, "tint", &[f32::NAN; abi::SHADER_PARAMS]).map(|_| ()),
            ("release", _) => self.host.render_release(self.handles.remove(0)),
            _ => return,
        };
        self.host.log(&format!("{id} {r:?}"));
    }
    fn stop(&mut self) {
        let r = self.host.shader_acquire(ShaderStage::Final, "tint", &[0.0; abi::SHADER_PARAMS]);
        self.host.log(&format!("stop acquire {r:?}"));
    }
}
unsafe extern "C" fn start_artist(host: *const abi::HostApi, out: *mut abi::FeatureApi) -> i32 { fm_feature_sdk::glue::start::<Artist>(host, out) }

/// A version folder in the package layout: bin/windows/feature.dll (not loaded) and shaders/.
fn package(tag: &str, files: &[(&str, &[u8])]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("fm-sdk-shaders-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("shaders")).unwrap();
    for (name, bytes) in files { std::fs::write(dir.join("shaders").join(name), bytes).unwrap(); }
    dir
}
fn artist(id: &str, caps: Vec<Capability>, dir: &std::path::Path) -> ActiveFeature {
    ActiveFeature { library: dir.join("bin").join("windows").join("feature.dll"), ..meta(id, caps) }
}

#[test]
fn shader_passes_need_render_shader_and_read_the_package_file() {
    let dir = package("rules", &[("tint.cso", b"tint"), ("blur-2.cso", b"blur"), ("big.cso", &vec![0u8; abi::SHADER_SIZE_MAX + 1])]);
    let backend = Arc::new(Shaders::default());
    let mut set = FeatureSet::new("msfs2024", backend.clone(), false);
    set.add(artist("color", vec![Capability::RenderPass], &dir), Ok(Entry { api: abi::API_VERSION, start: start_artist }));
    set.add(artist("art", vec![Capability::RenderPass, Capability::RenderShader], &dir), Ok(Entry { api: abi::API_VERSION, start: start_artist }));
    set.event("final tint");
    assert!(backend.logged("[color] final tint Err(Refused)"), "render_pass alone runs no feature shader");
    assert!(backend.called("acquire art Final tint [116, 105, 110, 116] 0.5"), "the bytes come from shaders/tint.cso");
    for bad in ["final ../tint", "final Tint", "final tint.cso", "final ", "final missing", "final big", "nan"] {
        set.event(bad);
        assert!(backend.logged(&format!("[art] {bad} Err(BadArgs)")), "{bad}");
    }
    assert!(backend.logged("[art] shader missing: "), "a missing file is logged with its path");
    assert!(backend.logged("[art] shader big: "), "a file over the limit is logged");
    set.event("depth tint");
    assert!(backend.logged("[art] depth tint Err(Refused)"), "the depth stage needs render_depth");
    set.event("scene blur-2");
    assert!(backend.called("acquire art Scene blur-2 [98, 108, 117, 114] 1.5"), "any number of passes, each with its stage");
    set.event("swap tint");
    assert!(backend.called("set art 2 Some((\"tint\", 4)) 2.5"), "a swap gives the new shader");
    set.event("params");
    assert!(backend.called("set art 2 None 9"), "a set without a name keeps the shader");
    set.event("release");
    assert!(backend.logged("[art] release Ok(())"), "render_release removes a shader pass");
    set.stop("art", State::Stopped, None);
    assert!(backend.logged("[art] stop acquire Err(Refused)"), "stop closes admission");
    assert!(backend.called("cleanup art 1"), "cleanup removes the passes that the feature kept");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_depth_stage_needs_render_depth_and_hosts_refuse_shaders_by_default() {
    let dir = package("depth", &[("heat.cso", b"heat")]);
    let backend = Arc::new(Shaders::default());
    let mut set = FeatureSet::new("msfs2024", backend.clone(), false);
    set.add(artist("heat", vec![Capability::RenderPass, Capability::RenderDepth, Capability::RenderShader], &dir), Ok(Entry { api: abi::API_VERSION, start: start_artist }));
    set.event("depth heat");
    assert!(backend.called("acquire heat SceneDepth heat"));
    // A host without shader passes (the trait defaults) refuses them.
    let plain = Arc::new(Pass::default());
    let mut set = FeatureSet::new("msfs2024", plain.clone(), false);
    set.add(artist("art", vec![Capability::RenderPass, Capability::RenderShader], &dir), Ok(Entry { api: abi::API_VERSION, start: start_artist }));
    set.event("final heat");
    assert!(plain.logged("[art] final heat Err(Refused)"));
    let _ = std::fs::remove_dir_all(&dir);
}

// Slice 7: a held pass whose file changes gets the new bytes with its last parameters at the
// next file check; a file that no longer loads keeps the old shader and is logged once.
#[test]
fn a_changed_shader_file_reloads_into_its_pass() {
    let dir = package("reload", &[("tint.cso", b"tint")]);
    let backend = Arc::new(Shaders::default());
    let mut set = FeatureSet::new("msfs2024", backend.clone(), false);
    set.add(artist("art", vec![Capability::RenderPass, Capability::RenderShader], &dir), Ok(Entry { api: abi::API_VERSION, start: start_artist }));
    set.event("final tint");
    set.frame::<&str>(1, 0, &[], &[]);
    assert!(!backend.called("set art"), "an unchanged file does not reload");
    std::fs::write(dir.join("shaders").join("tint.cso"), b"tint v2").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    set.frame::<&str>(2, 0, &[], &[]);
    assert!(backend.called("set art 1 Some((\"tint\", 7)) 0.5"), "the new bytes with the last parameters");
    assert!(backend.logged("[art] shader tint reloaded (pass 1)"));
    std::fs::remove_file(dir.join("shaders").join("tint.cso")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    set.frame::<&str>(3, 0, &[], &[]);
    assert_eq!(backend.calls.lock().unwrap().iter().filter(|c| c.starts_with("set art")).count(), 1, "a missing file does not swap");
    set.event("release");
    std::fs::write(dir.join("shaders").join("tint.cso"), b"tint v3!").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    set.frame::<&str>(4, 0, &[], &[]);
    assert_eq!(backend.calls.lock().unwrap().iter().filter(|c| c.starts_with("set art")).count(), 1, "a released pass is not reloaded");
    let _ = std::fs::remove_dir_all(&dir);
}
