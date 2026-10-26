// Signed app packages (plan M8). A package is a zip of one version folder. Its signature is the
// Tauri updater format (base64 of a minisign signature whose trusted comment holds
// "version:<v>"), so the app updater checks feed packages with the Tauri code and the setup exe
// checks its own payload here, with the same public key. The setup exe carries the package at
// its end: [exe][zip][signature][trailer JSON][trailer length u64 LE][MAGIC].
use base64::Engine;
use minisign_verify::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

const MAGIC: &[u8; 8] = b"FMSETUP1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trailer {
    pub version: String,
    pub zip_len: u64,
    pub sig_len: u64,
}

fn b64_text(s: &str) -> Result<String, String> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(s.trim()).map_err(|e| format!("bad base64: {e}"))?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

/// The app updater public key (Tauri format): app packages, the setup exe, reviewed maps and
/// session records. This file is its source in the Rust crates, so the SDK export needs no app
/// file; app/src-tauri/tauri.conf.json repeats it for the Tauri updater, and an installer test
/// checks that the two are the same.
pub fn app_key() -> &'static str {
    include_str!("../keys/app-update.pub").trim()
}

/// Checks `data` against a Tauri-format signature and the version it was signed for.
pub fn verify(data: &[u8], signature: &str, pubkey: &str, version: &str) -> Result<(), String> {
    let pk = PublicKey::decode(&b64_text(pubkey)?).map_err(|e| format!("public key: {e}"))?;
    let sig = Signature::decode(&b64_text(signature)?).map_err(|e| format!("signature: {e}"))?;
    pk.verify(data, &sig, true).map_err(|e| format!("the package signature is not valid: {e}"))?;
    let signed = sig.trusted_comment().split('\t').find_map(|kv| kv.strip_prefix("version:"));
    match signed {
        Some(s) if s == version => Ok(()),
        Some(s) => Err(format!("the package was signed for version {s}, not {version}")),
        None => Err("the signature has no version".into()),
    }
}

/// Zips a version folder (files only, relative paths with "/"). Unix modes: 0755 for a program,
/// 0644 for the rest (unpack keeps them).
pub fn pack(dir: &Path, out: &mut (impl Write + Seek)) -> io::Result<()> {
    let mut z = zip::ZipWriter::new(out);
    let mut files = vec![];
    walk(dir, dir, &mut files)?;
    files.sort();
    for rel in files {
        let mode = if is_program(dir, &rel) { 0o755 } else { 0o644 };
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).unix_permissions(mode);
        z.start_file(rel.as_str(), opts).map_err(io::Error::other)?;
        io::copy(&mut std::fs::File::open(dir.join(&rel))?, &mut z)?;
    }
    z.finish().map_err(io::Error::other)?;
    Ok(())
}

/// A program of the version folder: a file with an execute bit on a Unix build PC (cargo output
/// has it), or one of the programs that the app starts, also when packed on Windows: the app
/// exe, the launcher and the hook helper in resources/ (a missing bit there breaks every hook).
fn is_program(dir: &Path, rel: &str) -> bool {
    let named = [crate::app_exe_name(), fm_platform::install::launcher_name().to_string(), format!("resources/{}", fm_platform::install::hook_helper_name())];
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(dir.join(rel)).is_ok_and(|m| m.permissions().mode() & 0o111 != 0) {
            return true;
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
    named.iter().any(|n| n == rel)
}

pub(crate) fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) -> io::Result<()> {
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if p.is_dir() {
            walk(base, &p, out)?;
        } else {
            let rel = p.strip_prefix(base).map_err(io::Error::other)?;
            out.push(rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"));
        }
    }
    Ok(())
}

/// Appends a package to a setup stub (the build step of a setup exe).
pub fn append_payload(stub: &[u8], zip: &[u8], signature: &str, version: &str, out: &mut impl Write) -> io::Result<()> {
    crate::check_version(version)?;
    let t = Trailer { version: version.into(), zip_len: zip.len() as u64, sig_len: signature.len() as u64 };
    let json = serde_json::to_vec(&t).map_err(io::Error::other)?;
    out.write_all(stub)?;
    out.write_all(zip)?;
    out.write_all(signature.as_bytes())?;
    out.write_all(&json)?;
    out.write_all(&(json.len() as u64).to_le_bytes())?;
    out.write_all(MAGIC)
}

/// The package at the end of a setup exe, and the length of the exe without it (the installed
/// launcher is that part). None: no payload (an installed launcher, or a dev build).
pub struct Payload {
    pub trailer: Trailer,
    pub zip: Vec<u8>,
    pub signature: String,
    pub stub_len: u64,
}

