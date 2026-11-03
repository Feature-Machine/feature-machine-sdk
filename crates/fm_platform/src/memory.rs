// Windows process access for declarative maps. Reader never writes. Writer is the in-process
// adapter for the injected host: it writes and calls change handlers only inside `execute` on
// its qualified thread, so a map cannot turn a host worker into a game-thread writer.
#[cfg(windows)]
mod win {
    use fm_memory::{Error, Image, Memory, ModuleId};
    use sha2::{Digest, Sha256};
    use std::io::Read;
    use windows_sys::Win32::{Foundation::*, System::{Diagnostics::{Debug::{ReadProcessMemory, WriteProcessMemory}, ToolHelp::*}, Memory::*, Threading::*}};

    pub struct Reader { process: HANDLE, id: ModuleId, base: u64, len: usize }
    // HANDLE is used only with read/query calls, which Windows allows concurrently. Drop closes
    // it after the last owner; no raw handle is exposed to feature code.
    unsafe impl Send for Reader {}
    unsafe impl Sync for Reader {}

    impl Drop for Reader { fn drop(&mut self) { unsafe { CloseHandle(self.process); } } }

    fn error(text: &str) -> Error { Error::Unavailable(format!("{text}: Windows error {}", unsafe { GetLastError() })) }
    fn wide(text: &[u16]) -> String { String::from_utf16_lossy(&text[..text.iter().position(|c| *c == 0).unwrap_or(text.len())]) }

