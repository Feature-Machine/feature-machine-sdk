// Native read-only Windows evidence binds a service and its listener to one signed image.
// No command shell, service mutation or network access is part of these observations.
use std::{io, mem::size_of, path::{Path, PathBuf}};
use std::os::windows::{ffi::OsStrExt, io::AsRawHandle};
use windows_sys::Win32::{Foundation::*, Security::{Cryptography::*, WinTrust::*}, System::{Services::*}};

fn wide(s: &std::ffi::OsStr) -> Vec<u16> { s.encode_wide().chain(Some(0)).collect() }

/// One version of one file: volume, file id, last write time and size, read through the held
/// handle that denies writers. A replaced or rewritten image gets a new key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct FileKey { volume: u32, index: u64, written: u64, size: u64 }

fn file_key(file: &std::fs::File) -> io::Result<FileKey> {
    use windows_sys::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is open for this call and the structure is ours.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 { return Err(io::Error::last_os_error()); }
    let pair = |high: u32, low: u32| (u64::from(high) << 32) | u64::from(low);
    Ok(FileKey { volume: info.dwVolumeSerialNumber, index: pair(info.nFileIndexHigh, info.nFileIndexLow),
        written: pair(info.ftLastWriteTime.dwHighDateTime, info.ftLastWriteTime.dwLowDateTime), size: pair(info.nFileSizeHigh, info.nFileSizeLow) })
}

/// A verified Authenticode leaf name. WinTrust checks the signature chain without web access;
/// the publisher allowlist is applied by the reviewed application resolver. A good result is
/// kept by file version (review B3): the helper checks two images each second, and a full
/// Authenticode hash of each one each second was the main cost. Failures are never kept.
pub fn signer(path: &Path) -> Result<String, String> {
    use std::{collections::HashMap, os::windows::fs::OpenOptionsExt, sync::{Mutex, OnceLock}};
    static VERIFIED: OnceLock<Mutex<HashMap<FileKey, String>>> = OnceLock::new();
    let file = std::fs::OpenOptions::new().read(true).share_mode(1).open(path).map_err(|e| e.to_string())?;
    let key = file_key(&file).map_err(|e| e.to_string())?;
    let verified = VERIFIED.get_or_init(Default::default);
    if let Some(name) = verified.lock().unwrap_or_else(|e| e.into_inner()).get(&key) { return Ok(name.clone()); }
    let name = verify(&file, path)?;
    let mut cache = verified.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() >= 64 { cache.clear(); }
    cache.insert(key, name.clone());
    Ok(name)
}

fn verify(file: &std::fs::File, path: &Path) -> Result<String, String> {
    let path = wide(path.as_os_str());
    let mut info = WINTRUST_FILE_INFO { cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: path.as_ptr(), hFile: file.as_raw_handle(), ..Default::default() };
    let mut data = WINTRUST_DATA { cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE, fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE, Anonymous: WINTRUST_DATA_0 { pFile: &mut info },
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL | WTD_REVOCATION_CHECK_NONE,
        ..Default::default() };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: all inputs live through verification and state close. Provider pointers remain
    // owned by WinTrust until CLOSE, including on a failed verification.
    let result = unsafe {
        let status = WinVerifyTrust(std::ptr::null_mut(), &mut action, (&raw mut data).cast());
        let answer = if status != 0 { Err(format!("signature verification failed ({status:#x})")) } else {
            let provider = WTHelperProvDataFromStateData(data.hWVTStateData);
            let signer = if provider.is_null() { std::ptr::null_mut() } else { WTHelperGetProvSignerFromChain(provider, 0, 0, 0) };
            let cert = if signer.is_null() { std::ptr::null_mut() } else { WTHelperGetProvCertFromChain(signer, 0) };
            if cert.is_null() || (*cert).pCert.is_null() { Err("no signing certificate".into()) } else {
                let n = CertGetNameStringW((*cert).pCert, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, std::ptr::null(), std::ptr::null_mut(), 0);
                if !(2..=4096).contains(&n) { Err("invalid signing certificate name".into()) } else {
                    let mut name = vec![0u16; n as usize];
                    let got = CertGetNameStringW((*cert).pCert, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, std::ptr::null(), name.as_mut_ptr(), n);
                    if got != n { Err("signing certificate name changed".into()) }
                    else { Ok(String::from_utf16_lossy(&name[..n as usize - 1])) }
                }
            }
        };
        data.dwStateAction = WTD_STATEACTION_CLOSE;
        WinVerifyTrust(std::ptr::null_mut(), &mut action, (&raw mut data).cast());
        answer
    };
    result
}

pub(super) use crate::process::identity as process;

#[derive(Clone, Debug)]
pub struct Service { pub exe: PathBuf, pub pid: Option<u32> }