pub fn read_payload(file: &mut (impl Read + Seek)) -> io::Result<Option<Payload>> {
    let end = file.seek(SeekFrom::End(0))?;
    if end < 16 {
        return Ok(None);
    }
    let mut tail = [0u8; 16];
    file.seek(SeekFrom::Start(end - 16))?;
    file.read_exact(&mut tail)?;
    if &tail[8..] != MAGIC {
        return Ok(None);
    }
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "damaged setup payload");
    let json_len = u64::from_le_bytes(tail[..8].try_into().unwrap());
    let json_start = (end - 16).checked_sub(json_len).ok_or_else(bad)?;
    let mut json = vec![0u8; json_len as usize];
    file.seek(SeekFrom::Start(json_start))?;
    file.read_exact(&mut json)?;
    let t: Trailer = serde_json::from_slice(&json).map_err(|_| bad())?;
    // Checked: damaged lengths give an error, never a wrap or an allocation larger than the file.
    let stub_len = t.zip_len.checked_add(t.sig_len).and_then(|n| json_start.checked_sub(n)).ok_or_else(bad)?;
    file.seek(SeekFrom::Start(stub_len))?;
    let mut zip = vec![0u8; t.zip_len as usize];
    file.read_exact(&mut zip)?;
    let mut sig = vec![0u8; t.sig_len as usize];
    file.read_exact(&mut sig)?;
    Ok(Some(Payload { signature: String::from_utf8(sig).map_err(|_| bad())?, zip, trailer: t, stub_len }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_round_trip_and_plain_exe() {
        let zip = crate::tests::zip_of(&[("fm_app", b"x")]);
        let mut setup = vec![];
        append_payload(b"STUB", &zip, "sig", "1.2.3", &mut setup).unwrap();
        let p = read_payload(&mut io::Cursor::new(&setup)).unwrap().unwrap();
        assert_eq!((p.stub_len, p.zip, p.signature.as_str(), p.trailer.version.as_str()), (4, zip, "sig", "1.2.3"));
        assert!(read_payload(&mut io::Cursor::new(b"just an exe with no payload".to_vec())).unwrap().is_none());
        // A file cut at the start keeps the trailer, but the lengths no longer fit: an error.
        let mut cut = setup.clone();
        cut.drain(0..10);
        assert!(read_payload(&mut io::Cursor::new(cut)).is_err());
        // Lengths that wrap when added: an error, no panic and no huge allocation.
        let json = serde_json::to_vec(&Trailer { version: "1.0.0".into(), zip_len: u64::MAX, sig_len: 2 }).unwrap();
        let mut forged = b"STUB".to_vec();
        forged.extend(&json);
        forged.extend((json.len() as u64).to_le_bytes());
        forged.extend(MAGIC);
        assert!(read_payload(&mut io::Cursor::new(forged)).is_err());
    }

    #[test]
    fn pack_then_unpack_gives_the_same_files() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join(crate::app_exe_name()), b"app").unwrap();
        std::fs::create_dir(src.path().join("resources")).unwrap();
        std::fs::write(src.path().join("resources").join("p.dll"), b"plugin").unwrap();
        let mut zip = io::Cursor::new(vec![]);
        pack(src.path(), &mut zip).unwrap();
        let root = tempfile::tempdir().unwrap();
        let i = crate::Install::new(root.path());
        i.unpack("1.0.0", io::Cursor::new(zip.into_inner()), false).unwrap();
        assert_eq!(std::fs::read(i.version_dir("1.0.0").join("resources").join("p.dll")).unwrap(), b"plugin");
    }

    /// The programs that the app starts get 0755 in the zip, also when packed on Windows: the
    /// app exe, the launcher and the hook helper in resources/ (else every hook fails on Linux).
    #[test]
    fn programs_get_the_execute_mode() {
        let src = tempfile::tempdir().unwrap();
        let helper = format!("resources/{}", fm_platform::install::hook_helper_name());
        std::fs::create_dir(src.path().join("resources")).unwrap();
        for f in [crate::app_exe_name(), helper.clone(), "resources/p.dll".into()] {
            std::fs::write(src.path().join(&f), b"x").unwrap();
        }
        let mut zip = io::Cursor::new(vec![]);
        pack(src.path(), &mut zip).unwrap();
        let mut a = zip::ZipArchive::new(io::Cursor::new(zip.into_inner())).unwrap();
        let mut mode = |n: &str| a.by_name(n).unwrap().unix_mode().map(|m| m & 0o777);
        assert_eq!(mode(&crate::app_exe_name()), Some(0o755));
        assert_eq!(mode(&helper), Some(0o755));
        assert_eq!(mode("resources/p.dll"), Some(0o644));
    }

    // tests/data: a throwaway key (public part only) and `tauri signer sign --app-version 1.0.0`
    // of pkg.bin, so this checks the real Tauri signature format.
    #[test]
    fn signatures_need_the_key_and_the_signed_version() {
        let data = include_bytes!("../tests/data/pkg.bin");
        let sig = include_str!("../tests/data/pkg.bin.sig");
        let key = include_str!("../tests/data/test-key.pub");
        verify(data, sig, key, "1.0.0").unwrap();
        assert!(verify(data, sig, key, "1.0.1").unwrap_err().contains("signed for version 1.0.0"));
        assert!(verify(b"other bytes", sig, key, "1.0.0").is_err());
        assert!(PublicKey::decode(&b64_text(app_key()).unwrap()).is_ok(), "the app key decodes");
        assert!(verify(data, sig, app_key(), "1.0.0").is_err(), "another key is refused");
        assert!(verify(data, "!!", "!!", "1.0.0").is_err());
    }
}
