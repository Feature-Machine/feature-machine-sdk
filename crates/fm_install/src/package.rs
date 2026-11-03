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

/// The end of the trailer and its last 16 bytes. Plain setup: the end of the file. Authenticode
/// signed setup (plan M8 risk): signtool pads the file with up to 7 zero bytes to 8-byte
/// alignment and adds the certificate table, so the trailer ends 0 to 7 bytes before the table.
/// Authenticode hashes the payload (all bytes before the table except the checksum and the
/// security entry); the package keeps its own minisign check.
fn trailer_end(file: &mut (impl Read + Seek), file_end: u64) -> io::Result<Option<(u64, [u8; 16])>> {
    let table = pe_certificate_table(file, file_end)?;
    let ends: Vec<u64> = match table {
        Some(start) => (0..8).filter_map(|pad| start.checked_sub(pad)).collect(),
        None => vec![file_end],
    };
    for end in ends {
        if end < 16 {
            continue;
        }
        let mut tail = [0u8; 16];
        file.seek(SeekFrom::Start(end - 16))?;
        file.read_exact(&mut tail)?;
        let mut padding = vec![0u8; (table.unwrap_or(end) - end) as usize];
        file.read_exact(&mut padding)?;
        if &tail[8..] == MAGIC && padding.iter().all(|b| *b == 0) {
            return Ok(Some((end, tail)));
        }
    }
    Ok(None)
}

/// The file offset of the Authenticode certificate table when it is the last part of a PE file
/// (IMAGE_DIRECTORY_ENTRY_SECURITY; its address is a file offset, not an RVA). None for a file
/// that is not PE (ELF, Mach-O, tests), has no table, or has bytes after the table. An error
/// for a table at the end whose records do not parse (check_certificate_records).
fn pe_certificate_table(file: &mut (impl Read + Seek), file_end: u64) -> io::Result<Option<u64>> {
    let mut read_at = |at: u64, buf: &mut [u8]| -> io::Result<bool> {
        if at.checked_add(buf.len() as u64).is_none_or(|e| e > file_end) {
            return Ok(false);
        }
        file.seek(SeekFrom::Start(at))?;
        file.read_exact(buf)?;
        Ok(true)
    };
    let mut dos = [0u8; 64];
    if !read_at(0, &mut dos)? || &dos[..2] != b"MZ" {
        return Ok(None);
    }
    let pe = u32::from_le_bytes(dos[60..64].try_into().unwrap()) as u64;
    // Signature (4), COFF header (20), then the optional header up to the security entry.
    let mut head = [0u8; 4 + 20 + 112 + 5 * 8];
    if !read_at(pe, &mut head)? || &head[..4] != b"PE\0\0" {
        return Ok(None);
    }
    let opt = &head[24..];
    let (count_at, dirs_at) = match u16::from_le_bytes([opt[0], opt[1]]) {
        0x10b => (92, 96),
        0x20b => (108, 112),
        _ => return Ok(None),
    };
    // The entry must be inside the optional header that the COFF header declares (review r1).
    let opt_size = u16::from_le_bytes([head[20], head[21]]) as usize;
    if opt_size < dirs_at + 5 * 8 || u32::from_le_bytes(opt[count_at..count_at + 4].try_into().unwrap()) < 5 {
        return Ok(None);
    }
    let entry = &opt[dirs_at + 4 * 8..dirs_at + 5 * 8];
    let (start, size) = (u32::from_le_bytes(entry[..4].try_into().unwrap()) as u64, u32::from_le_bytes(entry[4..].try_into().unwrap()) as u64);
    if size == 0 || start + size != file_end {
        return Ok(None);
    }
    check_certificate_records(file, start, size)?;
    Ok(Some(start))
}