    impl Reader {
        /// Locate the main module only. Its file digest identifies the build; a live process
        /// handle plus its creation time identifies the session even after PID reuse.
        pub fn open(pid: u32, game: &str, module: &str) -> Result<Self, Error> {
            unsafe {
                let process = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid);
                if process.is_null() { return Err(error("OpenProcess read")); }
                let result = (|| {
                    let snap = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid);
                    if snap == INVALID_HANDLE_VALUE { return Err(error("module snapshot")); }
                    let mut entry: MODULEENTRY32W = std::mem::zeroed();
                    entry.dwSize = std::mem::size_of_val(&entry) as u32;
                    let found = Module32FirstW(snap, &mut entry) != 0 && wide(&entry.szModule).eq_ignore_ascii_case(module);
                    CloseHandle(snap);
                    if !found { return Err(Error::Unavailable("map does not name the main module".into())); }
                    // The Microsoft Store build refuses file reads from other processes (2026-10-20,
                    // test PC, 1.9.18.0). Then the identity has no digest: every exact map refuses
                    // (its digest never matches ""), and a camera locator, which needs none, still reads.
                    let digest = (|| -> std::io::Result<String> {
                        let mut file = std::fs::File::open(wide(&entry.szExePath))?;
                        let mut hash = Sha256::new();
                        let mut buf = [0; 65536];
                        loop { let n = file.read(&mut buf)?; if n == 0 { break; } hash.update(&buf[..n]); }
                        Ok(format!("{:x}", hash.finalize()))
                    })().unwrap_or_default();
                    let mut creation: FILETIME = std::mem::zeroed();
                    let mut exit: FILETIME = std::mem::zeroed();
                    let mut kernel: FILETIME = std::mem::zeroed();
                    let mut user: FILETIME = std::mem::zeroed();
                    if GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) == 0 { return Err(error("process identity")); }
                    let id = ModuleId { game: game.into(), module: wide(&entry.szModule), sha256: digest, session: ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64 };
                    let len = entry.modBaseSize as usize;
                    if len == 0 || len > 512 * 1024 * 1024 { return Err(Error::Unavailable("module exceeds snapshot limit".into())); }
                    Ok(Self { process, id, base: entry.modBaseAddr as u64, len })
                })();
                if result.is_err() { CloseHandle(process); }
                result
            }
        }

        /// Snapshot only readable image regions. Holes remain unavailable, so masked patterns
        /// cannot match zero-filled storage where the OS refused a read.
        pub fn snapshot(&self) -> Result<Image, Error> {
            let mut image = Image { base: self.base, bytes: vec![0; self.len], id: self.identity()?, readable: vec![] };
            let end = self.base.checked_add(self.len as u64).ok_or_else(|| Error::Unavailable("image overflow".into()))?;
            let mut at = self.base;
            while at < end {
                let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
                if unsafe { VirtualQueryEx(self.process, at as *const _, &mut info, std::mem::size_of_val(&info)) } == 0 { return Err(error("query image region")); }
                let next = (info.BaseAddress as u64).checked_add(info.RegionSize as u64).filter(|n| *n > at).ok_or_else(|| Error::Unavailable("invalid memory region".into()))?.min(end);
                if info.State == MEM_COMMIT && info.Protect & (PAGE_GUARD | PAGE_NOACCESS) == 0 {
                    let size = (next - at) as usize;
                    if let Ok(bytes) = self.read(at, size) {
                        let start = (at - self.base) as usize;
                        image.bytes[start..start + size].copy_from_slice(&bytes);
                        image.readable.push(start..start + size);
                    }
                }
                at = next;
            }
            Ok(image)
        }
    }

    impl Memory for Reader {
        fn identity(&self) -> Result<ModuleId, Error> {
            let mut exit = 0;
            if unsafe { GetExitCodeProcess(self.process, &mut exit) } == 0 || exit != 259 { return Err(Error::Unavailable("game process exited".into())); }
            Ok(self.id.clone())
        }
        fn read(&self, address: u64, size: usize) -> Result<Vec<u8>, Error> {
            if size == 0 || size > self.len || address < self.base || address.checked_add(size as u64).is_none_or(|end| end > self.base + self.len as u64) { return Err(Error::Unavailable("read outside main image".into())); }
            let mut bytes = vec![0; size];
            let mut read = 0;
            if unsafe { ReadProcessMemory(self.process, address as *const _, bytes.as_mut_ptr() as *mut _, size, &mut read) } == 0 || read != size { return Err(error("read mapped value")); }
            Ok(bytes)
        }
        fn can_write(&self) -> bool { false }
        fn write(&mut self, _: u64, _: &[u8]) -> Result<(), Error> { Err(Error::Unavailable("no qualified MSFS execution point".into())) }
        /// ReadProcessMemory fails on a bad page instead of a fault, so a stale pointer is an
        /// error. Small reads only: a step target is a pointer or a camera matrix.
        fn read_heap(&self, address: u64, size: usize) -> Result<Vec<u8>, Error> {
            if size == 0 || size > 4096 || address < 0x10000 || address.checked_add(size as u64).is_none() { return Err(Error::Unavailable("invalid heap read".into())); }
            let mut bytes = vec![0; size];
            let mut read = 0;
            if unsafe { ReadProcessMemory(self.process, address as *const _, bytes.as_mut_ptr() as *mut _, size, &mut read) } == 0 || read != size { return Err(error("read heap value")); }
            Ok(bytes)
        }
        /// camera-locator-v1: committed private read-write regions (no image, no mapped files,
        /// no guard pages), copied with ReadProcessMemory in 1 MiB chunks into one reused buffer.
        /// A chunk that fails to read (freed meanwhile) is skipped, never faulted on.
        fn scan(&self, visit: &mut dyn FnMut(u64, &[u8]) -> bool) -> Result<(), Error> {
            const CHUNK: usize = 1 << 20;
            let mut buffer = vec![0u8; CHUNK];
            let mut at = 0x10000u64;
            while at < 0x7fff_ffff_0000 {
                let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
                if unsafe { VirtualQueryEx(self.process, at as *const _, &mut info, std::mem::size_of_val(&info)) } == 0 { break; }
                let end = (info.BaseAddress as u64).saturating_add(info.RegionSize as u64);
                if end <= at { break; }
                if info.State == MEM_COMMIT && info.Type == MEM_PRIVATE && info.Protect == PAGE_READWRITE {
                    let mut p = info.BaseAddress as u64;
                    while p < end {
                        let n = ((end - p) as usize).min(CHUNK);
                        let mut read = 0;
                        if unsafe { ReadProcessMemory(self.process, p as *const _, buffer.as_mut_ptr() as *mut _, n, &mut read) } != 0 && read == n && !visit(p, &buffer[..n]) { return Ok(()); }
                        p += n as u64;
                    }
                }
                at = end;
            }
            Ok(())
        }
    }

    thread_local! { static EXECUTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

    /// Mark the current call as the qualified execution point. Only the host executor calls
    /// this, on the thread it proved. The mark ends with `f`, also when `f` panics.
    pub fn execute<R>(f: impl FnOnce() -> R) -> R {
        struct Reset(bool);
        impl Drop for Reset { fn drop(&mut self) { EXECUTING.with(|e| e.set(self.0)); } }
        let _reset = Reset(EXECUTING.with(|e| e.replace(true)));
        f()
    }

    /// Reads like Reader. Writes use WriteProcessMemory on this process, so a bad page fails
    /// with an error instead of a fault. `thread` 0 means no qualified thread: read only.
    pub struct Writer { reader: Reader, thread: u32 }

    impl Writer {
        pub fn open(game: &str, module: &str, thread: u32) -> Result<Self, Error> {
            Ok(Self { reader: Reader::open(unsafe { GetCurrentProcessId() }, game, module)?, thread })
        }
        pub fn snapshot(&self) -> Result<Image, Error> { self.reader.snapshot() }
        fn inside(&self, address: u64, size: u64) -> bool {
            address >= self.reader.base && address.checked_add(size).is_some_and(|end| end <= self.reader.base + self.reader.len as u64)
        }
    }

    impl Memory for Writer {
        fn identity(&self) -> Result<ModuleId, Error> { self.reader.identity() }
        fn read(&self, address: u64, size: usize) -> Result<Vec<u8>, Error> { self.reader.read(address, size) }
        fn read_heap(&self, address: u64, size: usize) -> Result<Vec<u8>, Error> { self.reader.read_heap(address, size) }
        fn can_write(&self) -> bool { self.thread != 0 && EXECUTING.with(|e| e.get()) && unsafe { GetCurrentThreadId() } == self.thread }
        fn write(&mut self, address: u64, bytes: &[u8]) -> Result<(), Error> {
            if !self.can_write() { return Err(Error::Unavailable("not at the qualified execution point".into())); }
            if bytes.is_empty() || !self.inside(address, bytes.len() as u64) { return Err(Error::Unavailable("write outside main image".into())); }
            let mut written = 0;
            if unsafe { WriteProcessMemory(GetCurrentProcess(), address as *const _, bytes.as_ptr() as *const _, bytes.len(), &mut written) } == 0 || written != bytes.len() {
                return Err(error("write mapped value"));
            }
            Ok(())
        }
        /// The handler comes from the vtable slot of a class-checked object, in the executable
        /// code of the main image; fm_memory refuses it when the slot changed since the record.
        /// The exact function is not proven (heat-mirage learning 58). Windows x64 has one C
        /// calling convention, so `fn(object)` matches the game's own call.
        fn call(&mut self, function: u64, object: u64) -> Result<(), Error> {
            if !self.can_write() { return Err(Error::Unavailable("not at the qualified execution point".into())); }
            if !self.inside(function, 1) || !self.inside(object, 8) { return Err(Error::Unavailable("handler or object outside main image".into())); }
            let handler: extern "C" fn(*mut core::ffi::c_void) = unsafe { std::mem::transmute(function as usize) };
            handler(object as *mut _);
            Ok(())
        }
    }
}

