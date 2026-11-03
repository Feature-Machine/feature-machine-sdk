// Signing for tests only (feature "testing"; never in the app): a throwaway key pair, so tests of
// the feed signature in fm_install, fm_pack and the app sign the packages that they create. The
// output has the Tauri format of `tauri signer sign --app-version`, which `package::verify` reads;
// the fixture test in package.rs proves that format with a real tauri-cli signature.
use base64::Engine;

/// A throwaway signing key.
pub struct TestKey(minisign::KeyPair);

impl Default for TestKey {
    fn default() -> Self {
        Self(minisign::KeyPair::generate_unencrypted_keypair().expect("test key"))
    }
}

impl TestKey {
    /// The public key as the app keeps it: base64 of the minisign public key file.
    pub fn public(&self) -> String {
        b64(&self.0.pk.to_box().expect("public key box").into_string())
    }

    /// A signature of `bytes` for `version`.
    pub fn sign(&self, bytes: &[u8], version: &str) -> String {
        let trusted = format!("timestamp:0\tfile:test.zip\tversion:{version}");
        let sig = minisign::sign(Some(&self.0.pk), &self.0.sk, std::io::Cursor::new(bytes), Some(&trusted), Some("test")).expect("test signature");
        b64(&sig.into_string())
    }
}

fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

/// A camera locator map file (contract camera-locator-v1) with id `cam-locator` for msfs2024:
/// `near` makes each test version tell which file the host read.
pub fn camera_locator(version: &str, near: f32) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "kind": "memory-map", "contract": crate::memory::LOCATOR_CONTRACT, "locator": { "schema": 1, "id": "cam-locator", "version": version, "game": "msfs2024",
        "module": "FlightSimulator2024.exe", "near": [near, 2.0], "fov": [1.0, 150.0], "aspect_tolerance": 0.01 } })).expect("locator json")
}

/// A build-independent setter map file (contract mirrored-setter-v2, the ground wetness shape)
/// with id `map_id` 1.0.0 for msfs2024: one write symbol with limits, a class guard and a setter.
pub fn setter_map(map_id: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "kind": "memory-map", "contract": crate::memory::SETTER_V2_CONTRACT, "map": { "schema": 1, "id": map_id, "version": "1.0.0", "game": "msfs2024",
        "module": "FlightSimulator2024.exe", "symbols": [{ "name": "wet-speed", "pattern": "01 00 00 00 00 00 00 00 47 41 4D 45 2F 57 65 74 00", "offset": 820, "kind": "f32", "access": "write",
        "min": 0, "max": 1, "class_guard": { "offset": -8, "member": "01 00 00 00 00 00 00 00 47 41 4D 45 2F", "members": 100 },
        "setter": { "contract": "mirrored_notify_v1", "mirror": -4, "clamp_min": 8, "clamp_max": 12, "notify_slot": 2 } }] } })).expect("setter json")
}

