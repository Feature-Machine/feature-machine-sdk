// fm_pack repack <package.zip> <signed feature.dll> <out.zip> (publish job plan, slice 2): puts
// the Authenticode-signed library in a built feature package. The signed file must have the same
// image as the built library (the signature is the only addition), so the signing machine cannot
// change code. Other entries are copied raw and stay byte-equal. Prints both package digests, so
// the review brief (built SHA-256) and the feed (signed SHA-256) stay linked.
use std::io::{Cursor, Read, Write};

const LIBRARY: &str = "bin/windows/feature.dll";

pub fn command(zip: &str, lib: &str, out: &str) -> Result<(), String> {
    let read = |p: &str| std::fs::read(p).map_err(|e| format!("{p}: {e}"));
    let (built, signed) = (read(zip)?, read(lib)?);
    let bytes = repack(&built, &signed)?;
    let (m, _) = fm_install::feature::inspect(Cursor::new(&bytes)).map_err(|e| e.to_string())?;
    std::fs::write(out, &bytes).map_err(|e| format!("{out}: {e}"))?;
    println!("{}", serde_json::json!({
        "id": m.id, "version": m.version, "sha256": crate::sha256_hex(&bytes), "size": bytes.len(),
        "built_sha256": crate::sha256_hex(&built), "library_sha256": crate::sha256_hex(&signed),
    }));
    Ok(())
}

/// The package `built` with `signed` in place of its Windows library, after the image check.
fn repack(built: &[u8], signed: &[u8]) -> Result<Vec<u8>, String> {
    let ze = |e: zip::result::ZipError| e.to_string();
    let (m, _) = fm_install::feature::inspect(Cursor::new(built)).map_err(|e| e.to_string())?;
    signable(&m)?;
    let mut src = zip::ZipArchive::new(Cursor::new(built)).map_err(ze)?;
    let mut old = vec![];
    src.by_name(LIBRARY).map_err(|_| format!("the package has no {LIBRARY}"))?.read_to_end(&mut old).map_err(|e| e.to_string())?;
    only_signature_added(&old, signed)?;
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..src.len() {
        let entry = src.by_index_raw(i).map_err(ze)?;
        if entry.name() != LIBRARY {
            w.raw_copy_file(entry).map_err(ze)?;
            continue;
        }
        // The options of fm_install::feature::pack_shaders, with the time of the built entry.
        let mut opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).unix_permissions(0o644);
        if let Some(t) = entry.last_modified() { opts = opts.last_modified_time(t); }
        drop(entry);
        w.start_file(LIBRARY, opts).map_err(ze)?;
        w.write_all(signed).map_err(|e| e.to_string())?;
    }
    Ok(w.finish().map_err(ze)?.into_inner())
}

/// A signed memory session record holds the SHA-256 of the whole library, so a library
/// signature breaks it: refused here, before a signing machine does any work. Such a feature
/// takes its map from a module (module writer) instead.
fn signable(m: &fm_install::feature::Manifest) -> Result<(), String> {
    match !m.memory_maps.is_empty() && !fm_install::memory::camera_only(m) {
        true => Err(format!("{}: its memory session record pins the library bytes; a signed library cannot go in (use a module map)", m.id)),
        false => Ok(()),
    }
}

/// Ok when `signed` is `built` plus an Authenticode signature: the same image digest input, and
/// at most 7 zero bytes of padding before the certificate table (signtool aligns it to 8).
fn only_signature_added(built: &[u8], signed: &[u8]) -> Result<(), String> {
    let (a, has) = image(built).map_err(|e| format!("the built library: {e}"))?;
    if has { return Err("the built library already has a signature".into()); }
    let (b, has) = image(signed).map_err(|e| format!("the signed library: {e}"))?;
    if !has { return Err("the signed library has no signature".into()); }
    let pad = b.len().checked_sub(a.len()).filter(|p| *p < 8);
    match pad.is_some() && b[..a.len()] == a[..] && b[a.len()..].iter().all(|x| *x == 0) {
        true => Ok(()),
        false => Err("the signed library is not the built library: more than a signature changed".into()),
    }
}