/// Width over height of the client area of the largest visible top-level window of `pid`: the
/// view that a camera locator checks a projection against (creator tools run outside the game;
/// the host uses its own game window). None when the process shows no window.
#[cfg(windows)]
pub fn window_aspect(pid: u32) -> Option<f32> {
    use windows_sys::Win32::{Foundation::{HWND, LPARAM, RECT}, UI::WindowsAndMessaging::{EnumWindows, GetClientRect, GetWindowThreadProcessId, IsWindowVisible}};
    unsafe extern "system" fn each(w: HWND, found: LPARAM) -> windows_sys::core::BOOL {
        let (pid, best) = &mut *(found as *mut (u32, (i64, f32)));
        let mut owner = 0;
        GetWindowThreadProcessId(w, &mut owner);
        let mut r: RECT = std::mem::zeroed();
        if owner == *pid && IsWindowVisible(w) != 0 && GetClientRect(w, &mut r) != 0 && r.right > 0 && r.bottom > 0 {
            let area = r.right as i64 * r.bottom as i64;
            if area > best.0 { *best = (area, r.right as f32 / r.bottom as f32); }
        }
        1
    }
    let mut found = (pid, (0i64, 0f32));
    unsafe { EnumWindows(Some(each), &mut found as *mut _ as LPARAM) };
    (found.1 .0 > 0).then_some(found.1 .1)
}

#[cfg(windows)]
pub use win::{execute, Reader, Writer};
