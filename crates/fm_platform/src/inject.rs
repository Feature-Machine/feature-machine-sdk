// Loads a DLL into another process (feature packages plan, decision 11: the app puts the MSFS
// host into the sim). Standard method: write the DLL path into the process, then a remote thread
// runs LoadLibraryW on it. kernel32 has the same address in every process of a boot, so the
// local LoadLibraryW address is valid there. Windows only; same user, no elevation needed.
use std::path::Path;

/// True when a module with this file name (for example "fm_msfs_host.dll") is in the process.
#[cfg(windows)]
pub fn module_loaded(pid: u32, file_name: &str) -> Result<bool, String> {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_BAD_LENGTH, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Module32FirstW, Module32NextW, MODULEENTRY32W, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32};
    // ERROR_BAD_LENGTH: the process changed its module list during the snapshot; try again.
    let snap = (0..5)
        .map(|_| unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid) })
        .find(|h| *h != INVALID_HANDLE_VALUE || unsafe { GetLastError() } != ERROR_BAD_LENGTH)
        .unwrap_or(INVALID_HANDLE_VALUE);
    if snap == INVALID_HANDLE_VALUE {
        return Err(format!("module list of process {pid}: error {}", unsafe { GetLastError() }));
    }
    let mut e: MODULEENTRY32W = unsafe { std::mem::zeroed() };
    e.dwSize = std::mem::size_of::<MODULEENTRY32W>() as u32;
    let mut found = false;
    let mut ok = unsafe { Module32FirstW(snap, &mut e) } != 0;
    while ok {
        let n = e.szModule.iter().position(|c| *c == 0).unwrap_or(e.szModule.len());
        if String::from_utf16_lossy(&e.szModule[..n]).eq_ignore_ascii_case(file_name) {
            found = true;
            break;
        }
        ok = unsafe { Module32NextW(snap, &mut e) } != 0;
    }
    unsafe { CloseHandle(snap) };
    Ok(found)
}

/// Runs LoadLibraryW(dll) in the process and waits up to 10 s for it. The DLL must be in a folder
/// that the process can read. Success means the loader returned a module (DllMain returned TRUE).
#[cfg(windows)]
pub fn load_library(pid: u32, dll: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows_sys::Win32::System::Memory::{VirtualAllocEx, VirtualFreeEx, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE};
    use windows_sys::Win32::System::Threading::{
        CreateRemoteThread, GetExitCodeThread, OpenProcess, WaitForSingleObject, PROCESS_CREATE_THREAD, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
    };
    let err = |what: &str| format!("{what} (process {pid}): error {}", unsafe { GetLastError() });
    let path: Vec<u16> = dll.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let bytes = path.len() * 2;
    let proc = unsafe { OpenProcess(PROCESS_CREATE_THREAD | PROCESS_QUERY_INFORMATION | PROCESS_VM_OPERATION | PROCESS_VM_WRITE | PROCESS_VM_READ, 0, pid) };
    if proc.is_null() {
        return Err(err("OpenProcess"));
    }
    let result = (|| {
        let kernel32: Vec<u16> = "kernel32.dll\0".encode_utf16().collect();
        let load = unsafe { GetProcAddress(GetModuleHandleW(kernel32.as_ptr()), c"LoadLibraryW".as_ptr() as *const u8) }.ok_or_else(|| err("GetProcAddress LoadLibraryW"))?;
        let mem = unsafe { VirtualAllocEx(proc, std::ptr::null(), bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE) };
        if mem.is_null() {
            return Err(err("VirtualAllocEx"));
        }
        let r = (|| {
            if unsafe { WriteProcessMemory(proc, mem, path.as_ptr() as *const _, bytes, std::ptr::null_mut()) } == 0 {
                return Err(err("WriteProcessMemory"));
            }
            // LoadLibraryW has the thread start routine shape: one pointer in, a value out.
            let start: unsafe extern "system" fn(*mut core::ffi::c_void) -> u32 = unsafe { std::mem::transmute(load) };
            let thread = unsafe { CreateRemoteThread(proc, std::ptr::null(), 0, Some(start), mem, 0, std::ptr::null_mut()) };
            if thread.is_null() {
                return Err(err("CreateRemoteThread"));
            }
            let waited = unsafe { WaitForSingleObject(thread, 10_000) };
            let mut code = 0u32;
            let got = unsafe { GetExitCodeThread(thread, &mut code) };
            unsafe { CloseHandle(thread) };
            if waited != WAIT_OBJECT_0 {
                // The thread can still run and read the path: keep the memory (a few hundred bytes).
                return Err(format!("LoadLibraryW did not end in 10 s (process {pid})"));
            }
            // The exit code is the low 32 bits of the module handle: 0 means the load failed.
            if got == 0 || code == 0 {
                return Err(format!("LoadLibraryW failed in process {pid} for {}", dll.display()));
            }
            Ok(())
        })();
        if !matches!(&r, Err(e) if e.contains("did not end")) {
            unsafe { VirtualFreeEx(proc, mem, 0, MEM_RELEASE) };
        }
        r
    })();
    unsafe { CloseHandle(proc) };
    result
}

