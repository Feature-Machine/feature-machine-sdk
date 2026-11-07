// SYSTEM process security can refuse even a query-limited handle. Native read-only queries
// supply the same image and FILETIME token; two snapshots bind the image to one process life.
use ntapi::ntexapi::{SYSTEM_PROCESS_ID_INFORMATION, SYSTEM_PROCESS_INFORMATION};
use std::{ffi::c_void, io, mem::{offset_of, size_of}, path::PathBuf, sync::OnceLock};
use windows_sys::Win32::{Storage::FileSystem::QueryDosDeviceW, System::LibraryLoader::{GetModuleHandleW, GetProcAddress}};

type Query = unsafe extern "system" fn(u32, *mut c_void, u32, *mut u32) -> i32;
const PROCESS_TABLE: u32 = 5;
const PROCESS_IMAGE: u32 = 88;
const LENGTH_MISMATCH: i32 = 0xc0000004u32 as i32;
const MAX_TABLE: usize = 16 * 1024 * 1024;

fn query() -> io::Result<Query> {
    static QUERY: OnceLock<Option<Query>> = OnceLock::new();
    QUERY.get_or_init(|| {
        let module: Vec<_> = "ntdll.dll".encode_utf16().chain(Some(0)).collect();
        // SAFETY: ntdll stays loaded for the process life. The documented function ABI and
        // information-class layouts come from the pinned Windows native API definitions.
        unsafe {
            let handle = GetModuleHandleW(module.as_ptr());
            GetProcAddress(handle, c"NtQuerySystemInformation".as_ptr().cast()).map(|f| std::mem::transmute::<unsafe extern "system" fn() -> isize, Query>(f))
        }
    }).ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "native process observation is unavailable"))
}

fn status_error(status: i32) -> io::Error { io::Error::other(format!("native process observation failed ({status:#x})")) }

fn table(query: Query) -> io::Result<Vec<u8>> {
    let mut wanted: usize = 256 * 1024;
    for _ in 0..4 {
        if wanted > MAX_TABLE { return Err(io::Error::other("process observation exceeds its limit")); }
        let mut words = vec![0u64; wanted.div_ceil(8)];
        let mut needed = 0;
        // SAFETY: the aligned buffer is bounded and writable for the supplied byte count.
        let status = unsafe { query(PROCESS_TABLE, words.as_mut_ptr().cast(), (words.len() * 8) as u32, &mut needed) };
        if status == LENGTH_MISMATCH { wanted = (needed as usize).max(wanted * 2); continue; }
        if status < 0 { return Err(status_error(status)); }
        if needed as usize > words.len() * 8 { return Err(io::Error::other("invalid process observation size")); }
        // SAFETY: the OS initialized these bytes; parsing below copies scalar fields only.
        return Ok(unsafe { std::slice::from_raw_parts(words.as_ptr().cast(), needed as usize) }.to_vec());
    }
    Err(io::Error::other("process table changed during observation"))
}

// Only scalar fields are read. No thread array or string pointer from the system table is
// dereferenced, and every next-record offset must advance within the returned byte range.
fn birth(bytes: &[u8], pid: u32) -> io::Result<u64> {
    let mut at = 0usize;
    const HEADER: usize = offset_of!(SYSTEM_PROCESS_INFORMATION, Threads);
    loop {
        let header = bytes.get(at..).and_then(|b| b.get(..HEADER)).ok_or_else(|| io::Error::other("truncated process observation"))?;
        let next = u32::from_ne_bytes(header[..4].try_into().unwrap()) as usize;
        let id_at = offset_of!(SYSTEM_PROCESS_INFORMATION, UniqueProcessId);
        let id = usize::from_ne_bytes(header[id_at..id_at + size_of::<usize>()].try_into().unwrap());
        if id == pid as usize {
            let start_at = offset_of!(SYSTEM_PROCESS_INFORMATION, CreateTime);
            let start = i64::from_ne_bytes(header[start_at..start_at + 8].try_into().unwrap());
            return u64::try_from(start).ok().filter(|s| *s > 0).ok_or_else(|| io::Error::other("invalid process birth token"));
        }
        if next == 0 { return Err(io::Error::new(io::ErrorKind::NotFound, "process ended during observation")); }
        if next < HEADER || next > bytes.len() - at { return Err(io::Error::other("invalid process observation offset")); }
        at += next;
    }
}

