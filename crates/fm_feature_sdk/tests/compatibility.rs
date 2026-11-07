#![cfg(feature = "host")]
// Load independently compiled old callers through the production loader. A fixed ABI copy
// checks binary compatibility as the application names and source SDK change.
use fm_feature_sdk::{active::{ActiveFeature, ActiveList, State}, host::{Backend, FeatureSet, Input}};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);
impl Backend for Recorder {
    fn ui(&self, _: &str, _: &str, _: &str) {}
    fn input(&self, _: &str, _: &str, _: Input) -> Result<(), String> { Err("unavailable".into()) }
    fn log(&self, line: &str) { self.0.lock().unwrap().push(line.into()); }
    fn stopped(&self, _: &str) {}
}

#[test]
fn fixed_api_1_to_6_libraries_load_run_and_stop() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let main = repo.ancestors().find(|p| p.join(".git").exists()).unwrap_or(&repo);
    let work = main.join(".agent-work/tmp/application-compatibility");
    std::fs::create_dir_all(&work).unwrap();
    let mut features = Vec::new();
    for version in 1..=6 {
        let library = work.join(format!("compat_api_{version}{}", std::env::consts::DLL_SUFFIX));
        let output = std::process::Command::new("rustc")
            .args(["--edition=2021", "--crate-type=cdylib", "--crate-name=compat_feature"])
            .arg(repo.join("crates/fm_feature_sdk/tests/fixtures/compat_feature.rs"))
            .arg("-o").arg(&library).env("FM_FIXTURE_API", version.to_string()).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let feature: ActiveFeature = serde_json::from_value(serde_json::json!({
            "id": format!("compat-{version}"), "version": "1.0.0", "library": library,
            "games": ["ats"], "capabilities": []
        })).unwrap();
        features.push(feature);
    }
    let recorder = Arc::new(Recorder::default());
    let list = ActiveList { features, installs: None, stops: Vec::new() };
    let mut set = FeatureSet::load("ats", &list, recorder.clone(), false);
    assert_eq!(set.statuses().len(), 6);
    assert!(set.statuses().iter().all(|f| f.state == State::Running), "{:?}", set.statuses());
    set.frame::<&str>(1, 10, &[], &[]);
    set.event("paused");
    set.stop_all();
    let logs = recorder.0.lock().unwrap();
    for action in ["start", "frame", "event", "stop"] {
        assert_eq!(logs.iter().filter(|l| l.ends_with(&format!("fixture:{action}"))).count(), 6, "{logs:?}");
    }
}