impl TestKey {
    /// A published setter module `id` 1.0.0 that provides `map_id` (setter_map) with the write
    /// power: its signed catalog entry and package.
    pub fn setter_module_entry(&self, id: &str, map_id: &str) -> (crate::module::CatalogEntry, Vec<u8>) {
        use crate::module::*;
        let map = setter_map(map_id);
        let pin = crate::feature::MapDependency { id: map_id.into(), version: "1.0.0".into(), sha256: crate::memory::digest(&map) };
        let m = ModuleManifest { schema: 1, id: id.into(), name: "Test setter maps".into(), version: "1.0.0".into(), kind: Kind::Data, games: vec![fm_platform::Game::Msfs2024],
            provides: vec![Provided { interface: crate::memory::SETTER_V2_CONTRACT.into(), version: "1.0.0".into(), map: pin.clone() }], powers: vec![Power::GameMemoryWrite],
            license: LICENSE.into(), source: Source { repository: "https://example.invalid/modules".into(), commit: "a".repeat(40) }, min_app: None };
        let mut out = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut out);
        for (name, data) in [(MANIFEST.to_string(), serde_json::to_vec(&m).expect("manifest")), (crate::memory::map_path(&pin), map)] {
            z.start_file(name, zip::write::SimpleFileOptions::default()).expect("zip");
            std::io::Write::write_all(&mut z, &data).expect("zip");
        }
        z.finish().expect("zip");
        let bytes = out.into_inner();
        let entry = CatalogEntry { manifest: m, url: format!("files/{id}-1.0.0.zip"), sha256: crate::memory::digest(&bytes), size: bytes.len() as u64, signature: self.sign(&bytes, "1.0.0"), yanked: false, channel: Default::default() };
        (entry, bytes)
    }

    /// A module writer `m` as a catalog install leaves it in `dir`: feature.json and a library
    /// from one package signed with this key, the stamp, and the kept feed package (Sol r1 B1).
    /// Returns the stamp (the package digest).
    pub fn feed_install(&self, dir: &std::path::Path, m: &crate::feature::Manifest) -> String {
        let files = [(crate::FEATURE_MANIFEST.to_string(), serde_json::to_vec(m).expect("manifest")), (crate::feature::parts::library(), b"library".to_vec())];
        let mut out = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut out);
        for (name, data) in &files {
            z.start_file(name.as_str(), zip::write::SimpleFileOptions::default()).expect("zip");
            std::io::Write::write_all(&mut z, data).expect("zip");
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("folder");
            std::fs::write(path, data).expect("file");
        }
        z.finish().expect("zip");
        let bytes = out.into_inner();
        let stamp = crate::memory::digest(&bytes);
        std::fs::write(dir.join(crate::PACKAGE_STAMP), &stamp).expect("stamp");
        crate::memory::keep_feed_package(dir, &bytes, &self.sign(&bytes, &m.version)).expect("feed package");
        stamp
    }

    /// A published data module `id` at `version` that provides cam-locator (map version =
    /// module version): its signed catalog entry (url files/<id>-<version>.zip) and package.
    pub fn module_entry(&self, id: &str, version: &str, near: f32) -> (crate::module::CatalogEntry, Vec<u8>) {
        use crate::module::*;
        let map = camera_locator(version, near);
        let pin = crate::feature::MapDependency { id: "cam-locator".into(), version: version.into(), sha256: crate::memory::digest(&map) };
        let m = ModuleManifest { schema: 1, id: id.into(), name: "Test camera maps".into(), version: version.into(), kind: Kind::Data, games: vec![fm_platform::Game::Msfs2024],
            provides: vec![Provided { interface: crate::memory::LOCATOR_CONTRACT.into(), version: "1.0.0".into(), map: pin.clone() }], powers: vec![Power::GameMemoryRead],
            license: LICENSE.into(), source: Source { repository: "https://example.invalid/modules".into(), commit: "a".repeat(40) }, min_app: None };
        let mut out = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut out);
        for (name, data) in [(MANIFEST.to_string(), serde_json::to_vec(&m).expect("manifest")), (crate::memory::map_path(&pin), map)] {
            z.start_file(name, zip::write::SimpleFileOptions::default()).expect("zip");
            std::io::Write::write_all(&mut z, &data).expect("zip");
        }
        z.finish().expect("zip");
        let bytes = out.into_inner();
        let entry = CatalogEntry { manifest: m, url: format!("files/{id}-{version}.zip"), sha256: crate::memory::digest(&bytes), size: bytes.len() as u64, signature: self.sign(&bytes, version), yanked: false, channel: Default::default() };
        (entry, bytes)
    }

    /// The catalog bytes and their signature for its revision (as fm_pack module-publish signs).
    pub fn sign_catalog(&self, c: &crate::module::Catalog) -> (Vec<u8>, String) {
        let bytes = serde_json::to_vec_pretty(c).expect("catalog json");
        let sig = self.sign(&bytes, &c.revision.to_string());
        (bytes, sig)
    }
}
