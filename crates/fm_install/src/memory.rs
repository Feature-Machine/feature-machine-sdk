// Memory-enabled features carry independently signed maps and one signed session record. The
// record binds consent metadata, host requirements and every package file before code loads.
// Camera-only features (plan 2026-10-20-camera-map-updates 6c, 2026-10-21) carry their
// camera maps as plain package files, bound by the manifest pins, with no signature of ours.
use crate::feature::{Manifest, MapDependency};
use fm_memory::{Locator, Map};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};
use std::path::Path;

pub const SESSION: &str = "memory-session.json";
pub const SIGNATURE: &str = "memory-session.json.sig";
pub const HOST_VERSION: &str = "0.1.0";
const LIMIT: usize = 256 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MapPackage {
    pub kind: String, pub contract: String,
    /// Exactly one of the two: an exact map (pinned to one exe digest) or a camera locator
    /// recipe (limits only, not pinned; plan 2026-10-20-camera-map-updates, owner decision 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map: Option<Map>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<Locator>,
}

/// A verified map dependency, as the host uses it (signed by us, or pinned by a camera-only feature).
#[derive(Clone, Debug)]
pub enum Signed { Map(Map), Locator(Locator) }

impl Signed {
    pub fn id(&self) -> &str { match self { Self::Map(m) => &m.id, Self::Locator(l) => &l.id } }
    pub fn version(&self) -> &str { match self { Self::Map(m) => &m.version, Self::Locator(l) => &l.version } }
    pub fn game(&self) -> &str { match self { Self::Map(m) => &m.game, Self::Locator(l) => &l.game } }
    /// What a render_depth feature without vars_memory may pin: camera maps and locators.
    pub fn is_camera(&self) -> bool { match self { Self::Map(m) => is_camera(m), Self::Locator(_) => true } }
    /// The camera contract of a verified map: only camera-projection maps hold camera symbols
    /// (contract_matches), so a camera map has that contract. None for other maps.
    pub fn camera_contract(&self) -> Option<&'static str> {
        match self { Self::Locator(_) => Some(LOCATOR_CONTRACT), Self::Map(m) => is_camera(m).then_some(CAMERA_CONTRACT) }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub kind: String,
    pub host_version: String,
    pub host_api: u32,
    pub manifest: Manifest,
    pub files: BTreeMap<String, String>,
}

pub fn digest(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }
pub fn digest_valid(s: &str) -> bool { s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) }
pub fn map_path(m: &MapDependency) -> String { format!("maps/{}/{}/map.json", m.id, m.version) }

/// One configured public key serves app packages, reviewed maps and session records. Private
/// signing material stays in the approved store and never enters the app or native host.
pub fn public_key() -> String { crate::package::app_key().into() }

fn text(bytes: Vec<u8>) -> Result<String, String> { String::from_utf8(bytes).map_err(|e| e.to_string()) }

pub fn verify_map(bytes: &[u8], signature: &str, key: &str, expected: &MapDependency) -> Result<Signed, String> {
    crate::package::verify(bytes, signature, key, &expected.version)?;
    pinned(bytes, expected)
}

/// The pin rules: the file has the pinned digest, passes the body rules and has the pinned id
/// and version. Both trust paths use them.
fn pinned(bytes: &[u8], expected: &MapDependency) -> Result<Signed, String> {
    if digest(bytes) != expected.sha256 { return Err(format!("{}: map digest differs from its dependency (fm_pack map pin writes it)", expected.id)); }
    let signed = parse_package(bytes)?;
    if signed.id() != expected.id || signed.version() != expected.version { return Err("map package identity or contract differs".into()); }
    Ok(signed)
}

/// A feature without vars_memory and vars_write. Its manifest check allows only render_depth
/// camera maps, and the map check below allows only camera contracts. Its maps need no
/// signature of ours: the feature library already runs in the game without one, so our
/// signature on a read-only map adds no safety and only stops a creator (owner decision, plan 6c).
pub fn camera_only(m: &Manifest) -> bool {
    !m.capabilities.contains(&fm_feature_sdk::Capability::VarsMemory) && !m.capabilities.contains(&fm_feature_sdk::Capability::VarsWrite)
}

/// Rules of each map that a feature may use: a camera map without vars_memory, the contract that
/// the manifest names for it (consent compares that name), and a game of the feature.
fn allowed(m: &Manifest, map: &Signed) -> Result<(), String> {
    if !m.capabilities.contains(&fm_feature_sdk::Capability::VarsMemory) && !map.is_camera() {
        return Err(format!("{}: only camera maps are allowed without vars_memory", map.id()));
    }
    if m.map_contracts.get(map.id()).is_some_and(|c| Some(c.as_str()) != map.camera_contract()) {
        return Err(format!("{}: map contract differs from map_contracts", map.id()));
    }
    if !m.games.iter().any(|game| serde_json::to_value(game).ok().and_then(|v| v.as_str().map(str::to_owned)).as_deref() == Some(map.game())) {
        return Err("map is for another game".into());
    }
    Ok(())
}