/// Walks the WIN_CERTIFICATE records (a nested signtool /as signature stays in one record). Each
/// has dwLength (8-byte header included); the next starts at the next 8-byte offset. As the PE
/// format requires, the rounded lengths add up to the directory size exactly: 28103 real signed
/// files had no byte after the last record (review r1 scan). Any other layout: InvalidData, as a
/// table that ends the file but does not parse means a damaged file, not "no payload".
fn check_certificate_records(file: &mut (impl Read + Seek), start: u64, size: u64) -> io::Result<()> {
    let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidData, format!("damaged certificate table: {why}"));
    let mut at = 0u64;
    while at < size {
        if size - at < 8 {
            return Err(bad("cut record header"));
        }
        let mut len = [0u8; 4];
        file.seek(SeekFrom::Start(start + at))?;
        file.read_exact(&mut len)?;
        let len = u32::from_le_bytes(len) as u64;
        if len == 0 {
            return Err(bad("zero record length"));
        }
        if len < 8 {
            return Err(bad("record shorter than its header"));
        }
        if len > size - at {
            return Err(bad("record runs past the table"));
        }
        at = (at + len).next_multiple_of(8);
        // The alignment bytes of the record are part of the table: a size that stops inside
        // them does not match the records (review r1: dwLength 13 in a table of 13 bytes).
        if at > size {
            return Err(bad("table ends before the alignment bytes of a record"));
        }
    }
    Ok(())
}

