// Test help for features (cargo feature "testing", for dev-dependencies only). DirFiles is the
// user files service on a test folder: the same list rules and file work as the hosts
// (files::Service), so a feature test also proves that its keys fit its feature.json.
use crate::files::{Service, UserFile};
use crate::{FileInfo, FileKind, HostError, UserFiles};
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

/// `root` stands for the user folder of every game; `list` is the feature's user_files.
pub struct DirFiles {
    pub root: PathBuf,
    pub list: Vec<UserFile>,
}

impl DirFiles {
    /// The list of a feature.json text (its "user_files" field).
    pub fn from_manifest(root: PathBuf, manifest_json: &str) -> DirFiles {
        let v: serde_json::Value = serde_json::from_str(manifest_json).expect("feature.json");
        let list = serde_json::from_value(v["user_files"].clone()).expect("user_files");
        DirFiles { root, list }
    }

    fn with<T>(&self, f: impl FnOnce(&Service) -> Result<T, (i32, String)>) -> Result<T, HostError> {
        let root = self.root.clone();
        let root_of = move |_: &str| Ok(root.clone());
        f(&Service { list: &self.list, scope: None, root: &root_of }).map_err(|(code, _)| HostError::from_code(code as i64))
    }
}

impl UserFiles for DirFiles {
    fn file_info(&self, key: &str) -> Result<FileInfo, HostError> {
        let i = self.with(|s| s.info(key))?;
        let kind = match i.kind {
            crate::abi::FILE_FILE => FileKind::File,
            crate::abi::FILE_FOLDER => FileKind::Folder,
            _ => FileKind::None,
        };
        Ok(FileInfo { kind, size: i.size, modified: (i.modified_us > 0).then(|| UNIX_EPOCH + Duration::from_micros(i.modified_us)) })
    }

    fn file_read(&self, key: &str, offset: u64, max: usize) -> Result<Vec<u8>, HostError> {
        let mut buf = vec![0u8; max.min(crate::files::MAX_BYTES)];
        let n = self.with(|s| s.read(key, offset, &mut buf))?;
        buf.truncate(n);
        Ok(buf)
    }

    fn file_write(&self, key: &str, bytes: &[u8]) -> Result<(), HostError> {
        self.with(|s| s.write(key, bytes))
    }
}