fn image(query: Query, pid: u32) -> io::Result<PathBuf> {
    let mut text = vec![0u16; 32767];
    // SAFETY: the native structure holds only the PID and our own live output buffer.
    let mut info: SYSTEM_PROCESS_ID_INFORMATION = unsafe { std::mem::zeroed() };
    info.ProcessId = pid as usize as _;
    info.ImageName.Buffer = text.as_mut_ptr();
    info.ImageName.MaximumLength = (text.len() * 2) as u16;
    let pointer = info.ImageName.Buffer;
    // SAFETY: both structure and string buffer remain live for this synchronous query.
    let status = unsafe { query(PROCESS_IMAGE, (&raw mut info).cast(), size_of::<SYSTEM_PROCESS_ID_INFORMATION>() as u32, std::ptr::null_mut()) };
    if status < 0 { return Err(status_error(status)); }
    let len = info.ImageName.Length as usize;
    if info.ImageName.Buffer != pointer || len == 0 || !len.is_multiple_of(2) || len > text.len() * 2 { return Err(io::Error::other("invalid process image observation")); }
    let native = String::from_utf16(&text[..len / 2]).map_err(io::Error::other)?;
    for drive in b'A'..=b'Z' {
        let name = [drive as u16, b':' as u16, 0];
        let mut target = vec![0u16; 32768];
        // SAFETY: the drive name is terminated; the output buffer is writable and bounded.
        let n = unsafe { QueryDosDeviceW(name.as_ptr(), target.as_mut_ptr(), target.len() as u32) };
        if n == 0 { continue; }
        let end = target[..n as usize].iter().position(|c| *c == 0).unwrap_or(n as usize);
        let prefix = String::from_utf16(&target[..end]).map_err(io::Error::other)?;
        if native.get(..prefix.len()).is_some_and(|s| s.eq_ignore_ascii_case(&prefix)) && native.as_bytes().get(prefix.len()) == Some(&b'\\') {
            return Ok(PathBuf::from(format!("{}:{}", drive as char, &native[prefix.len()..])));
        }
    }
    Err(io::Error::other("process image has no verified local drive mapping"))
}

pub(super) fn identity(pid: u32) -> io::Result<(PathBuf, u64)> {
    let query = query()?;
    let before = birth(&table(query)?, pid)?;
    let path = image(query, pid)?;
    let after = birth(&table(query)?, pid)?;
    if before != after { return Err(io::Error::other("process changed during identity observation")); }
    Ok((path, before))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_identity_matches_the_handle_image_and_exact_birth_token() {
        let native = identity(std::process::id()).unwrap();
        let handle = super::super::identity_handle(std::process::id()).unwrap();
        assert!(crate::applications::same_path(&native.0, &handle.0));
        assert_eq!(native.1, handle.1);
        assert!(identity(u32::MAX).is_err(), "a cached image cannot prove a nonexistent process");
    }

    #[test]
    fn process_table_rejects_truncation_invalid_offsets_and_missing_processes() {
        let header = offset_of!(SYSTEM_PROCESS_INFORMATION, Threads);
        assert!(birth(&[], 1).is_err());
        let mut bytes = vec![0u8; header];
        assert_eq!(birth(&bytes, 1).unwrap_err().kind(), io::ErrorKind::NotFound);
        bytes[..4].copy_from_slice(&1u32.to_ne_bytes());
        assert!(birth(&bytes, 1).is_err());
        bytes[..4].copy_from_slice(&u32::MAX.to_ne_bytes());
        assert!(birth(&bytes, 1).is_err());
    }
}