/// A PE file as its Authenticode digest sees it (PE/COFF spec, "Authenticode PE image hash"):
/// the checksum and the certificate table entry are zero, and the bytes stop at the certificate
/// table. A table must end the file at an 8-byte boundary. Also says if the file has a table.
fn image(bytes: &[u8]) -> Result<(Vec<u8>, bool), String> {
    let bad = || "not a PE file".to_string();
    let u16_at = |o: usize| bytes.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or_else(bad);
    let u32_at = |o: usize| bytes.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize).ok_or_else(bad);
    if bytes.get(..2) != Some(b"MZ") { return Err(bad()); }
    let pe = u32_at(0x3c)?;
    if bytes.get(pe..pe.saturating_add(4)) != Some(b"PE\0\0") { return Err(bad()); }
    let opt = pe + 24;
    // The data directories follow NumberOfRvaAndSizes; entry 4 is the certificate table.
    let dirs = match u16_at(opt)? { 0x10b => opt + 96, 0x20b => opt + 112, _ => return Err(bad()) };
    if u32_at(dirs - 4)? < 5 { return Err("the PE file has no certificate table entry".into()); }
    let (checksum, entry) = (opt + 64, dirs + 4 * 8);
    let (at, size) = (u32_at(entry)?, u32_at(entry + 4)?);
    let end = match size {
        0 => bytes.len(),
        _ if at % 8 == 0 && at.checked_add(size) == Some(bytes.len()) && at >= entry + 8 => at,
        _ => return Err("the certificate table is not at the end of the file".into()),
    };
    let mut v = bytes[..end].to_vec();
    v[checksum..checksum + 4].fill(0);
    v[entry..entry + 8].fill(0);
    Ok((v, size != 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PE32+ header with room for 16 data directories, then `body`.
    fn pe(body: &[u8]) -> Vec<u8> {
        let mut v = vec![0u8; 0x40 + 24 + 240];
        v[..2].copy_from_slice(b"MZ");
        v[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        v[0x40..0x44].copy_from_slice(b"PE\0\0");
        let opt = 0x40 + 24;
        v[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        v[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(body);
        v
    }

    /// What signtool does: a new checksum, padding to 8, the table entry, the table at the end.
    fn sign(built: &[u8]) -> Vec<u8> {
        let (opt, mut v) = (0x40 + 24, built.to_vec());
        v[opt + 64..opt + 68].copy_from_slice(&0x1234u32.to_le_bytes());
        v.resize(built.len().div_ceil(8) * 8, 0);
        let (at, table) = (v.len() as u32, [7u8; 24]);
        v[opt + 144..opt + 148].copy_from_slice(&at.to_le_bytes());
        v[opt + 148..opt + 152].copy_from_slice(&(table.len() as u32).to_le_bytes());
        v.extend_from_slice(&table);
        v
    }

    // The check accepts a signature only. A changed code byte, an unsigned "signed" file, a
    // built file that is already signed, and a table that does not end the file are refused.
    #[test]
    fn only_a_signature_can_be_added() {
        let built = pe(b"code bytes");
        let signed = sign(&built);
        only_signature_added(&built, &signed).unwrap();
        let mut changed = signed.clone();
        changed[0x40 + 24 + 240] ^= 1;
        assert!(only_signature_added(&built, &changed).unwrap_err().contains("more than a signature"));
        assert!(only_signature_added(&built, &built).unwrap_err().contains("has no signature"));
        assert!(only_signature_added(&signed, &signed).unwrap_err().contains("already has a signature"));
        let mut tail = signed.clone();
        tail.push(0);
        assert!(only_signature_added(&built, &tail).unwrap_err().contains("not at the end"));
        assert!(only_signature_added(b"MZ", &signed).unwrap_err().contains("not a PE file"));
    }

    // A feature with an embedded map and a session record (wet-dry 0.2.0) cannot take a signed
    // library; a module writer (wet-dry 0.3.0) and a feature without maps can.
    #[test]
    fn a_session_record_feature_is_refused() {
        let m = |extra: serde_json::Value| {
            let mut v = serde_json::json!({ "id": "wet-dry", "name": "W", "version": "0.2.0", "description": "d", "api": fm_install::feature::FEATURE_API,
                "games": ["msfs2024"], "capabilities": ["vars_read", "vars_memory", "vars_write", "ui_app"], "multiplayer": "allowed" });
            v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            serde_json::from_value::<fm_install::feature::Manifest>(v).unwrap()
        };
        let session = m(serde_json::json!({ "memory_maps": [{ "id": "msfs2024-weather", "version": "1.1.0", "sha256": "1".repeat(64) }] }));
        assert!(signable(&session).unwrap_err().contains("session record"));
        let writer = m(serde_json::json!({ "requires": [{ "interface": "mirrored-setter-v2", "versions": "^1.0", "map": "msfs2024-ground-wetness" }] }));
        signable(&writer).unwrap();
        signable(&m(serde_json::json!({}))).unwrap();
    }

    // repack on a real package: only the library entry changes, the result is a valid package
    // with the signed library, and a library with other code is refused.
    #[test]
    fn repack_puts_the_signed_library_in_the_package() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        std::fs::create_dir_all(src.join("ui")).unwrap();
        let m = serde_json::json!({ "id": "trip", "name": "Trip", "version": "0.2.2", "description": "d", "api": fm_install::feature::FEATURE_API,
            "games": ["msfs2024"], "capabilities": ["vars_read", "ui_app"], "multiplayer": "allowed", "ui": "index.html" });
        std::fs::write(src.join(fm_install::FEATURE_MANIFEST), m.to_string()).unwrap();
        std::fs::write(src.join("ui/index.html"), "<p>trip</p>").unwrap();
        let lib = t.path().join("feature.dll");
        let built_lib = pe(b"library code");
        std::fs::write(&lib, &built_lib).unwrap();
        let mut built = Cursor::new(Vec::new());
        fm_install::feature::pack(&src, &[lib], &mut built).unwrap();
        let built = built.into_inner();
        let signed_lib = sign(&built_lib);
        let out = repack(&built, &signed_lib).unwrap();
        let (mut a, mut b) = (zip::ZipArchive::new(Cursor::new(&built)).unwrap(), zip::ZipArchive::new(Cursor::new(&out)).unwrap());
        assert_eq!(a.len(), b.len());
        for i in 0..a.len() {
            let (mut x, mut y) = (vec![], vec![]);
            let name = a.by_index(i).unwrap().name().to_string();
            a.by_index(i).unwrap().read_to_end(&mut x).unwrap();
            b.by_index(i).unwrap().read_to_end(&mut y).unwrap();
            assert_eq!(b.by_index(i).unwrap().name(), name);
            assert_eq!(y, if name == LIBRARY { signed_lib.clone() } else { x });
        }
        fm_install::feature::inspect(Cursor::new(&out)).unwrap();
        let other = sign(&pe(b"other code!!"));
        assert!(repack(&built, &other).unwrap_err().contains("more than a signature"));
    }
}
