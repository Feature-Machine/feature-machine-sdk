// These boundary checks keep legacy bytes readable and prove the new names carry the same
// powers. They use JSON inputs rather than constructing the implementation's wire types.
use fm_install::{feature::Manifest, module::{ModuleManifest, Power}, policy::{Policies, Subject}};
use serde_json::{json, Value};

fn manifest() -> Value {
    json!({"format":2,"id":"status-probe","name":"Status","description":"Status data","version":"1.0.0","api":6,
        "applications":["dbox-haptisync-center"],"capabilities":["vars_read","ui_app"]})
}

#[test]
fn neutral_manifest_round_trips_and_rejects_conflicting_names() {
    let value = manifest();
    let parsed: Manifest = serde_json::from_value(value.clone()).unwrap();
    parsed.check().unwrap();
    let output = serde_json::to_value(&parsed).unwrap();
    assert_eq!(output["applications"], value["applications"]);
    assert!(output.get("games").is_none());
    let mut conflict = value;
    conflict["games"] = json!(["ats"]);
    assert!(serde_json::from_value::<Manifest>(conflict).is_err());
}

#[test]
fn power_aliases_cannot_evade_policy_or_change_module_consent() {
    let mut value = manifest();
    value["capabilities"] = json!(["application_command","application_files"]);
    let neutral: Manifest = serde_json::from_value(value.clone()).unwrap();
    value["capabilities"] = json!(["game_command","game_files"]);
    let legacy: Manifest = serde_json::from_value(value).unwrap();
    assert_eq!(neutral.capabilities, legacy.capabilities);
    let old: Power = serde_json::from_str("\"game_memory_write\"").unwrap();
    let new: Power = serde_json::from_str("\"application_memory_write\"").unwrap();
    assert_eq!(old, new);
    let mut rule = Policies::baseline().rules.remove(0);
    rule.deny = vec!["application_command".into(), "application_memory_write".into()];
    let powers = vec!["game_command".into(), "game_memory_write".into()];
    assert_eq!(rule.hit(&Subject { id:"probe", creator:None, powers:&powers }).unwrap().powers.len(), 2);
    rule.deny = powers;
    let powers = vec!["application_command".into(), "application_memory_write".into()];
    assert_eq!(rule.hit(&Subject { id:"probe", creator:None, powers:&powers }).unwrap().powers.len(), 2);
}

#[test]
fn neutral_modules_read_both_power_spellings_and_keep_legacy_output() {
    let mut value = json!({"schema":2,"id":"p42.status-data","name":"Status","version":"1.0.0","kind":"data",
        "applications":["msfs2024"],"provides":[],"powers":["application_memory_read"],
        "license":"CC0-1.0","source":{"repository":"Feature-Machine/p42-feature-machine","commit":"a".repeat(40)}});
    let parsed: ModuleManifest = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(parsed.powers, [Power::GameMemoryRead]);
    assert!(serde_json::to_value(&parsed).unwrap().get("applications").is_some());
    value["schema"] = json!(1);
    value["games"] = value["applications"].take();
    value.as_object_mut().unwrap().remove("applications");
    value["powers"] = json!(["game_memory_read"]);
    let parsed: ModuleManifest = serde_json::from_value(value).unwrap();
    let old = serde_json::to_value(parsed).unwrap();
    assert!(old.get("games").is_some());
    assert_eq!(old["powers"], json!(["game_memory_read"]));
}