/// The maps of a camera-only feature, read from its package, folder or source by `read`.
fn verify_pinned(m: &Manifest, mut read: impl FnMut(&str) -> Result<Vec<u8>, String>) -> Result<Vec<Signed>, String> {
    m.check().map_err(|e| e.to_string())?;
    m.memory_maps.iter().map(|dependency| {
        let map = pinned(&read(&map_path(dependency))?, dependency)?;
        allowed(m, &map)?;
        Ok(map)
    }).collect()
}

/// The body rules of a map package without its signature: kind, contract and exactly one valid
/// map or locator. The creator tools (`fm_pack map`) use it on unsigned files, so a file that
/// passes there fails in the host only on its signature or dependency.
pub fn parse_package(bytes: &[u8]) -> Result<Signed, String> {
    let p: MapPackage = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if p.kind != "memory-map" || !contract_matches(&p) { return Err("map package identity or contract differs".into()); }
    match (p.map, p.locator) {
        (Some(m), None) => { m.check().map_err(|e| e.to_string())?; Ok(Signed::Map(m)) }
        (None, Some(l)) => { l.check().map_err(|e| e.to_string())?; Ok(Signed::Locator(l)) }
        _ => Err("a map package holds one map or one locator".into()),
    }
}

/// The signed contract names what the host will execute. A scalar map cannot carry a setter,
/// and a setter map must use one, so a review of one contract never approves the other. A
/// camera map has only read-only projections behind pointer steps, and no other map has those.
/// A locator recipe has its own contract and no symbols, so no map review approves a scan.
/// The v1 contracts are for one exe (digest pinned); `mirrored-setter-v2` is for any build and
/// has no digest (plan 2026-10-23: its objects are proved by class, then watched, at run time).
fn contract_matches(p: &MapPackage) -> bool {
    if p.contract == LOCATOR_CONTRACT { return p.map.is_none() && p.locator.is_some(); }
    let Some(map) = &p.map else { return false };
    let setters = map.symbols.iter().any(|s| s.setter.is_some());
    let cameras = map.symbols.iter().filter(|s| camera_symbol(s)).count();
    let pointers = map.symbols.iter().any(|s| s.rip.is_some() || !s.steps.is_empty() || s.kind == fm_memory::Kind::Projection);
    let exact = map.sha256.is_some();
    match p.contract.as_str() {
        "anchored-scalar-v1" => exact && !setters && !pointers,
        "mirrored-setter-v1" => exact && setters && !pointers,
        SETTER_V2_CONTRACT => !exact && setters && !pointers,
        CAMERA_CONTRACT => exact && cameras == map.symbols.len(),
        _ => false,
    }
}

pub const SETTER_V2_CONTRACT: &str = "mirrored-setter-v2";

pub const CAMERA_CONTRACT: &str = "camera-projection-v1";
pub const LOCATOR_CONTRACT: &str = "camera-locator-v1";

fn camera_symbol(s: &fm_memory::Symbol) -> bool {
    s.kind == fm_memory::Kind::Projection && s.access == fm_memory::Access::Read && s.setter.is_none() && s.rip.is_some() && !s.steps.is_empty()
}

/// True for a camera map: what a render_depth feature without vars_memory may pin.
pub fn is_camera(map: &Map) -> bool { map.symbols.iter().all(camera_symbol) }

fn verify_with(key: &str, m: &Manifest, mut read: impl FnMut(&str) -> Result<Vec<u8>, String>) -> Result<Vec<Signed>, String> {
    m.check().map_err(|e| e.to_string())?;
    let bytes = read(SESSION)?;
    crate::package::verify(&bytes, &text(read(SIGNATURE)?)?, key, &m.version)?;
    let s: Session = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if s.kind != "feature-memory-session" || s.host_version != HOST_VERSION || s.host_api != fm_feature_sdk::API_VERSION || s.manifest != *m || s.files.len() > 4096 {
        return Err("signed session differs from feature metadata or this host".into());
    }
    if !s.files.contains_key(crate::FEATURE_MANIFEST) || !s.files.contains_key(&crate::feature::parts::library()) {
        return Err("signed session is missing its manifest or library".into());
    }
    let mut total = 0usize;
    let mut contents = BTreeMap::new();
    for (path, sha) in &s.files {
        crate::feature::safe_rel(path).map_err(|e| e.to_string())?;
        if [SESSION, SIGNATURE].contains(&path.as_str()) || !digest_valid(sha) { return Err("invalid signed file record".into()); }
        let b = read(path)?;
        total = total.checked_add(b.len()).filter(|n| *n <= LIMIT).ok_or("signed session exceeds size limit")?;
        if digest(&b) != *sha { return Err(format!("signed file changed: {path}")); }
        if path.starts_with("maps/") { contents.insert(path.clone(), b); }
    }
    let mut maps = Vec::new();
    for dependency in &m.memory_maps {
        let path = map_path(dependency);
        let bytes = contents.get(&path).ok_or_else(|| format!("missing signed map: {path}"))?;
        let sig = contents.get(&format!("{path}.sig")).ok_or("map signature missing from the signed session")?;
        let map = verify_map(bytes, std::str::from_utf8(sig).map_err(|e| e.to_string())?, key, dependency)?;
        allowed(m, &map)?;
        maps.push(map);
    }
    Ok(maps)
}

