// OS part of the app install (plan M8): where the app goes, how the OS knows it (Windows: the
// Apps entry, the featuremachine:// link and a Start menu shortcut, all for the current user;
// Linux: a desktop file with the link handler and an Uninstall action), and how the setup
// closes a running app. The install pattern itself is OS-neutral (crate fm_install).
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const PRODUCT: &str = "Feature Machine";
/// The company in the Apps entry, the same name as on the signing certificate (Parallel 42 LLC).
pub const PUBLISHER: &str = "Parallel 42";
pub const LINK_SCHEME: &str = "featuremachine";

/// Install root: the per-user program folder on Windows, the XDG data folder on Linux.
pub fn install_root() -> Option<PathBuf> {
    if cfg!(windows) {
        dirs_local().map(|d| d.join("Programs").join(PRODUCT))
    } else {
        dirs_local().map(|d| d.join("feature-machine"))
    }
}

fn dirs_local() -> Option<PathBuf> {
    // fm_paths uses the dirs crate for the same folder (LOCALAPPDATA, XDG_DATA_HOME).
    fm_paths::data_dir().and_then(|d| d.parent().map(Path::to_path_buf))
}

/// The launcher in the install root (shortcuts and the link point to it, never to a version).
pub fn launcher_name() -> &'static str {
    if cfg!(windows) {
        "Feature Machine.exe"
    } else {
        "feature-machine"
    }
}

/// The feature lifecycle helper in resources/ of each app version (the app's hooks.rs starts it).
pub fn hook_helper_name() -> &'static str {
    if cfg!(windows) {
        "fm_feature_hook.exe"
    } else {
        "fm_feature_hook"
    }
}

/// The input process in resources/ of each app version (the app's controls.rs starts it).
pub fn input_reader_name() -> &'static str {
    if cfg!(windows) {
        "fm_input_reader.exe"
    } else {
        "fm_input_reader"
    }
}

/// What the OS shows for the installed app.
pub struct AppEntry<'a> {
    pub root: &'a Path,
    pub launcher: &'a Path,
    pub version: &'a str,
    /// Icon file (Linux desktop file); Windows uses the launcher's own icon.
    pub icon: Option<&'a Path>,
}

/// Process IDs of programs that run from `root` (the app, its MSFS input guard, a launcher).
pub fn processes_under(root: &Path) -> Vec<u32> {
    let me = std::process::id();
    let root = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    crate::process::snapshot()
        .into_iter()
        .filter(|p| p.pid != me && p.exe.as_deref().is_some_and(|e| dunce::canonicalize(e).unwrap_or_else(|_| e.to_path_buf()).starts_with(&root)))
        .map(|p| p.pid)
        .collect()
}

/// Waits until none of `pids` runs. Returns false at the timeout.
pub fn wait_gone(pids: &[u32], timeout: Duration) -> bool {
    let end = Instant::now() + timeout;
    loop {
        let alive: Vec<u32> = crate::process::snapshot().into_iter().map(|p| p.pid).filter(|p| pids.contains(p)).collect();
        if alive.is_empty() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Ends a process at once (only after a graceful quit had its time).
pub fn kill(pid: u32) {
    let mut sys = sysinfo::System::new();
    let p = sysinfo::Pid::from_u32(pid);
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[p]), true);
    if let Some(proc_) = sys.process(p) {
        proc_.kill();
    }
}

