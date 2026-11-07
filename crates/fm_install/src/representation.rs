// One conversion owns version 2 manifest names. Version 1 serialization stays compatible;
// typed enums normalize equivalent powers before policy and consent comparisons.
use serde_json::Value;
pub(crate) fn neutral_manifest(value: &mut Value) {
    let Some(object) = value.as_object_mut() else { return };
    for (old, new) in [("games", "applications"), ("game_commands", "application_commands")] {
        if let Some(value) = object.remove(old) { object.insert(new.into(), value); }
    }
    if let Some(files) = object.get_mut("user_files").and_then(Value::as_array_mut) {
        for file in files { if let Some(object) = file.as_object_mut() { if let Some(value) = object.remove("game") { object.insert("application".into(), value); } } }
    }
    for field in ["capabilities", "powers"] {
        if let Some(powers) = object.get_mut(field).and_then(Value::as_array_mut) {
            for power in powers { if let Some(text) = power.as_str() { *power = fm_feature_sdk::canonical_power(text).into(); } }
        }
    }
}
