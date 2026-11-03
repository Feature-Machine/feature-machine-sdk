// File writes that must never leave half a file: the game plugin, the install record, the
// creator projects, the MCP token and the module trust files. Write to a temporary file in the
// same folder, flush it, then replace the target in one durable step (replace below).
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
    // A name of its own per write (process and counter): two writers of one file never share a
    // temporary file, so one cannot truncate, rename or delete the half-written file of the other.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut tmp = target.as_os_str().to_owned();
    tmp.push(format!(".fm-new-{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let tmp = PathBuf::from(tmp);
    let result = (|| {
        let mut f = create(&tmp, private)?;
        io::Write::write_all(&mut f, bytes)?;
        f.sync_all()?;
        drop(f);
        replace(&tmp, target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Renames `from` over `to` and makes the rename durable (Sol r2 R2-4): sync_all of the file
/// does not write the new folder entry, so the folder is flushed after the rename. The rename
/// stays std::fs::rename (POSIX semantics on Windows): MoveFileExW cannot replace a file that a
/// reader holds open, and the host reads active.json and the module files at any time.
#[cfg(windows)]
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::rename(from, to)?;
    let dir = to.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    // A folder opens only with FILE_FLAG_BACKUP_SEMANTICS; FlushFileBuffers needs write access.
    std::fs::OpenOptions::new().write(true).custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS).open(dir)?.sync_all()
}

#[cfg(not(windows))]
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)?;
    let dir = to.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::File::open(dir)?.sync_all()
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
        let names: Vec<_> = std::fs::read_dir(dir.path().join("sub")).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["a.json"], "no temporary file stays");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    // Sol r2 R2-4: the durable replace keeps the rules of the rename. It replaces a file that a
    // reader holds open (the host reads active.json and the module files), and it fails with no
    // change when the target is a folder. Durability itself needs a power cut to show.
    #[test]
    fn the_durable_replace_works_like_the_rename() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.json");
        write_replace(&f, b"one").unwrap();
        let open = std::fs::File::open(&f).unwrap();
        write_replace(&f, b"two").unwrap();
        drop(open);
        assert_eq!(std::fs::read(&f).unwrap(), b"two");
        std::fs::create_dir(dir.path().join("d")).unwrap();
        assert!(write_replace(&dir.path().join("d"), b"x").is_err());
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 2, "no temporary file stays: {names:?}");
    }
}