pub fn read_payload(file: &mut (impl Read + Seek)) -> io::Result<Option<Payload>> {
    let file_end = file.seek(SeekFrom::End(0))?;
    let Some((end, tail)) = trailer_end(file, file_end)? else { return Ok(None) };
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

    // A PE stub: DOS header, then the PE signature, COFF header and optional header with 16
    // data directories at 0x80. Only the fields that pe_certificate_table reads are set.
    // PE32 (0x10b) and PE32+ (0x20b) keep the directories at different offsets.
    fn pe_stub(magic: u16) -> Vec<u8> {
        let (count_at, dirs_at) = if magic == 0x10b { (92, 96) } else { (108, 112) };
        let mut s = vec![0u8; 0x200];
        s[..2].copy_from_slice(b"MZ");
        s[60..64].copy_from_slice(&0x80u32.to_le_bytes());
        s[0x80..0x84].copy_from_slice(b"PE\0\0");
        s[0x80 + 20..0x80 + 22].copy_from_slice(&((dirs_at + 16 * 8) as u16).to_le_bytes());
        let opt = 0x80 + 24;
        s[opt..opt + 2].copy_from_slice(&magic.to_le_bytes());
        s[opt + count_at..opt + count_at + 4].copy_from_slice(&16u32.to_le_bytes());
        s
    }

    // A WIN_CERTIFICATE list as the PE format gives it: per record dwLength (the 8-byte header
    // included), revision 2.0, type PKCS signed data and filler bytes, then zero padding to 8
    // bytes. `tail` zero bytes after the last record make a table larger than its records (an
    // error case). Tests have no signing path, so they build tables of many records here.
    fn cert_table(lens: &[u32], tail: usize) -> Vec<u8> {
        let mut t = vec![];
        for (i, len) in lens.iter().enumerate() {
            t.extend(len.to_le_bytes());
            t.extend(0x0200u16.to_le_bytes());
            t.extend(0x0002u16.to_le_bytes());
            t.extend(std::iter::repeat_n(0xA0 + i as u8, *len as usize - 8));
            while !t.len().is_multiple_of(8) {
                t.push(0);
            }
        }
        t.extend(std::iter::repeat_n(0, tail));
        t
    }

    // What signtool does to the file: zero padding to 8-byte alignment, the certificate table
    // at the end, and the security directory entry that points to it.
    fn sign(file: &mut Vec<u8>, table: &[u8]) {
        while !file.len().is_multiple_of(8) {
            file.push(0);
        }
        let start = file.len() as u32;
        file.extend(table);
        let dirs_at = if file[0x80 + 24] == 0x0b && file[0x80 + 25] == 0x01 { 96 } else { 112 };
        let entry = 0x80 + 24 + dirs_at + 4 * 8;
        file[entry..entry + 4].copy_from_slice(&start.to_le_bytes());
        file[entry + 4..entry + 8].copy_from_slice(&(table.len() as u32).to_le_bytes());
    }

    // A setup with the package and then `table` as its certificate table (signature length n).
    fn signed_setup(magic: u16, zip: &[u8], n: usize, table: &[u8]) -> Vec<u8> {
        let mut setup = vec![];
        append_payload(&pe_stub(magic), zip, &"s".repeat(n), "1.2.3", &mut setup).unwrap();
        sign(&mut setup, table);
        setup
    }

    /// Multiple certificate records: 1, 2 and 3 records of odd lengths (padded to 8 bytes). A
    /// nested signature (signtool /as) is one record and is the 1-record case. Each layout gives
    /// the exact package and stub length, for every padding before the table (signature lengths
    /// 1 to 8) and for PE32 and PE32+.
    #[test]
    fn a_table_with_more_than_one_record_keeps_the_payload() {
        let zip = crate::tests::zip_of(&[("fm_app", b"x")]);
        let layouts: [&[u32]; 6] = [&[13], &[40], &[13, 1029], &[16, 27], &[9, 22, 1501], &[1203, 8, 45]];
        for magic in [0x10b, 0x20b] {
            for lens in layouts {
                let table = cert_table(lens, 0);
                for n in 1..=8 {
                    let setup = signed_setup(magic, &zip, n, &table);
                    let p = read_payload(&mut io::Cursor::new(&setup)).unwrap().unwrap_or_else(|| panic!("{magic:#x} {lens:?} n {n}"));
                    let want = Trailer { version: "1.2.3".into(), zip_len: zip.len() as u64, sig_len: n as u64 };
                    assert_eq!((p.stub_len, &p.zip, p.signature.as_str(), &p.trailer), (0x200, &zip, "s".repeat(n).as_str(), &want), "{magic:#x} {lens:?} n {n}");
                }
            }
        }
    }

    /// A damaged table that ends the file: a cut record header, a record length past the table,
    /// a zero record length (also first), a size that stops inside the alignment bytes of a
    /// record (review r1), or zero bytes after the last record. Each one is an InvalidData
    /// error, not a panic, not a payload read from a wrong offset.
    #[test]
    fn a_damaged_certificate_table_is_an_error() {
        let zip = crate::tests::zip_of(&[("fm_app", b"x")]);
        let two = cert_table(&[13, 21], 0);
        let mut cut = cert_table(&[13], 0);
        cut.extend([0x30, 0x00, 0x00, 0x00]);
        let mut past = two.clone();
        past[16..20].copy_from_slice(&(21u32 + 8).to_le_bytes());
        let mut past_first = two.clone();
        past_first[..4].copy_from_slice(&(two.len() as u32 + 1).to_le_bytes());
        let mut zero = two.clone();
        zero[16..20].copy_from_slice(&0u32.to_le_bytes());
        let mut zero_first = two.clone();
        zero_first[..4].copy_from_slice(&0u32.to_le_bytes());
        let mut short = two.clone();
        short[16..20].copy_from_slice(&7u32.to_le_bytes());
        // dwLength 13 in a table of 13 bytes, and 13 + 21 in a table of 16 + 21 bytes.
        let unaligned_one = cert_table(&[13], 0)[..13].to_vec();
        let unaligned_last = two[..16 + 21].to_vec();
        let cases = [
            ("cut header", cut),
            ("header only", vec![8, 0, 0]),
            ("past", past),
            ("past first", past_first),
            ("zero", zero),
            ("zero first", zero_first),
            ("short", short),
            ("all zero", vec![0; 16]),
            ("unaligned one", unaligned_one),
            ("unaligned last", unaligned_last),
            ("zero tail 8", cert_table(&[16], 8)),
            ("zero tail 24", cert_table(&[9, 22, 1501], 24)),
        ];
        for magic in [0x10b, 0x20b] {
            for (name, table) in &cases {
                for n in 1..=8 {
                    let setup = signed_setup(magic, &zip, n, table);
                    let Err(err) = read_payload(&mut io::Cursor::new(&setup)) else { panic!("{magic:#x} {name} n {n}: no error") };
                    assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{magic:#x} {name} n {n}");
                }
            }
        }
    }

    #[test]
    fn a_signed_setup_keeps_its_payload() {
        let zip = crate::tests::zip_of(&[("fm_app", b"x")]);
        for magic in [0x10b, 0x20b] {
            // Signature lengths 1 to 16 give every padding from 0 to 7 before the table.
            let mut paddings = std::collections::BTreeSet::new();
            for n in 1..=16 {
                let sig = "s".repeat(n);
                let mut setup = vec![];
                append_payload(&pe_stub(magic), &zip, &sig, "1.2.3", &mut setup).unwrap();
                let unsigned = setup.len();
                sign(&mut setup, &cert_table(&[40], 0));
                paddings.insert(setup.len() - 40 - unsigned);
                let p = read_payload(&mut io::Cursor::new(&setup)).unwrap().unwrap();
                assert_eq!((p.stub_len, &p.zip, p.signature.as_str()), (0x200, &zip, sig.as_str()), "{magic:#x} n {n}");
            }
            assert_eq!(paddings, (0..8).collect(), "{magic:#x}");
            // An unsigned PE setup and a signed stub with a package after its table: end of file.
            let mut signed_stub = pe_stub(magic);
            sign(&mut signed_stub, &cert_table(&[16], 0));
            for stub in [pe_stub(magic), signed_stub] {
                let mut setup = vec![];
                append_payload(&stub, &zip, "sig", "1.2.3", &mut setup).unwrap();
                assert_eq!(read_payload(&mut io::Cursor::new(&setup)).unwrap().unwrap().stub_len, stub.len() as u64);
            }
        }
    }

    #[test]
    fn a_signed_file_without_a_clean_trailer_has_no_payload() {
        let zip = crate::tests::zip_of(&[("fm_app", b"x")]);
        // The first signature length whose setup needs padding.
        let (setup, unsigned) = (1..=8)
            .map(|n| {
                let mut s = vec![];
                append_payload(&pe_stub(0x20b), &zip, &"s".repeat(n), "1.2.3", &mut s).unwrap();
                let unsigned = s.len();
                sign(&mut s, &cert_table(&[40], 0));
                (s, unsigned)
            })
            .find(|(s, unsigned)| s.len() - 40 > *unsigned)
            .unwrap();
        assert!(read_payload(&mut io::Cursor::new(&setup)).unwrap().is_some());
        // A padding byte that is not zero: not our layout.
        let mut dirty = setup.clone();
        dirty[unsigned] = 1;
        assert!(read_payload(&mut io::Cursor::new(&dirty)).unwrap().is_none());
        // Fewer than 5 directories, or an optional header too short for the security entry:
        // the entry is not read, so the table is not skipped.
        let mut few = setup.clone();
        few[0x80 + 24 + 108..0x80 + 24 + 112].copy_from_slice(&4u32.to_le_bytes());
        let mut short = setup.clone();
        short[0x80 + 20..0x80 + 22].copy_from_slice(&(112u16 + 4 * 8).to_le_bytes());
        for bad in [few, short] {
            assert!(read_payload(&mut io::Cursor::new(&bad)).unwrap().is_none());
        }
        // A signed launcher with no package, a table entry that points past the file, and a PE
        // header offset past the end of a short file: no payload and no error.
        let mut launcher = pe_stub(0x20b);
        sign(&mut launcher, &cert_table(&[24], 0));
        assert!(read_payload(&mut io::Cursor::new(&launcher)).unwrap().is_none());
        let mut past = launcher.clone();
        past.truncate(past.len() - 8);
        assert!(read_payload(&mut io::Cursor::new(&past)).unwrap().is_none());
        assert!(read_payload(&mut io::Cursor::new(launcher[..0x90].to_vec())).unwrap().is_none());
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