#[cfg(windows)]
pub use windows_impl::{delete_after_exit, register, unregister};
#[cfg(target_os = "linux")]
pub use linux_impl::{delete_after_exit, register, unregister};

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\FeatureMachine";

    fn classes_key() -> String {
        format!(r"Software\Classes\{LINK_SCHEME}")
    }

    fn shortcut() -> Option<PathBuf> {
        std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join(r"Microsoft\Windows\Start Menu\Programs").join(format!("{PRODUCT}.lnk")))
    }

    fn size_kb(dir: &Path) -> u64 {
        fn walk(d: &Path) -> u64 {
            std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| if e.path().is_dir() { walk(&e.path()) } else { e.metadata().map_or(0, |m| m.len()) }).sum()
        }
        walk(dir) / 1024
    }

    /// Writes the Apps entry, the link and the Start menu shortcut (current user only).
    pub fn register(e: &AppEntry) -> io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let launcher = e.launcher.display().to_string();
        let (k, _) = hkcu.create_subkey(UNINSTALL_KEY)?;
        k.set_value("DisplayName", &PRODUCT)?;
        k.set_value("DisplayVersion", &e.version)?;
        k.set_value("Publisher", &PUBLISHER)?;
        k.set_value("InstallLocation", &e.root.display().to_string())?;
        k.set_value("DisplayIcon", &launcher)?;
        k.set_value("UninstallString", &format!("\"{launcher}\" --uninstall"))?;
        k.set_value("QuietUninstallString", &format!("\"{launcher}\" --uninstall --quiet"))?;
        k.set_value("NoModify", &1u32)?;
        k.set_value("NoRepair", &1u32)?;
        k.set_value("EstimatedSize", &(size_kb(e.root) as u32))?;
        let (c, _) = hkcu.create_subkey(classes_key())?;
        c.set_value("", &format!("URL:{PRODUCT}"))?;
        c.set_value("URL Protocol", &"")?;
        let (icon, _) = c.create_subkey("DefaultIcon")?;
        icon.set_value("", &format!("\"{launcher}\",0"))?;
        let (cmd, _) = c.create_subkey(r"shell\open\command")?;
        cmd.set_value("", &format!("\"{launcher}\" \"%1\""))?;
        if let Some(lnk) = shortcut() {
            make_shortcut(&lnk, e.launcher)?;
        }
        Ok(())
    }

    /// Removes what register wrote. A part that is already gone is not an error.
    pub fn unregister() -> io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        for key in [UNINSTALL_KEY.to_string(), classes_key()] {
            match hkcu.delete_subkey_all(&key) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        if let Some(lnk) = shortcut() {
            match std::fs::remove_file(lnk) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }

    /// Start menu shortcut through the Shell COM interface (IShellLinkW, IPersistFile).
    fn make_shortcut(lnk: &Path, target: &Path) -> io::Result<()> {
        use windows::core::{Interface, HSTRING};
        use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
        use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
        let err = |e: windows::core::Error| io::Error::other(e.to_string());
        if let Some(dir) = lnk.parent() {
            std::fs::create_dir_all(dir)?;
        }
        unsafe {
            let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let result = (|| {
                let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(err)?;
                link.SetPath(&HSTRING::from(target.as_os_str())).map_err(err)?;
                if let Some(dir) = target.parent() {
                    link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str())).map_err(err)?;
                }
                link.SetDescription(&HSTRING::from(PRODUCT)).map_err(err)?;
                link.cast::<IPersistFile>().map_err(err)?.Save(&HSTRING::from(lnk.as_os_str()), true).map_err(err)
            })();
            if init.is_ok() {
                CoUninitialize();
            }
            result
        }
    }

    /// Windows cannot delete a running exe (the uninstaller copy in TEMP). A hidden cmd waits
    /// 3 seconds, so this process has ended, deletes `path`, then its folder if it is empty
    /// (rmdir without /s never removes other files). raw_arg: cmd does not read the \" escapes
    /// that Command::arg writes, so the line goes to cmd as written.
    pub fn delete_after_exit(path: &Path) -> io::Result<()> {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let dir = path.parent().unwrap_or(path);
        let line = format!("/d /c ping -n 4 127.0.0.1 >nul & del /f /q \"{}\" & rmdir \"{}\"", path.display(), dir.display());
        std::process::Command::new("cmd").raw_arg(line).creation_flags(CREATE_NO_WINDOW).spawn().map(|_| ())
    }

    #[cfg(test)]
    mod tests {
        // The real call, on files in folders with a space (the TEMP copy folder can have one):
        // the file and its empty folder go; a folder with another file in it stays.
        #[test]
        fn delete_after_exit_deletes_the_file_and_its_empty_folder() {
            let dir = tempfile::tempdir().unwrap();
            let alone = dir.path().join("a b").join("copy.exe");
            let shared = dir.path().join("c d").join("copy.exe");
            for f in [&alone, &shared] {
                std::fs::create_dir_all(f.parent().unwrap()).unwrap();
                std::fs::write(f, b"x").unwrap();
                super::delete_after_exit(f).unwrap();
            }
            std::fs::write(dir.path().join("c d").join("other.txt"), b"keep").unwrap();
            let end = std::time::Instant::now() + std::time::Duration::from_secs(15);
            while (alone.parent().unwrap().exists() || shared.exists()) && std::time::Instant::now() < end {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            assert!(!alone.parent().unwrap().exists());
            assert!(!shared.exists());
            assert!(dir.path().join("c d").join("other.txt").exists());
        }
    }
}

