// Microsoft Store installs. The package API in kernel32 (FindPackagesByPackageFamily,
// GetPackagePathByFullName) gives the install folder for the current user with no admin and no
// WinRT. Only main packages count (not language resource packages of the same family). The EXE
// inside WindowsApps has no readable version, so the package version is used.
use crate::games::GameInstall;

#[cfg(windows)]
pub fn installs() -> Vec<GameInstall> {
    use crate::games::{Source, CATALOG};
    let mut out = Vec::new();
    for def in CATALOG {
        let (Some(family), Some(exe_rel)) = (def.store_family, def.exe_rel) else { continue };
        for full_name in win::packages_by_family(family) {
            let Some(dir) = win::package_path(&full_name) else { continue };
            out.push(GameInstall {
                game: def.game,
                source: Source::MicrosoftStore,
                exe: dir.join(exe_rel),
                install_dir: dir,
                version: package_version(&full_name).map(str::to_owned),
                steam_build_id: None,
                ready: true,
                running_pid: None,
            });
        }
    }
    out
}

#[cfg(not(windows))]
pub fn installs() -> Vec<GameInstall> {
    Vec::new()
}

/// Version part of a package full name: "Name_1.9.18.0_x64__publisherid" gives "1.9.18.0".
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn package_version(full_name: &str) -> Option<&str> {
    full_name.split('_').nth(1).filter(|v| !v.is_empty())
}

#[cfg(windows)]
mod win {
    use std::path::PathBuf;
    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS};
    use windows_sys::Win32::Storage::Packaging::Appx::{FindPackagesByPackageFamily, GetPackagePathByFullName, PACKAGE_FILTER_HEAD};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn from_wide(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }

    /// Full names of the main packages of one family installed for this user (usually one).
    /// A package can be added between the size query and the fill call; then the sizes are
    /// asked again (3 tries).
    pub fn packages_by_family(family: &str) -> Vec<String> {
        let fam = wide(family);
        let none = std::ptr::null_mut();
        for _ in 0..3 {
            let (mut count, mut len) = (0u32, 0u32);
            // SAFETY: the size query passes null buffers, which the API allows.
            let r = unsafe { FindPackagesByPackageFamily(fam.as_ptr(), PACKAGE_FILTER_HEAD, &mut count, none, &mut len, none.cast(), none.cast()) };
            if r != ERROR_INSUFFICIENT_BUFFER || count == 0 {
                return Vec::new();
            }
            let mut names = vec![std::ptr::null_mut::<u16>(); count as usize];
            let mut buf = vec![0u16; len as usize];
            // SAFETY: both buffers have the sizes that the size query returned.
            let r = unsafe { FindPackagesByPackageFamily(fam.as_ptr(), PACKAGE_FILTER_HEAD, &mut count, names.as_mut_ptr(), &mut len, buf.as_mut_ptr(), none.cast()) };
            if r == ERROR_INSUFFICIENT_BUFFER {
                continue;
            }
            if r != ERROR_SUCCESS {
                return Vec::new();
            }
            // The name pointers point into buf; read them as offsets so no raw pointer is followed.
            let base = buf.as_ptr() as usize;
            return names
                .iter()
                .take(count as usize)
                .filter_map(|&p| (p as usize).checked_sub(base).map(|off| off / 2))
                .filter(|&off| off < buf.len())
                .map(|off| from_wide(&buf[off..]))
                .collect();
        }
        Vec::new()
    }

    pub fn package_path(full_name: &str) -> Option<PathBuf> {
        let name = wide(full_name);
        let mut len = 0u32;
        // SAFETY: size query with a null buffer, as documented.
        let r = unsafe { GetPackagePathByFullName(name.as_ptr(), &mut len, std::ptr::null_mut()) };
        if r != ERROR_INSUFFICIENT_BUFFER {
            return None;
        }
        let mut buf = vec![0u16; len as usize];
        // SAFETY: buf has the length that the size query returned.
        let r = unsafe { GetPackagePathByFullName(name.as_ptr(), &mut len, buf.as_mut_ptr()) };
        (r == ERROR_SUCCESS).then(|| PathBuf::from(from_wide(&buf)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_from_full_name() {
        assert_eq!(package_version("Microsoft.Limitless_1.9.18.0_x64__8wekyb3d8bbwe"), Some("1.9.18.0"));
        assert_eq!(package_version("broken"), None);
    }

    #[cfg(windows)]
    #[test]
    fn unknown_family_gives_nothing() {
        assert!(win::packages_by_family("Example.DoesNotExist_0000000000000").is_empty());
    }
}
