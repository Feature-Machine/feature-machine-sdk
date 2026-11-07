// Discovery uses sysinfo without CPU or memory sampling. Windows also supplies the exact
// process start token, so a reused PID cannot inherit an earlier application session.
use std::ffi::OsString;
use std::path::PathBuf;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

#[cfg(windows)]
mod windows;

#[derive(Clone, Debug)]
pub struct ProcInfo {
    pub pid: u32,
    pub start_id: u64,
    pub name: OsString,
    /// None when the OS does not give the path (other user, protected or elevated process).
    pub exe: Option<PathBuf>,
    /// Command line, first item the program. Empty when the OS does not give it.
    pub cmd: Vec<OsString>,
}

/// Process list kept between refreshes: the EXE path and command line of a known process are
/// not asked again (OnlyIfNotSet), and dead processes are removed. No CPU, memory or thread
/// data. Linux threads are skipped, so a PID is always a process. sysinfo gives an empty path
/// when Windows refuses it (elevated game, client not elevated): that is None.
pub struct ProcessList {
    sys: System,
    known: Identities,
}

impl Default for ProcessList {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessList {
    pub fn new() -> Self {
        ProcessList { sys: System::new(), known: Identities::default() }
    }

    pub fn refresh(&mut self) -> Vec<ProcInfo> {
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().without_tasks().with_exe(UpdateKind::OnlyIfNotSet).with_cmd(UpdateKind::OnlyIfNotSet),
        );
        let known = &mut self.known;
        let out: Vec<ProcInfo> = self.sys
            .processes()
            .iter()
            .filter(|(_, p)| p.thread_kind().is_none())
            .map(|(pid, p)| {
                let (start_id, exact) = known.get(pid.as_u32(), p.start_time(), exact_identity);
                ProcInfo {
                pid: pid.as_u32(),
                start_id,
                name: p.name().to_os_string(),
                exe: exact.or_else(|| p.exe().filter(|e| !e.as_os_str().is_empty()).map(|e| e.to_path_buf())),
                cmd: p.cmd().to_vec(),
            }})
            .collect();
        known.retain(&out);
        out
    }
}

#[cfg(windows)]
fn exact_identity(pid: u32) -> Option<(PathBuf, u64)> { identity_handle(pid).ok() }
#[cfg(not(windows))]
fn exact_identity(_: u32) -> Option<(PathBuf, u64)> { None }

/// The start identity of each live process, read once when its PID first appears (review A5).
/// A later poll reuses it, so a transient OpenProcess failure cannot swap the FILETIME token
/// for sysinfo seconds and restart a game session. A new sysinfo start time is a reused PID.
#[derive(Default)]
struct Identities(std::collections::HashMap<u32, (u64, u64, Option<PathBuf>)>);

impl Identities {
    fn get(&mut self, pid: u32, seconds: u64, exact: impl FnOnce(u32) -> Option<(PathBuf, u64)>) -> (u64, Option<PathBuf>) {
        if self.0.get(&pid).is_none_or(|known| known.0 != seconds) {
            let found = exact(pid);
            self.0.insert(pid, (seconds, found.as_ref().map_or(seconds, |f| f.1), found.map(|f| f.0)));
        }
        let (_, start, exe) = &self.0[&pid];
        (*start, exe.clone())
    }

    fn retain(&mut self, live: &[ProcInfo]) {
        let pids: std::collections::HashSet<u32> = live.iter().map(|p| p.pid).collect();
        self.0.retain(|pid, _| pids.contains(pid));
    }
}

/// One snapshot of all processes.
pub fn snapshot() -> Vec<ProcInfo> {
    ProcessList::new().refresh()
}

#[cfg(windows)]
/// Keep exact identity for protected service processes without asking for more rights. The
/// native fallback checks the birth token around its image query to reject a reused PID.
pub fn identity(pid: u32) -> std::io::Result<(std::path::PathBuf, u64)> {
    identity_handle(pid).or_else(|error| {
        if error.kind() == std::io::ErrorKind::PermissionDenied { windows::identity(pid) } else { Err(error) }
    })
}

#[cfg(windows)]
fn identity_handle(pid: u32) -> std::io::Result<(std::path::PathBuf, u64)> {
    use std::{io, path::PathBuf};
    use windows_sys::Win32::{Foundation::*, System::Threading::*};
    // SAFETY: each successful OpenProcess is closed once below; buffers and FILETIMEs are live.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() { return Err(io::Error::last_os_error()); }
        let (mut created, mut ended, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let mut buf = vec![0u16; 32768];
        let mut n = buf.len() as u32;
        let ok = GetProcessTimes(handle, &mut created, &mut ended, &mut kernel, &mut user) != 0
            && QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut n) != 0;
        let err = io::Error::last_os_error();
        CloseHandle(handle);
        if !ok { return Err(err); }
        let start = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        Ok((PathBuf::from(String::from_utf16_lossy(&buf[..n as usize])), start))
    }
}


/// Unix process identity uses the same OS observation as discovery. A missing process cannot
/// inherit the identity of an earlier helper or parent that used its PID.
#[cfg(not(windows))]
pub fn identity(pid: u32) -> std::io::Result<(std::path::PathBuf, u64)> {
    ProcessList::new().refresh().into_iter().find(|p| p.pid == pid).and_then(|p| p.exe.map(|exe| (exe, p.start_id)))
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "process identity is unavailable"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn snapshot_contains_this_process() {
        let me = std::process::id();
        let procs = super::snapshot();
        let p = procs.iter().find(|p| p.pid == me).expect("own process in snapshot");
        let exe = std::env::current_exe().unwrap();
        assert!(crate::applications::same_path(p.exe.as_deref().expect("own exe path"), &exe));
    }

    // Review A5: one exact read per process life; a failed later read cannot change units.
    #[test]
    fn a_process_identity_is_read_once_per_process_life() {
        use std::{cell::Cell, path::PathBuf};
        let mut known = super::Identities::default();
        let reads = Cell::new(0);
        let exact = |answer: Option<u64>| { let reads = &reads; move |_| { reads.set(reads.get() + 1); answer.map(|t| (PathBuf::from("/game.exe"), t)) } };
        assert_eq!(known.get(7, 100, exact(Some(133_000_000_000))), (133_000_000_000, Some(PathBuf::from("/game.exe"))));
        assert_eq!(known.get(7, 100, exact(None)), (133_000_000_000, Some(PathBuf::from("/game.exe"))), "a failed read cannot replace the token");
        assert_eq!(reads.get(), 1);
        assert_eq!(known.get(7, 160, exact(None)).0, 160, "a reused PID is a new process life");
        assert_eq!(known.get(7, 160, exact(Some(5))).0, 160, "and keeps its first unit");
        assert_eq!(reads.get(), 2);
        known.retain(&[]);
        assert_eq!(known.get(7, 160, exact(Some(5))).0, 5, "an ended process is forgotten");
    }
}