#[cfg(target_os = "linux")]
mod linux_impl {
    use super::*;

    fn desktop_file() -> Option<PathBuf> {
        dirs_local().map(|d| d.join("applications").join("feature-machine.desktop"))
    }

    /// Quotes a path for a desktop file Exec line. Freedesktop rules: inside double quotes \ " ` $
    /// get a backslash; then the string-value rule doubles each backslash (so \ becomes four);
    /// a % is a field code, so a literal % is %%.
    fn exec_arg(p: &Path) -> String {
        let s = p.display().to_string();
        let quoted: String = s.chars().flat_map(|c| if matches!(c, '"' | '`' | '$' | '\\') { vec!['\\', c] } else { vec![c] }).collect();
        format!("\"{}\"", quoted.replace('\\', "\\\\").replace('%', "%%"))
    }

    /// Writes the desktop file (menu entry, link handler, Uninstall action) and makes it the
    /// handler of featuremachine:// (xdg-mime). Missing xdg tools only skip that last step.
    pub fn register(e: &AppEntry) -> io::Result<()> {
        let file = desktop_file().ok_or_else(|| io::Error::other("no XDG data folder"))?;
        std::fs::create_dir_all(file.parent().unwrap())?;
        let exec = exec_arg(e.launcher);
        let icon = e.icon.map(|i| format!("Icon={}\n", i.display())).unwrap_or_default();
        let text = format!(
            "[Desktop Entry]\nType=Application\nName={PRODUCT}\nComment=Mods for your games\nExec={exec} %u\n{icon}Terminal=false\nCategories=Game;Utility;\nMimeType=x-scheme-handler/{LINK_SCHEME};\nX-FeatureMachine-Version={}\nActions=Uninstall;\n\n[Desktop Action Uninstall]\nName=Uninstall {PRODUCT}\nExec={exec} --uninstall\n",
            e.version
        );
        crate::files::write_replace(&file, text.as_bytes())?;
        let dir = file.parent().unwrap().display().to_string();
        let _ = std::process::Command::new("update-desktop-database").arg(&dir).status();
        let _ = std::process::Command::new("xdg-mime").args(["default", "feature-machine.desktop", &format!("x-scheme-handler/{LINK_SCHEME}")]).status();
        Ok(())
    }

    pub fn unregister() -> io::Result<()> {
        if let Some(file) = desktop_file() {
            match std::fs::remove_file(&file) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            let _ = std::process::Command::new("update-desktop-database").arg(file.parent().unwrap()).status();
        }
        Ok(())
    }

    /// Linux deletes a running file at once; nothing to wait for.
    pub fn delete_after_exit(path: &Path) -> io::Result<()> {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn exec_paths_are_quoted() {
            assert_eq!(super::exec_arg(std::path::Path::new("/home/a b/$x\"y")), r#""/home/a b/\\$x\\"y""#);
            assert_eq!(super::exec_arg(std::path::Path::new("/a\\b/50%")), r#""/a\\\\b/50%%""#);
        }
    }
}