/// Read service configuration and status through SCM query rights only. The aligned buffer
/// belongs to this call; its string pointer is copied before the buffer is released.
pub fn service(name: &str) -> Result<Service, String> {
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: handles use query-only rights and are closed on every result path.
    unsafe {
        let manager = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if manager.is_null() { return Err(io::Error::last_os_error().to_string()); }
        let svc = OpenServiceW(manager, name.as_ptr(), SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS);
        let error = io::Error::last_os_error();
        CloseServiceHandle(manager);
        if svc.is_null() { return Err(error.to_string()); }
        let result = (|| {
            let mut needed = 0;
            QueryServiceConfigW(svc, std::ptr::null_mut(), 0, &mut needed);
            if needed == 0 || needed > 65536 { return Err("invalid service configuration size".into()); }
            let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
            if QueryServiceConfigW(svc, buf.as_mut_ptr().cast(), needed, &mut needed) == 0 { return Err(io::Error::last_os_error().to_string()); }
            let config = &*buf.as_ptr().cast::<QUERY_SERVICE_CONFIGW>();
            let p = config.lpBinaryPathName;
            let start = buf.as_ptr() as usize;
            let end = start + buf.len() * 8;
            if p.is_null() || (p as usize) < start || (p as usize) >= end { return Err("invalid service image path".into()); }
            let text = std::slice::from_raw_parts(p, (end - p as usize) / 2);
            let n = text.iter().position(|c| *c == 0).ok_or("service image path has no terminator")?;
            let exe = super::service_executable(&String::from_utf16_lossy(&text[..n]))?;
            let mut status = SERVICE_STATUS_PROCESS::default();
            if QueryServiceStatusEx(svc, SC_STATUS_PROCESS_INFO, (&raw mut status).cast(), size_of::<SERVICE_STATUS_PROCESS>() as u32, &mut needed) == 0 { return Err(io::Error::last_os_error().to_string()); }
            Ok(Service { exe, pid: (status.dwCurrentState == SERVICE_RUNNING && status.dwProcessId != 0).then_some(status.dwProcessId) })
        })();
        CloseServiceHandle(svc);
        result
    }
}

/// Only an IPv4 loopback listener counts. The connector uses that exact address, so an IPv6
/// row or a wildcard socket cannot establish the identity of its destination.
/// The listener-only table establishes socket state; its dwState field is not needed.
pub fn listener(port: u16, pid: u32) -> Result<(), String> {
    use windows_sys::Win32::NetworkManagement::IpHelper::*;
    use windows_sys::Win32::Networking::WinSock::AF_INET;
    // SAFETY: the table is in our aligned, bounded buffer. Rows are checked against its size.
    unsafe {
        let mut size = 0;
        GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, AF_INET as u32, TCP_TABLE_OWNER_PID_LISTENER, 0);
        for _ in 0..3 {
            if !(4..=16 * 1024 * 1024).contains(&size) { return Err("invalid TCP table size".into()); }
            let mut buf = vec![0u64; (size as usize).div_ceil(8)];
            match GetExtendedTcpTable(buf.as_mut_ptr().cast(), &mut size, 0, AF_INET as u32, TCP_TABLE_OWNER_PID_LISTENER, 0) {
                ERROR_INSUFFICIENT_BUFFER => continue,
                ERROR_SUCCESS => {
                    let count = *buf.as_ptr().cast::<u32>() as usize;
                    let offset = std::mem::offset_of!(MIB_TCPTABLE_OWNER_PID, table);
                    if count > (buf.len() * 8 - offset) / size_of::<MIB_TCPROW_OWNER_PID>() { return Err("invalid TCP table length".into()); }
                    let rows = std::slice::from_raw_parts(buf.as_ptr().cast::<u8>().add(offset).cast::<MIB_TCPROW_OWNER_PID>(), count);
                    let ours: Vec<_> = rows.iter().filter(|r| u16::from_be(r.dwLocalPort as u16) == port && r.dwLocalAddr == u32::from_ne_bytes([127, 0, 0, 1])).collect();
                    return if !ours.is_empty() && ours.iter().all(|r| r.dwOwningPid == pid) { Ok(()) } else { Err("loopback listener does not belong to the verified service".into()) };
                }
                code => return Err(format!("TCP observation failed ({code})")),
            }
        }
        Err("TCP table changed during observation".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_identity_uses_the_actual_image_and_start_token() {
        let (path, start) = process(std::process::id()).unwrap();
        assert!(crate::applications::same_path(&path, &std::env::current_exe().unwrap()));
        assert_ne!(start, 0);
        assert_eq!(process(std::process::id()).unwrap().1, start);
    }

    #[test]
    fn unsigned_images_cannot_supply_a_publisher() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"not a signed executable").unwrap();
        assert!(signer(file.path()).is_err());
    }

    // Review B3: a kept signature result belongs to one version of one file.
    #[test]
    fn a_rewritten_file_gets_a_new_signature_key() {
        let temp = tempfile::tempdir().unwrap();
        let (a, b) = (temp.path().join("a.exe"), temp.path().join("b.exe"));
        std::fs::write(&a, b"first").unwrap();
        std::fs::write(&b, b"first").unwrap();
        let key = |p: &Path| file_key(&std::fs::File::open(p).unwrap()).unwrap();
        let first = key(&a);
        assert_eq!(first, key(&a));
        assert_ne!(first, key(&b), "another file");
        std::fs::write(&a, b"second version").unwrap();
        assert_ne!(first, key(&a), "a rewrite");
        assert!(signer(&a).is_err() && signer(&a).is_err(), "a failure is never kept");
    }

    #[test]
    fn loopback_listener_must_belong_to_the_expected_process() {
        let socket = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = socket.local_addr().unwrap().port();
        listener(port, std::process::id()).unwrap();
        assert!(listener(port, std::process::id() + 1).is_err());
    }
}
