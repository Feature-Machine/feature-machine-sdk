// File writes that must never leave half a file: the game plugin, the install record, the
// creator projects and the MCP token. Write to a temporary file in the same folder, flush, then
// rename over the target (a rename in one folder is atomic on NTFS and on Linux file systems).
// write_private also limits the file to the current user on Unix (Windows: the per-user app
// data folder already has a user-only ACL).
use std::io;
use std::path::{Path, PathBuf};

/// Writes next to the target, then renames over it. The rename fails while a game holds the old
/// DLL open, so a running game keeps its file and the temporary file is removed. The folder is
/// created first: a game with no other plugin has no plugins folder.
pub fn write_replace(target: &Path, bytes: &[u8]) -> io::Result<()> {
    write_with(target, bytes, false)
}

/// Same as write_replace, for secrets: on Unix the file gets mode 0600 before any byte is in it.
pub fn write_private(target: &Path, bytes: &[u8]) -> io::Result<()> {
    write_with(target, bytes, true)
}

fn write_with(target: &Path, bytes: &[u8], private: bool) -> io::Result<()> {
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = target.as_os_str().to_owned();
    tmp.push(".fm-new");
    let tmp = PathBuf::from(tmp);
    let result = (|| {
        let mut f = create(&tmp, private)?;
        io::Write::write_all(&mut f, bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(unix)]
fn create(path: &Path, private: bool) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    if private {
        o.mode(0o600);
    }
    o.open(path)
}

#[cfg(not(unix))]
fn create(path: &Path, _private: bool) -> io::Result<std::fs::File> {
    std::fs::File::create(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_file_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("sub").join("a.json");
        write_replace(&f, b"one").unwrap();
        write_private(&f, b"two").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"two");
        assert!(!dir.path().join("sub").join("a.json.fm-new").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
}