/// A copy of `src` under `root/<first 16 hex of its SHA-256>/`. A loaded DLL stays locked until
/// the process ends, so the process gets this copy, never the file in the app's version folder
/// (an app update must be able to remove that folder while the sim runs). A copy that differs
/// from `src` is written again. Copies of other versions are deleted; a copy that a running sim
/// still has loaded is locked, so it stays until a later call.
pub fn staged_copy(src: &Path, root: &Path) -> Result<std::path::PathBuf, String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let hash: String = Sha256::digest(&bytes).iter().take(8).map(|b| format!("{b:02x}")).collect();
    let name = src.file_name().ok_or("no file name")?;
    let dest = root.join(&hash).join(name);
    if std::fs::read(&dest).ok().as_deref() != Some(&bytes[..]) {
        std::fs::create_dir_all(dest.parent().unwrap()).map_err(|e| e.to_string())?;
        crate::files::write_replace(&dest, &bytes).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    for old in std::fs::read_dir(root).into_iter().flatten().flatten().filter(|e| e.file_name() != hash.as_str()) {
        let _ = std::fs::remove_dir_all(old.path());
    }
    Ok(dest)
}

#[cfg(not(windows))]
pub fn module_loaded(_pid: u32, _file_name: &str) -> Result<bool, String> {
    Err("DLL injection is Windows only".into())
}

#[cfg(not(windows))]
pub fn load_library(_pid: u32, _dll: &Path) -> Result<(), String> {
    Err("DLL injection is Windows only".into())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    // Loads a system DLL that a plain console process does not load into a child process (ping,
    // which waits), and finds it in the module list. Tests the method with no sim and no own DLL.
    #[test]
    fn loads_a_dll_into_another_process() {
        let mut child = std::process::Command::new("ping").args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let dll = std::path::PathBuf::from(std::env::var("SystemRoot").unwrap()).join("System32").join("winmm.dll");
        let before = module_loaded(pid, "winmm.dll").unwrap();
        let r = load_library(pid, &dll);
        let after = module_loaded(pid, "winmm.dll");
        let _ = child.kill();
        let _ = child.wait();
        assert!(!before, "ping does not load winmm.dll itself");
        r.unwrap();
        assert!(after.unwrap(), "the module is in the process after the load");
        assert!(load_library(pid, &dll).is_err(), "a process that ended refuses");
    }

    #[test]
    fn staged_copy_keeps_only_the_current_version() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("h.dll");
        std::fs::write(&src, "one").unwrap();
        let a = staged_copy(&src, &t.path().join("stage")).unwrap();
        assert_eq!(staged_copy(&src, &t.path().join("stage")).unwrap(), a);
        std::fs::write(&src, "two").unwrap();
        let b = staged_copy(&src, &t.path().join("stage")).unwrap();
        assert_ne!(a, b);
        assert!(!a.exists(), "the copy of the old version is deleted");
        assert_eq!(b.file_name().unwrap(), "h.dll");
        std::fs::write(&b, "changed").unwrap();
        assert_eq!(std::fs::read(staged_copy(&src, &t.path().join("stage")).unwrap()).unwrap(), b"two", "a changed copy is written again");
    }
}
