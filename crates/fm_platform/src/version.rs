// EXE file version from the Windows version resource (the numeric VS_FIXEDFILEINFO, not the
// FileVersion string: ATS puts a commit hash in the string). Other OSes have no such resource;
// there the version comes from the game itself later (ATS plugin Hello).
use std::path::Path;

#[cfg(windows)]
pub fn file_version(path: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO};
    let wpath: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // SAFETY: wpath is NUL-terminated; the handle argument is unused and may be null.
    let size = unsafe { GetFileVersionInfoSizeW(wpath.as_ptr(), std::ptr::null_mut()) };
    if size == 0 {
        return None;
    }
    let mut data = vec![0u8; size as usize];
    // SAFETY: data has the size that GetFileVersionInfoSizeW returned.
    if unsafe { GetFileVersionInfoW(wpath.as_ptr(), 0, size, data.as_mut_ptr().cast()) } == 0 {
        return None;
    }
    let root: Vec<u16> = "\\\0".encode_utf16().collect();
    let (mut info, mut len) = (std::ptr::null_mut(), 0u32);
    // SAFETY: VerQueryValueW returns a pointer into data, which lives until the end of this function.
    if unsafe { VerQueryValueW(data.as_ptr().cast(), root.as_ptr(), &mut info, &mut len) } == 0
        || (len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>()
    {
        return None;
    }
    // SAFETY: the API checked the block; the size check above covers the struct. Read unaligned.
    let fi: VS_FIXEDFILEINFO = unsafe { std::ptr::read_unaligned(info.cast()) };
    let (ms, ls) = (fi.dwFileVersionMS, fi.dwFileVersionLS);
    Some(format!("{}.{}.{}.{}", ms >> 16, ms & 0xffff, ls >> 16, ls & 0xffff))
}

#[cfg(not(windows))]
pub fn file_version(_path: &Path) -> Option<String> {
    None
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn reads_a_system_dll_version() {
        let dir = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let v = super::file_version(&std::path::Path::new(&dir).join("System32\\kernel32.dll")).expect("version");
        assert_eq!(v.split('.').count(), 4);
        assert!(super::file_version(std::path::Path::new("C:\\does\\not\\exist.exe")).is_none());
    }
}