/// Consent and unpack inspect the same archive. No unsigned extra file may join a signed
/// session; a camera-only feature has no session, and its pins bind its maps.
pub fn verify_archive<R: Read + Seek>(a: &mut zip::ZipArchive<R>, m: &Manifest) -> Result<Vec<Signed>, String> {
    crate::checked_entries(a, crate::Kind::Feature).map_err(|e| e.to_string())?;
    let read = |a: &mut zip::ZipArchive<R>, path: &str| -> Result<Vec<u8>, String> {
        let mut f = a.by_name(path).map_err(|e| format!("{path}: {e}"))?;
        if f.size() > LIMIT as u64 { return Err("package file exceeds size limit".into()); }
        let mut b = Vec::new();
        f.read_to_end(&mut b).map_err(|e| e.to_string())?;
        Ok(b)
    };
    if camera_only(m) { return verify_pinned(m, |path| read(a, path)); }
    let maps = verify_with(&public_key(), m, |path| read(a, path))?;
    let mut bytes = Vec::new();
    a.by_name(SESSION).map_err(|e| e.to_string())?.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let s: Session = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let expected: BTreeSet<_> = s.files.keys().map(String::as_str).chain([SESSION, SIGNATURE]).collect();
    for i in 0..a.len() {
        let f = a.by_index(i).map_err(|e| e.to_string())?;
        if !f.is_dir() && !expected.contains(f.name()) { return Err(format!("unsigned package file: {}", f.name())); }
    }
    Ok(maps)
}

/// The version store is also a trust entry point. Legacy stores keep their existing contract,
/// while any declared memory access or session record requires full signed verification.
pub(crate) fn verify_install<R: Read + Seek>(a: &mut zip::ZipArchive<R>, version: &str) -> Result<(), String> {
    let mut bytes = Vec::new();
    a.by_name(crate::FEATURE_MANIFEST).map_err(|e| e.to_string())?.take(LIMIT as u64 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > LIMIT { return Err("feature manifest exceeds size limit".into()); }
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let memory = value["capabilities"].as_array().is_some_and(|caps| caps.iter().any(|c| matches!(c.as_str(), Some("vars_memory" | "vars_write"))))
        || value["memory_maps"].as_array().is_some_and(|maps| !maps.is_empty())
        || a.by_name(SESSION).is_ok() || a.by_name(SIGNATURE).is_ok();
    if memory {
        let m: Manifest = serde_json::from_value(value).map_err(|e| e.to_string())?;
        if m.version != version { return Err("signed feature version differs from install version".into()); }
        verify_archive(a, &m)?;
    }
    Ok(())
}

/// Hosts recheck installed content at game start. A missing signature or incompatible map
/// refuses the complete memory session; it never falls back to an unsigned development map.
pub fn verify_directory(root: &Path, m: &Manifest) -> Result<Vec<Signed>, String> {
    let read = |name: &str| -> Result<Vec<u8>, String> {
        let path = root.join(crate::feature::safe_rel(name).map_err(|e| e.to_string())?);
        if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > LIMIT as u64 { return Err("installed file exceeds size limit".into()); }
        std::fs::read(path).map_err(|e| e.to_string())
    };
    if camera_only(m) { return verify_pinned(m, read); }
    let maps = verify_with(&public_key(), m, read)?;
    let session: Session = serde_json::from_slice(&std::fs::read(root.join(SESSION)).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let expected: BTreeSet<_> = session.files.keys().map(String::as_str).chain([SESSION, SIGNATURE, crate::COMPLETE, crate::PACKAGE_STAMP]).collect();
    check_inventory(root, root, &expected, &mut 0)?;
    Ok(maps)
}

// Refuse links before recursion and bound the traversal. A completion marker belongs to the
// version store, but all code, map and UI files must belong to the signed session.
fn check_inventory(root: &Path, dir: &Path, expected: &BTreeSet<&str>, count: &mut usize) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
    if is_link(&metadata) { return Err("installed session contains a link".into()); }
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        *count += 1;
        if *count > 8192 { return Err("installed session exceeds file limit".into()); }
        let path = entry.map_err(|e| e.to_string())?.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if is_link(&metadata) { return Err("installed session contains a link".into()); }
        if metadata.is_dir() { check_inventory(root, &path, expected, count)?; }
        else {
            let name = path.strip_prefix(root).map_err(|e| e.to_string())?.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
            if !expected.contains(name.as_str()) { return Err(format!("unsigned installed file: {name}")); }
        }
    }
    Ok(())
}

