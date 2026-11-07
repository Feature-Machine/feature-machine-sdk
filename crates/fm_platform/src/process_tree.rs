// One process-tree owner serves lifecycle hooks and resident helpers. Kill-on-close keeps
// helper children from surviving app exit; Unix callers create their own process group.
#[cfg(windows)]
pub struct Tree(windows_sys::Win32::Foundation::HANDLE);

// SAFETY: the owned job handle may move between threads; access is serialized by its owner.
#[cfg(windows)]
unsafe impl Send for Tree {}

#[cfg(windows)]
impl Tree {
    pub fn available(&self) -> bool { !self.0.is_null() }

    pub fn new(child: &std::process::Child) -> Tree {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::*;
        // SAFETY: plain Win32 calls on a new job and on the live child's handle; on a
        // failure the job is closed and the tree is empty (a null handle).
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Tree(job);
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
            let set = SetInformationJobObject(job, JobObjectExtendedLimitInformation, (&raw const info).cast(), size);
            if set == 0 || AssignProcessToJobObject(job, child.as_raw_handle()) == 0 {
                windows_sys::Win32::Foundation::CloseHandle(job);
                return Tree(std::ptr::null_mut());
            }
            Tree(job)
        }
    }

    pub fn kill(&self) {
        if !self.0.is_null() {
            // SAFETY: the job handle is ours and open until drop.
            unsafe { windows_sys::Win32::System::JobObjects::TerminateJobObject(self.0, 1) };
        }
    }
}

#[cfg(windows)]
impl Drop for Tree {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: closed once, here.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
        }
    }
}

#[cfg(unix)]
pub struct Tree(libc::pid_t);

#[cfg(unix)]
impl Tree {
    pub fn available(&self) -> bool { true }

    pub fn new(child: &std::process::Child) -> Tree {
        Tree(child.id() as libc::pid_t)
    }

    pub fn kill(&self) {
        // SAFETY: a signal to the helper's own process group (pgid = helper pid, see run).
        unsafe { libc::kill(-self.0, libc::SIGKILL) };
    }
}
