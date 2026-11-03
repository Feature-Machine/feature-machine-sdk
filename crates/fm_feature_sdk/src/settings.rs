// The settings file of one feature (feature surfaces plan, sections 2 and 3). The app writes
// <features dir>/<id>/settings.json with an atomic replace; a game host only reads it, at each
// start of the feature, so a restart after a change or a link loss starts with the current
// values. It holds the member's values and, per installed version, the projection that this
// version runs (each setting of its own schema; the app fills defaults). One file per feature, so
// a removal of the feature folder removes it too.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const FILE: &str = "settings.json";
/// The largest file that a host reads. 32 settings with text of 256 bytes, for a few versions.
pub const FILE_LIMIT: u64 = 256 * 1024;
/// The largest projection (JSON text) that a host gives a feature, at start or in an update.
/// It covers every valid projection: 32 settings with ids of 32 bytes and text of 256 bytes
/// that is all escaped quotes are about 18 KB (fm_install surfaces test). The app refuses a
/// write that gives a larger one, so a host never drops values that the member saved.
pub const VALUES_LIMIT: usize = 32 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SettingsFile {
    /// Grows with each change of the member; a host reports the revision that a start runs.
    pub revision: u64,
    /// The member's values by setting id (any version's ids; a projection picks its own).
    #[serde(default)]
    pub values: Map<String, Value>,
    /// Ids that the member changed (a migration never replaces them).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub touched: BTreeSet<String>,
    /// Ids that a feature's old saved value filled once (migration, section 2).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub imported: BTreeSet<String>,
    /// The full projection of each installed version that has settings.
    #[serde(default)]
    pub versions: BTreeMap<String, Map<String, Value>>,
}

/// <features dir>/<id>/settings.json, next to the feature's data and versions folders.
pub fn path(features_dir: &Path, id: &str) -> PathBuf {
    features_dir.join(id).join(FILE)
}

impl SettingsFile {
    /// A missing file is no settings (revision 0); a damaged or too large one is an error that
    /// the host logs, and the feature starts with no values (it uses its own defaults).
    pub fn load(file: &Path) -> Result<SettingsFile, String> {
        let bytes = match std::fs::metadata(file) {
            Ok(m) if m.len() > FILE_LIMIT => return Err(format!("{}: larger than {FILE_LIMIT} bytes", file.display())),
            Ok(_) => std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(SettingsFile::default()),
            Err(e) => return Err(format!("{}: {e}", file.display())),
        };
        serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", file.display()))
    }

    /// What a start of `version` runs: the revision and the projection as JSON text. A version
    /// with no usable projection (none in the file, or too large) gets "{}" at revision 0: the
    /// feature uses its own defaults, and the report never says that it runs the stored values.
    pub fn start(&self, version: &str) -> (u64, String) {
        match self.versions.get(version).map(|m| Value::Object(m.clone()).to_string()).filter(|t| t.len() <= VALUES_LIMIT) {
            Some(values) => (self.revision, values),
            None => (0, "{}".into()),
        }
    }
}

/// A JSON object of at most VALUES_LIMIT bytes: the only form a host gives a feature.
pub fn valid_values(json: &str) -> bool {
    json.len() <= VALUES_LIMIT && matches!(serde_json::from_str::<Value>(json), Ok(Value::Object(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_start_gets_the_projection_of_its_version() {
        let t = tempfile::tempdir().unwrap();
        let file = path(t.path(), "rain");
        assert_eq!(SettingsFile::load(&file).unwrap().start("1.0.0"), (0, "{}".into()), "no file: no values");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, r#"{"revision":4,"values":{"strength":0.7},"versions":{"1.0.0":{"strength":0.7},"0.9.0":{"strength":0.5}}}"#).unwrap();
        let s = SettingsFile::load(&file).unwrap();
        assert_eq!(s.start("1.0.0"), (4, r#"{"strength":0.7}"#.into()));
        assert_eq!(s.start("0.9.0"), (4, r#"{"strength":0.5}"#.into()));
        assert_eq!(s.start("2.0.0"), (0, "{}".into()), "no projection: revision 0, not 4");
        let big = format!(r#"{{"revision":4,"versions":{{"1.0.0":{{"a":"{}"}}}}}}"#, "x".repeat(VALUES_LIMIT));
        std::fs::write(&file, big).unwrap();
        assert_eq!(SettingsFile::load(&file).unwrap().start("1.0.0"), (0, "{}".into()), "a dropped projection is not applied");
        std::fs::write(&file, "not json").unwrap();
        assert!(SettingsFile::load(&file).is_err());
        std::fs::write(&file, vec![b' '; FILE_LIMIT as usize + 1]).unwrap();
        assert!(SettingsFile::load(&file).unwrap_err().contains("larger than"));
        assert!(valid_values("{}") && !valid_values("[]") && !valid_values("1") && !valid_values(&format!("{{\"a\":\"{}\"}}", "x".repeat(VALUES_LIMIT))));
    }
}