fn is_link(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    { use std::os::windows::fs::MetadataExt; metadata.file_attributes() & 0x400 != 0 }
    #[cfg(not(windows))]
    { metadata.file_type().is_symlink() }
}

pub(crate) fn verify_source(root: &Path, libraries: &[std::path::PathBuf], shaders: &[(String, Vec<u8>)], m: &Manifest) -> Result<(), String> {
    if camera_only(m) {
        return verify_pinned(m, |name| std::fs::read(root.join(crate::feature::safe_rel(name).map_err(|e| e.to_string())?)).map_err(|e| format!("{name}: {e}"))).map(|_| ());
    }
    let signed: Session = serde_json::from_slice(&std::fs::read(root.join(SESSION)).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    if signed.files != seal(root, libraries, shaders)?.files { return Err("source files differ from the signed session".into()); }
    verify_with(&public_key(), m, |name| {
        if let Some((_, b)) = shaders.iter().find(|(n, _)| name == shader_path(n)) { return Ok(b.clone()); }
        let library = libraries.iter().find(|p| match p.extension().and_then(|e| e.to_str()) {
            Some("dll") => name == "bin/windows/feature.dll", Some("so") => name == "bin/linux/libfeature.so", _ => false,
        });
        let path = library.cloned().unwrap_or(root.join(crate::feature::safe_rel(name).map_err(|e| e.to_string())?));
        if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > LIMIT as u64 { return Err("source file exceeds size limit".into()); }
        std::fs::read(path).map_err(|e| e.to_string())
    }).map(|_| ())
}

/// Where a compiled shader sits in a package (feature::pack_shaders writes it there).
fn shader_path(name: &str) -> String { format!("{}/{name}.cso", crate::feature::parts::SHADERS) }

/// Sealing records hashes only. The developer signs this record with the existing signer,
/// then pack and install run full verification before accepting its result. Compiled shaders
/// are package files too (a depth feature can pin a camera map), so the record lists them.
pub fn seal(root: &Path, libraries: &[std::path::PathBuf], shaders: &[(String, Vec<u8>)]) -> Result<Session, String> {
    let m: Manifest = serde_json::from_slice(&std::fs::read(root.join(crate::FEATURE_MANIFEST)).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    m.check().map_err(|e| e.to_string())?;
    if m.memory_maps.is_empty() { return Err("no memory dependencies to seal".into()); }
    let mut files = BTreeMap::new();
    files.insert(crate::FEATURE_MANIFEST.into(), digest(&std::fs::read(root.join(crate::FEATURE_MANIFEST)).map_err(|e| e.to_string())?));
    for lib in libraries {
        let name = match lib.extension().and_then(|e| e.to_str()) { Some("dll") => "bin/windows/feature.dll", Some("so") => "bin/linux/libfeature.so", _ => return Err("not a feature library".into()) };
        if files.insert(name.into(), digest(&std::fs::read(lib).map_err(|e| e.to_string())?)).is_some() { return Err("duplicate feature library".into()); }
    }
    for part in ["maps", "ui", "ats-mod", "msfs-community"] {
        let dir = root.join(part);
        if !dir.is_dir() { continue; }
        let mut paths = Vec::new();
        crate::package::walk(&dir, &dir, &mut paths).map_err(|e| e.to_string())?;
        for path in paths { files.insert(format!("{part}/{path}"), digest(&std::fs::read(dir.join(path)).map_err(|e| e.to_string())?)); }
    }
    for (name, bytes) in shaders {
        if files.insert(shader_path(name), digest(bytes)).is_some() { return Err("duplicate compiled shader".into()); }
    }
    Ok(Session { kind: "feature-memory-session".into(), host_version: HOST_VERSION.into(), host_api: fm_feature_sdk::API_VERSION, manifest: m, files })
}

#[cfg(test)]
mod tests {
    use super::*;
    // These public fixtures use the configured signing key. Their library bytes are plain text
    // and their module hash is synthetic, so they cannot load code or resolve a real game.
    fn files() -> BTreeMap<String, Vec<u8>> {
        [
            ("feature.json", include_bytes!("../tests/data/memory/feature.json").as_slice()),
            (SESSION, include_bytes!("../tests/data/memory/memory-session.json").as_slice()),
            (SIGNATURE, include_bytes!("../tests/data/memory/memory-session.json.sig").as_slice()),
            ("maps/fixture-map/1.0.0/map.json", include_bytes!("../tests/data/memory/map.json").as_slice()),
            ("maps/fixture-map/1.0.0/map.json.sig", include_bytes!("../tests/data/memory/map.json.sig").as_slice()),
            ("bin/windows/feature.dll", b"not an executable library".as_slice()),
            ("bin/linux/libfeature.so", b"not an executable library".as_slice()),
        ].into_iter().map(|(name, bytes)| (name.into(), bytes.to_vec())).collect()
    }
    fn manifest(files: &BTreeMap<String, Vec<u8>>) -> Manifest { serde_json::from_slice(&files["feature.json"]).unwrap() }
    fn verify(files: &BTreeMap<String, Vec<u8>>, m: &Manifest) -> Result<Vec<Signed>, String> {
        verify_with(&public_key(), m, |name| files.get(name).cloned().ok_or_else(|| format!("missing {name}")))
    }
    fn archive(files: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in files { zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap(); zip.write_all(bytes).unwrap(); }
        zip.finish().unwrap().into_inner()
    }
    // A depth feature with a camera map ships compiled shaders (plan 7.5): the record lists
    // each .cso, so the package and the installed folder hold no unsigned file.
    #[test]
    fn the_session_record_lists_compiled_shaders() {
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in files().into_iter().filter(|(n, _)| n == "feature.json" || n.starts_with("maps/")) {
            std::fs::create_dir_all(dir.path().join(&name).parent().unwrap()).unwrap();
            std::fs::write(dir.path().join(&name), bytes).unwrap();
        }
        let lib = dir.path().join("feature.dll");
        std::fs::write(&lib, b"library").unwrap();
        let shader = ("mirage".to_string(), b"DXBC container".to_vec());
        let s = seal(dir.path(), std::slice::from_ref(&lib), std::slice::from_ref(&shader)).unwrap();
        assert_eq!(s.files.get("shaders/mirage.cso"), Some(&digest(&shader.1)));
        assert!(seal(dir.path(), std::slice::from_ref(&lib), &[shader.clone(), shader]).is_err());
        assert!(!seal(dir.path(), &[lib], &[]).unwrap().files.keys().any(|k| k.starts_with("shaders/")));
    }
    #[test]
    fn complete_signed_session_passes_and_each_changed_file_is_refused() {
        let original = files();
        let m = manifest(&original);
        assert_eq!(verify(&original, &m).unwrap()[0].id(), "fixture-map");
        crate::feature::inspect(std::io::Cursor::new(archive(&original))).unwrap();
        for name in original.keys() {
            let mut changed = original.clone();
            changed.get_mut(name).unwrap().push(b'!');
            assert!(verify(&changed, &m).is_err(), "{name}");
            let mut missing = original.clone();
            missing.remove(name);
            assert!(verify(&missing, &m).is_err(), "{name}");
        }
    }
    #[test]
    fn signatures_bind_kind_identity_version_dependencies_and_consent() {
        let files = files();
        let m = manifest(&files);
        let mut changed = m.clone();
        changed.capabilities.push(fm_feature_sdk::Capability::VarsWrite);
        assert!(verify(&files, &changed).is_err());
        changed = m.clone(); changed.memory_maps[0].sha256 = "b".repeat(64);
        assert!(verify(&files, &changed).is_err());
        let path = map_path(&m.memory_maps[0]);
        let mut wrong = m.memory_maps[0].clone();
        wrong.id = "another-map".into();
        let sig = std::str::from_utf8(&files[&format!("{path}.sig")]).unwrap();
        assert!(verify_map(&files[&path], sig, &public_key(), &wrong).is_err());
        wrong = m.memory_maps[0].clone(); wrong.version = "1.0.1".into();
        assert!(verify_map(&files[&path], sig, &public_key(), &wrong).is_err());
        let key = include_str!("../tests/data/test-key.pub");
        assert!(verify_map(&files[&path], sig, key, &m.memory_maps[0]).is_err());
        let mut extra = files.clone(); extra.insert("ui/unreviewed.js".into(), b"extra".to_vec());
        assert!(crate::feature::inspect(std::io::Cursor::new(archive(&extra))).is_err());
    }

    // Exercise the install and host preflight interfaces with the same signed artifact.
    // A trust check at consent alone must not permit changed bytes at unpack or game start.
    #[test]
    fn install_and_host_refuse_changed_or_extra_files() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.agent-work/tmp").join(format!("signed-install-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let store = crate::Install::feature(root.join("store"));
        let mut changed = files();
        let m = manifest(&changed);
        changed.get_mut("bin/windows/feature.dll").unwrap().push(b'!');
        assert!(store.unpack(&m.version, std::io::Cursor::new(archive(&changed)), false).is_err(), "unpack must verify the signed session");
        let original = files();
        store.unpack(&m.version, std::io::Cursor::new(archive(&original)), false).unwrap();
        store.activate(&m.version).unwrap();
        let dir = store.active().unwrap().1;
        // The app writes the package stamp after each unpack; it is a version store file.
        std::fs::write(dir.join(crate::PACKAGE_STAMP), "a".repeat(64)).unwrap();
        verify_directory(&dir, &m).unwrap();
        std::fs::write(dir.join("bin/windows/unreviewed.dll"), b"unsigned sidecar").unwrap();
        assert!(verify_directory(&dir, &m).is_err(), "host preflight must reject unsigned files");
        let resolved = root.canonicalize().unwrap();
        assert!(resolved.starts_with(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.agent-work/tmp").canonicalize().unwrap()));
        std::fs::remove_dir_all(resolved).unwrap();
    }

    #[test]
    fn setter_maps_need_their_own_signed_contract() {
        let package = |contract: &str, setter: bool| -> MapPackage {
            let mut symbol = serde_json::json!({ "name": "s", "pattern": "01 02 03 04", "offset": 8, "kind": "f32", "access": "write", "min": 0.0, "max": 1.0,
                "type_guard": { "offset": -8, "rva": 64 } });
            if setter { symbol["setter"] = serde_json::json!({ "contract": "mirrored_notify_v1", "mirror": -4, "clamp_min": 8, "clamp_max": 12, "notify_slot": 2 }); }
            serde_json::from_value(serde_json::json!({ "kind": "memory-map", "contract": contract, "map": { "schema": 1, "id": "m", "version": "1.0.0",
                "game": "msfs2024", "module": "x.exe", "sha256": "a".repeat(64), "symbols": [symbol] } })).unwrap()
        };
        assert!(contract_matches(&package("anchored-scalar-v1", false)));
        assert!(contract_matches(&package("mirrored-setter-v1", true)));
        assert!(!contract_matches(&package("anchored-scalar-v1", true)), "a scalar review cannot approve native calls");
        assert!(!contract_matches(&package("mirrored-setter-v1", false)));
        assert!(!contract_matches(&package("other-v1", false)));
        assert!(!contract_matches(&package(SETTER_V2_CONTRACT, true)), "a v2 map has no exe digest");
    }

    // mirrored-setter-v2 (plan 2026-10-23): setters with a class guard and no exe digest. A v1
    // contract never approves a map without a digest, and v2 never approves pointers.
    #[test]
    fn build_independent_setter_maps_need_the_v2_contract() {
        let package = |contract: &str, setter: bool, rip: bool| -> MapPackage {
            let mut symbol = serde_json::json!({ "name": "s", "pattern": "01 02 03 04 05 06 07 08", "offset": 16, "kind": "f32", "access": "write", "min": 0.0, "max": 1.0,
                "class_guard": { "offset": -8, "member": "01 02 03 04", "members": 100 } });
            if setter { symbol["setter"] = serde_json::json!({ "contract": "mirrored_notify_v1", "mirror": -4, "clamp_min": 8, "clamp_max": 12, "notify_slot": 2 }); }
            if rip { symbol["rip"] = 0.into(); }
            serde_json::from_value(serde_json::json!({ "kind": "memory-map", "contract": contract, "map": { "schema": 1, "id": "m", "version": "1.1.0",
                "game": "msfs2024", "module": "x.exe", "symbols": [symbol] } })).unwrap()
        };
        let v2 = package(SETTER_V2_CONTRACT, true, false);
        assert!(contract_matches(&v2));
        assert!(parse_package(&serde_json::to_vec(&v2).unwrap()).is_ok(), "the map rules pass too");
        assert!(!contract_matches(&package("mirrored-setter-v1", true, false)), "v1 is for one exe");
        assert!(!contract_matches(&package("anchored-scalar-v1", false, false)), "v1 is for one exe");
        assert!(!contract_matches(&package(SETTER_V2_CONTRACT, false, false)), "v2 holds setters");
        assert!(!contract_matches(&package(SETTER_V2_CONTRACT, true, true)), "v2 holds no pointer");
    }

    #[test]
    fn camera_maps_need_the_camera_contract_and_only_projections() {
        let package = |contract: &str, kind: &str, steps: bool| -> MapPackage {
            let mut symbol = serde_json::json!({ "name": "view-projection", "pattern": "48 8B 0D ?? ?? ?? ?? C3", "offset": 0, "rip": 3,
                "kind": kind, "access": "read", "min": 5.0, "max": 150.0 });
            if steps { symbol["steps"] = serde_json::json!([1240, 12896]); }
            serde_json::from_value(serde_json::json!({ "kind": "memory-map", "contract": contract, "map": { "schema": 1, "id": "msfs2024-camera",
                "version": "1.0.0", "game": "msfs2024", "module": "x.exe", "sha256": "a".repeat(64), "symbols": [symbol] } })).unwrap()
        };
        let camera = package(CAMERA_CONTRACT, "projection", true);
        assert!(contract_matches(&camera) && is_camera(camera.map.as_ref().unwrap()));
        assert!(!contract_matches(&package("anchored-scalar-v1", "projection", true)), "a scalar review cannot approve pointer steps");
        assert!(!contract_matches(&package(CAMERA_CONTRACT, "f32", true)), "a camera map holds projections only");
        assert!(!contract_matches(&package(CAMERA_CONTRACT, "projection", false)), "a camera is behind pointer steps");
        assert!(!is_camera(package("anchored-scalar-v1", "f32", false).map.as_ref().unwrap()), "a scalar map is not a camera map");
    }

    // A locator recipe needs its own contract and holds no map; a map review never approves a
    // scan, and a locator review never approves symbols (owner decision 2: read-only, unpinned).
    #[test]
    fn locators_need_the_locator_contract_and_no_map() {
        let locator = serde_json::json!({ "schema": 1, "id": "msfs2024-camera-locator", "version": "1.0.0", "game": "msfs2024",
            "module": "FlightSimulator2024.exe", "near": [0.005, 2.0], "fov": [5.0, 150.0], "aspect_tolerance": 0.01 });
        let map = serde_json::json!({ "schema": 1, "id": "m", "version": "1.0.0", "game": "msfs2024", "module": "x.exe", "sha256": "a".repeat(64),
            "symbols": [{ "name": "s", "pattern": "01 02 03 04", "offset": 8, "kind": "f32", "access": "read", "min": 0.0, "max": 1.0 }] });
        let package = |contract: &str, body: serde_json::Value| -> MapPackage {
            let mut p = serde_json::json!({ "kind": "memory-map", "contract": contract });
            for (k, v) in body.as_object().unwrap() { p[k] = v.clone(); }
            serde_json::from_value(p).unwrap()
        };
        assert!(contract_matches(&package(LOCATOR_CONTRACT, serde_json::json!({ "locator": locator }))));
        assert!(!contract_matches(&package(CAMERA_CONTRACT, serde_json::json!({ "locator": locator }))), "a camera map review cannot approve a scan");
        assert!(!contract_matches(&package(LOCATOR_CONTRACT, serde_json::json!({ "map": map }))));
        assert!(!contract_matches(&package(LOCATOR_CONTRACT, serde_json::json!({ "map": map, "locator": locator }))));
        let file = |contract: &str| serde_json::to_vec(&package(contract, serde_json::json!({ "locator": locator }))).unwrap();
        assert!(matches!(parse_package(&file(LOCATOR_CONTRACT)), Ok(Signed::Locator(_))));
        assert!(parse_package(&file(CAMERA_CONTRACT)).is_err(), "the creator tools apply the same contract rules (review round 2)");
        assert!(Signed::Locator(serde_json::from_value(locator).unwrap()).is_camera(), "render_depth may pin a locator");
    }

    // Owner decision 2026-10-21 (plan 6c): a camera-only feature carries its maps like its library, with
    // no signature of ours, so a creator ships and repairs them alone. Its pins bind each map,
    // and only camera contracts for its games pass.
    fn camera_files(map: serde_json::Value) -> (BTreeMap<String, Vec<u8>>, Manifest) {
        let bytes = serde_json::to_vec(&map).unwrap();
        let m: Manifest = serde_json::from_value(serde_json::json!({ "id": "camera-fixture", "name": "Camera fixture", "version": "1.0.0", "description": "d",
            "api": 5, "games": ["msfs2024"], "capabilities": ["render_pass", "render_depth"], "multiplayer": "allowed",
            "memory_maps": [{ "id": map["locator"]["id"].as_str().or(map["map"]["id"].as_str()).unwrap(), "version": "1.0.0", "sha256": digest(&bytes) }] })).unwrap();
        let files = [
            ("feature.json".to_string(), serde_json::to_vec(&m).unwrap()),
            (map_path(&m.memory_maps[0]), bytes),
            ("bin/windows/feature.dll".into(), b"not an executable library".to_vec()),
            ("bin/linux/libfeature.so".into(), b"not an executable library".to_vec()),
        ].into_iter().collect();
        (files, m)
    }
    fn locator_map(game: &str) -> serde_json::Value {
        serde_json::json!({ "kind": "memory-map", "contract": LOCATOR_CONTRACT, "locator": { "schema": 1, "id": "cam-locator", "version": "1.0.0", "game": game,
            "module": "FlightSimulator2024.exe", "near": [0.005, 2.0], "fov": [5.0, 150.0], "aspect_tolerance": 0.01 } })
    }

    #[test]
    fn a_camera_only_feature_needs_no_signature_of_ours() {
        let (files, m) = camera_files(locator_map("msfs2024"));
        assert!(camera_only(&m) && !camera_only(&manifest(&self::files())));
        let (inspected, _) = crate::feature::inspect(std::io::Cursor::new(archive(&files))).unwrap();
        assert_eq!(inspected, m);
        let mut a = zip::ZipArchive::new(std::io::Cursor::new(archive(&files))).unwrap();
        assert!(matches!(verify_archive(&mut a, &m).unwrap()[..], [Signed::Locator(_)]));
        // A changed map fails on its pin, in the package and in the installed folder.
        let mut changed = files.clone();
        changed.get_mut(&map_path(&m.memory_maps[0])).unwrap().push(b' ');
        assert!(crate::feature::inspect(std::io::Cursor::new(archive(&changed))).unwrap_err().to_string().contains("digest"));
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in &files {
            std::fs::create_dir_all(dir.path().join(name).parent().unwrap()).unwrap();
            std::fs::write(dir.path().join(name), bytes).unwrap();
        }
        assert_eq!(verify_directory(dir.path(), &m).unwrap().len(), 1);
        std::fs::write(dir.path().join(map_path(&m.memory_maps[0])), &changed[&map_path(&m.memory_maps[0])]).unwrap();
        assert!(verify_directory(dir.path(), &m).is_err(), "the host refuses a changed map");
    }

    // Review round 2: consent compares the named contract, so the package must hold it. A
    // contract that names no pin, or that is not a camera contract, fails the manifest check.
    #[test]
    fn a_named_map_contract_binds_the_map_file() {
        let with = |contracts: serde_json::Value| {
            let (mut files, mut m) = camera_files(locator_map("msfs2024"));
            m.map_contracts = serde_json::from_value(contracts).unwrap();
            files.insert("feature.json".into(), serde_json::to_vec(&m).unwrap());
            crate::feature::inspect(std::io::Cursor::new(archive(&files))).map(|_| ()).map_err(|e| e.to_string())
        };
        with(serde_json::json!({ "cam-locator": LOCATOR_CONTRACT })).unwrap();
        assert!(with(serde_json::json!({ "cam-locator": CAMERA_CONTRACT })).unwrap_err().contains("map contract differs"));
        assert!(with(serde_json::json!({ "cam-other": LOCATOR_CONTRACT })).unwrap_err().contains("not pinned"));
        assert!(with(serde_json::json!({ "cam-locator": "anchored-scalar-v1" })).unwrap_err().contains("not a camera contract"));
    }

    // An older app reads a manifest with map_contracts: Manifest ignores unknown top-level fields,
    // while a pin refuses them (so the contracts are not in the pin).
    #[test]
    fn older_readers_ignore_map_contracts_but_not_a_pin_field() {
        #[derive(serde::Deserialize)]
        #[allow(dead_code)]
        struct Older { id: String, #[serde(default)] memory_maps: Vec<MapDependency> }
        let (_, m) = camera_files(locator_map("msfs2024"));
        let mut value = serde_json::to_value(&m).unwrap();
        value["map_contracts"] = serde_json::json!({ "cam-locator": LOCATOR_CONTRACT });
        assert!(serde_json::from_value::<Older>(value.clone()).is_ok());
        value["memory_maps"][0]["contract"] = serde_json::json!(LOCATOR_CONTRACT);
        assert!(serde_json::from_value::<Older>(value).is_err());
    }

    // Review 2026-10-21: an update from a signed version can still carry the old session files
    // and an unpinned map. Unpack and the host take the pins only: the extra files grant nothing,
    // and a changed pinned map is still refused at unpack.
    #[test]
    fn a_camera_only_unpack_uses_only_its_pinned_maps() {
        let (mut files, m) = camera_files(locator_map("msfs2024"));
        files.insert(SESSION.into(), include_bytes!("../tests/data/memory/memory-session.json").to_vec());
        files.insert(SIGNATURE.into(), include_bytes!("../tests/data/memory/memory-session.json.sig").to_vec());
        files.insert("maps/cam-extra/1.0.0/map.json".into(), serde_json::to_vec(&locator_map("msfs2024")).unwrap());
        let dir = tempfile::tempdir().unwrap();
        let store = crate::Install::feature(dir.path());
        let mut changed = files.clone();
        changed.get_mut(&map_path(&m.memory_maps[0])).unwrap().push(b' ');
        assert!(store.unpack(&m.version, std::io::Cursor::new(archive(&changed)), false).is_err(), "unpack checks the pins");
        store.unpack(&m.version, std::io::Cursor::new(archive(&files)), false).unwrap();
        store.activate(&m.version).unwrap();
        let installed = store.active().unwrap().1;
        std::fs::write(installed.join(crate::PACKAGE_STAMP), "a".repeat(64)).unwrap();
        assert!(matches!(&verify_directory(&installed, &m).unwrap()[..], [Signed::Locator(l)] if l.id == "cam-locator"));
    }

    #[test]
    fn a_camera_only_feature_takes_only_camera_maps_for_its_games() {
        let (files, _) = camera_files(locator_map("ats"));
        assert!(crate::feature::inspect(std::io::Cursor::new(archive(&files))).unwrap_err().to_string().contains("another game"));
        let scalar = serde_json::json!({ "kind": "memory-map", "contract": "anchored-scalar-v1", "map": { "schema": 1, "id": "cam-scalar", "version": "1.0.0",
            "game": "msfs2024", "module": "x.exe", "sha256": "a".repeat(64),
            "symbols": [{ "name": "s", "pattern": "01 02 03 04", "offset": 8, "kind": "f32", "access": "read", "min": 0.0, "max": 1.0 }] } });
        let (files, _) = camera_files(scalar);
        assert!(crate::feature::inspect(std::io::Cursor::new(archive(&files))).unwrap_err().to_string().contains("only camera maps"));
        // A memory feature still needs the signed session: without it, it is refused.
        let mut signed = self::files();
        signed.remove(SIGNATURE);
        assert!(crate::feature::inspect(std::io::Cursor::new(archive(&signed))).is_err());
    }
}
