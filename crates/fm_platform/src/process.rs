// Process list for "is the game running". sysinfo is the common cross-platform crate for this
// (Windows, Linux, macOS), so this crate has no per-OS process code of its own.
use std::ffi::OsString;
use std::path::PathBuf;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

#[derive(Clone, Debug)]
pub struct ProcInfo {
    pub pid: u32,
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
}

impl Default for ProcessList {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessList {
    pub fn new() -> Self {
        ProcessList { sys: System::new() }
    }

    pub fn refresh(&mut self) -> Vec<ProcInfo> {
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().without_tasks().with_exe(UpdateKind::OnlyIfNotSet).with_cmd(UpdateKind::OnlyIfNotSet),
        );
        self.sys
            .processes()
            .iter()
            .filter(|(_, p)| p.thread_kind().is_none())
            .map(|(pid, p)| ProcInfo {
                pid: pid.as_u32(),
                name: p.name().to_os_string(),
                exe: p.exe().filter(|e| !e.as_os_str().is_empty()).map(|e| e.to_path_buf()),
                cmd: p.cmd().to_vec(),
            })
            .collect()
    }
}

/// One snapshot of all processes.
pub fn snapshot() -> Vec<ProcInfo> {
    ProcessList::new().refresh()
}

#[cfg(test)]
mod tests {
    #[test]
    fn snapshot_contains_this_process() {
        let me = std::process::id();
        let procs = super::snapshot();
        let p = procs.iter().find(|p| p.pid == me).expect("own process in snapshot");
        let exe = std::env::current_exe().unwrap();
        assert!(crate::games::same_path(p.exe.as_deref().expect("own exe path"), &exe));
    }
}
