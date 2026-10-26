// Release packaging tool, never shipped (plan M8). The usage line in main lists the commands.
// "setup" and "catalog" check the signature first (app key, feed key), so no setup exe or feed
// carries a package that the app would refuse. "feature" and "shaders" compile shaders/*.hlsl
// with the pinned compiler. Pipeline parts: "build" packs one commit (build.rs), "review-brief"
// makes the safety brief from a checked verdict (review.rs). "map": memory maps (map.rs).
mod build;
mod map;
mod review;
mod shaders;
use sha2::Digest;
use std::process::ExitCode;

fn main() -> ExitCode {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let result = match a.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["zip", dir, out] => zip(dir, out),
        ["setup", stub, zip, sig, version, out] => setup(stub, zip, sig, version, out),
        ["feature", dir, out, libs @ ..] if !libs.is_empty() => feature(dir, out, libs),
        ["build", repo, commit, dir, out] => build::command(repo, commit, dir, out),
        ["review-brief", dir, verdict] => review::command(dir, verdict),
        ["shaders", dir, out] => shaders::write(std::path::Path::new(dir), std::path::Path::new(out)),
        ["catalog", feed, zip] => catalog(feed, zip),
        ["inspect", zip] => inspect(zip),
        ["withdraw", feed, id, version] => withdraw(feed, id, version),
        ["memory-session", dir, libs @ ..] if !libs.is_empty() => memory_session(dir, libs),
        ["map", rest @ ..] => map::command(rest),
        _ => Err(format!("usage: fm_pack zip <dir> <out.zip> | fm_pack setup <stub> <zip> <sig> <version> <out> | fm_pack feature <dir> <out.zip> <library>... | fm_pack build <repo> <commit> <feature dir> <out dir> | fm_pack review-brief <build dir> <verdict.json> | fm_pack shaders <dir> <out dir> | fm_pack catalog <feed> <zip> (needs <zip>.sig) | fm_pack inspect <zip> | fm_pack withdraw <feed> <id> <version> | {}", map::USAGE)),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fm_pack: {e}");
            ExitCode::FAILURE
        }
    }
}

// Build the exact session record before signing. Packing verifies its signature and hashes.
// The shaders compile here as in `feature` (the pinned dxc gives the same bytes), so the record
// covers the .cso files that the package will hold.
fn memory_session(dir: &str, libs: &[&str]) -> Result<(), String> {
    let root = std::path::Path::new(dir);
    let libraries: Vec<std::path::PathBuf> = libs.iter().map(Into::into).collect();
    let s = fm_install::memory::seal(root, &libraries, &shaders::compile(root)?)?;
    std::fs::write(root.join(fm_install::memory::SESSION), serde_json::to_vec_pretty(&s).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

fn zip(dir: &str, out: &str) -> Result<(), String> {
    let dir = std::path::Path::new(dir);
    if !dir.join(fm_install::app_exe_name()).is_file() {
        return Err(format!("{} has no {}", dir.display(), fm_install::app_exe_name()));
    }
    let mut f = std::fs::File::create(out).map_err(|e| format!("{out}: {e}"))?;
    fm_install::package::pack(dir, &mut f).map_err(|e| e.to_string())
}

fn setup(stub: &str, zip: &str, sig: &str, version: &str, out: &str) -> Result<(), String> {
    let read = |p: &str| std::fs::read(p).map_err(|e| format!("{p}: {e}"));
    let (stub, zip, sig) = (read(stub)?, read(zip)?, String::from_utf8(read(sig)?).map_err(|e| e.to_string())?);
    let key = fm_install::package::app_key();
    fm_install::package::verify(&zip, sig.trim(), key, version)?;
    if fm_install::package::read_payload(&mut std::io::Cursor::new(&stub)).map_err(|e| e.to_string())?.is_some() {
        return Err("the stub already carries a package".into());
    }
    let mut f = std::fs::File::create(out).map_err(|e| format!("{out}: {e}"))?;
    fm_install::package::append_payload(&stub, &zip, sig.trim(), version, &mut f).map_err(|e| e.to_string())
}

/// A feature package from its source folder, its built libraries (.dll, .so) and its compiled
/// shaders. Prints the catalog fields (sha256, size) that the stub feed needs.
fn feature(dir: &str, out: &str, libs: &[&str]) -> Result<(), String> {
    let libs: Vec<std::path::PathBuf> = libs.iter().map(Into::into).collect();
    let (m, sha, size) = pack_feature(std::path::Path::new(dir), std::path::Path::new(out), &libs)?;
    println!("{}", serde_json::json!({ "id": m.id, "version": m.version, "sha256": sha, "size": size }));
    Ok(())
}

/// `feature` without the output line, for `build` too: the manifest, SHA-256 and size of the
/// package that it wrote to `out`.
fn pack_feature(dir: &std::path::Path, out: &std::path::Path, libs: &[std::path::PathBuf]) -> Result<(fm_install::feature::Manifest, String, u64), String> {
    sdk_api(dir)?;
    for lib in libs { library_api(lib)?; }
    let compiled = shaders::compile(dir)?;
    let mut f = std::fs::File::create(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let m = fm_install::feature::pack_shaders(dir, libs, &compiled, &mut f).map_err(|e| e.to_string())?;
    drop(f);
    let bytes = std::fs::read(out).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok((m, sha256_hex(&bytes), bytes.len() as u64))
}

/// The SHA-256 of `bytes` as lowercase hex, as the catalog and the build record write it.
fn sha256_hex(bytes: &[u8]) -> String {
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// The libraries come from this workspace, and its SDK always exports API_VERSION. feature.json
/// must say the same: the installer and the catalog judge compatibility by that field, so a stale
/// "api" lets an old host install a library that it then refuses to load.
fn sdk_api(dir: &std::path::Path) -> Result<(), String> {
    let path = dir.join(fm_install::FEATURE_MANIFEST);
    let m: fm_install::feature::Manifest = serde_json::from_slice(&std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?).map_err(|e| format!("{}: {e}", path.display()))?;
    match m.api == fm_install::feature::FEATURE_API {
        true => Ok(()),
        false => Err(format!("{}: \"api\" is {}, but this SDK builds feature API {}", path.display(), m.api, fm_install::feature::FEATURE_API)),
    }
}

/// The API that a library of this platform exports must be the SDK API too: a stale library from
/// an older build would pass the manifest check. A library of another platform cannot load here;
/// its build uses the same SDK, so the manifest check covers it. The library stays loaded until
/// the packer exits, as in the SDK host: an unload can stop code that the load started.
fn library_api(lib: &std::path::Path) -> Result<(), String> {
    if lib.extension().and_then(|e| e.to_str()) != Some(std::env::consts::DLL_EXTENSION) { return Ok(()); }
    let l: &'static libloading::Library = Box::leak(Box::new(unsafe { libloading::Library::new(lib) }.map_err(|e| format!("{}: {e}", lib.display()))?));
    let f = unsafe { l.get::<fm_feature_sdk::abi::ApiVersionFn>(fm_feature_sdk::abi::SYM_API_VERSION) }.map_err(|_| format!("{}: not a feature library", lib.display()))?;
    match unsafe { f() } {
        v if v == fm_install::feature::FEATURE_API => Ok(()),
        v => Err(format!("{}: built for feature API {v}, this SDK builds {}; build it again", lib.display(), fm_install::feature::FEATURE_API)),
    }
}

/// Puts a feature package on a feed (the stub feed, app/scripts/stub-feed.mjs serve): copies it
/// to <feed>/features/files/<id>-<version>.zip, adds it to features/<id>/versions.json, and sets
/// the catalog entry to the newest version that is not withdrawn. Nothing is deleted, so every
/// version stays installable (version history plan, slice 1). URLs are relative to the catalog.
fn catalog(feed: &str, zip: &str) -> Result<(), String> {
    catalog_with(feed, zip, fm_install::feature::feed_key())
}

/// `catalog` with the key that checks `<zip>.sig` (tests use a throwaway key). The signature
/// goes into the entry; the app refuses a package without it, so the feed never gets one
/// (publishing pipeline plan, L1: signing fails closed).
fn catalog_with(feed: &str, zip: &str, key: &str) -> Result<(), String> {
    use fm_install::feature::{inspect, verify_package, CatalogEntry};
    let bytes = std::fs::read(zip).map_err(|e| format!("{zip}: {e}"))?;
    let (manifest, parts) = inspect(std::io::Cursor::new(&bytes)).map_err(|e| e.to_string())?;
    let sig_file = format!("{zip}.sig");
    let signature = std::fs::read_to_string(&sig_file).map_err(|e| format!("{sig_file}: {e} (sign the package first: app/scripts/sign-feature.mjs)"))?.trim().to_string();
    let dir = std::path::Path::new(feed).join("features");
    let name = format!("{}-{}.zip", manifest.id, manifest.version);
    let sha256 = sha256_hex(&bytes);
    let id = manifest.id.clone();
    let mut history = read_history(&dir, &id)?;
    // A feed made before the history file: its catalog entry is the first known version.
    if history.versions.is_empty() {
        if let Some(old) = read_catalog(&dir)?.features.into_iter().find(|f| f.manifest.id == id) {
            history.add(CatalogEntry { history: None, ..old }, 0)?;
        }
    }
    let published = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let entry = CatalogEntry { manifest, url: format!("files/{name}"), sha256, size: bytes.len() as u64, parts, history: None, signature: Some(signature) };
    verify_package(&bytes, &entry, key).map_err(|e| format!("{sig_file}: {e}"))?;
    // Members who have this version must not get other bytes with the same version.
    let added = history.add(entry, published)?;
    std::fs::create_dir_all(dir.join("files")).map_err(|e| e.to_string())?;
    if added {
        std::fs::write(dir.join("files").join(&name), &bytes).map_err(|e| e.to_string())?;
    }
    println!("catalog: {name} ({} bytes){}", bytes.len(), if added { "" } else { ", already on the feed" });
    write_feed(&dir, &id, &history)
}

/// The id, version and parts of a feature package, after the same checks as `catalog`.
fn inspect(zip: &str) -> Result<(), String> {
    let bytes = std::fs::read(zip).map_err(|e| format!("{zip}: {e}"))?;
    let (m, parts) = fm_install::feature::inspect(std::io::Cursor::new(&bytes)).map_err(|e| e.to_string())?;
    println!("{}", serde_json::json!({ "id": m.id, "version": m.version, "parts": parts }));
    Ok(())
}

/// Withdraws a version: it stays in the history and in files/, but the version list and
/// auto-update skip it, and the catalog entry goes to the newest version that is left.
fn withdraw(feed: &str, id: &str, version: &str) -> Result<(), String> {
    fm_install::feature::check_id(id).map_err(|e| e.to_string())?;
    let dir = std::path::Path::new(feed).join("features");
    let mut history = read_history(&dir, id)?;
    history.withdraw(version)?;
    println!("withdrawn: {id} {version}");
    write_feed(&dir, id, &history)
}

fn read_catalog(dir: &std::path::Path) -> Result<fm_install::feature::Catalog, String> {
    let file = dir.join("catalog.json");
    match std::fs::read(&file) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(e) => Err(format!("{}: {e}", file.display())),
    }
}

fn read_history(dir: &std::path::Path, id: &str) -> Result<fm_install::feature::History, String> {
    let file = dir.join(fm_install::feature::history_path(id));
    // Only a missing file is an empty history: any other read error stops before a write, so
    // a locked or unreadable file never makes the feed forget versions.
    match std::fs::read(&file) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(e) => Err(format!("{}: {e}", file.display())),
    }
}

/// Writes the history first, then the catalog that points to it: a reader never sees a catalog
/// entry whose history file is missing. With every version withdrawn the entry leaves the catalog.
fn write_feed(dir: &std::path::Path, id: &str, history: &fm_install::feature::History) -> Result<(), String> {
    let path = fm_install::feature::history_path(id);
    let file = dir.join(&path);
    std::fs::create_dir_all(file.parent().unwrap_or(dir)).map_err(|e| e.to_string())?;
    std::fs::write(&file, serde_json::to_vec_pretty(history).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let mut cat = read_catalog(dir)?;
    cat.features.retain(|f| f.manifest.id != id);
    if let Some(latest) = history.latest() {
        cat.features.push(fm_install::feature::CatalogEntry { history: Some(path), ..latest.clone() });
    }
    cat.features.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    std::fs::write(dir.join("catalog.json"), serde_json::to_vec_pretty(&cat).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    // Every feature folder of the workspace must pack: a stale "api" fails here, before a release.
    // The SDK repository (tools/sdk-export) has no features/ folder: there this check has nothing to do.
    #[test]
    fn every_workspace_feature_declares_the_sdk_api() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../features");
        if !root.is_dir() { return; }
        let mut seen = 0;
        for dir in std::fs::read_dir(&root).unwrap().map(|e| e.unwrap().path()).filter(|p| p.join(fm_install::FEATURE_MANIFEST).is_file()) {
            if let Err(e) = super::sdk_api(&dir) { panic!("{e}"); }
            seen += 1;
        }
        assert!(seen >= 5, "found {seen} features in {}", root.display());
    }

    // A second package with the same id and version but other bytes is refused; the same bytes
    // again are accepted (a retry), and the feed keeps the first package.
    #[test]
    fn the_feed_refuses_other_content_under_the_same_version() {
        let key = fm_install::test_signing::TestKey::default();
        let root = std::env::temp_dir().join(format!("fm_pack_feed_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let manifest = serde_json::json!({ "id": "feed-test", "name": "Feed test", "version": "1.0.0", "description": "d", "api": fm_install::feature::FEATURE_API, "games": ["msfs2024"], "capabilities": [] });
        std::fs::write(src.join(fm_install::FEATURE_MANIFEST), manifest.to_string()).unwrap();
        // The library of this system (.dll on Windows, .so on Linux), as `inspect` requires.
        let zip = |n: &str, lib: &[u8]| {
            let l = root.join(format!("{n}.{}", std::env::consts::DLL_EXTENSION));
            std::fs::write(&l, lib).unwrap();
            let out = root.join(format!("{n}.zip"));
            fm_install::feature::pack(&src, &[l], &mut std::fs::File::create(&out).unwrap()).unwrap();
            std::fs::write(root.join(format!("{n}.zip.sig")), key.sign(&std::fs::read(&out).unwrap(), "1.0.0")).unwrap();
            out.to_string_lossy().into_owned()
        };
        let (a, b) = (zip("a", b"first"), zip("b", b"second"));
        let feed = root.join("feed").to_string_lossy().into_owned();
        let catalog = |zip: &str| super::catalog_with(&feed, zip, &key.public());
        catalog(&a).unwrap();
        catalog(&a).unwrap();
        assert!(catalog(&b).unwrap_err().contains("new version"));
        let kept = std::fs::read(root.join("feed/features/files/feed-test-1.0.0.zip")).unwrap();
        assert_eq!(kept, std::fs::read(&a).unwrap());
        std::fs::remove_dir_all(&root).unwrap();
    }

    // Publishing pipeline L1: a package without <zip>.sig, or with a signature of another key,
    // does not reach the feed (nothing is written); a signed one goes in with its signature in
    // the catalog and the history. An entry from before signing gets the signature on a re-add.
    #[test]
    fn the_feed_takes_only_signed_packages() {
        use fm_install::feature::{Catalog, History};
        let key = fm_install::test_signing::TestKey::default();
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let manifest = serde_json::json!({ "id": "sig-test", "name": "Signature test", "version": "1.0.0", "description": "d", "api": fm_install::feature::FEATURE_API, "games": ["msfs2024"], "capabilities": [] });
        std::fs::write(src.join(fm_install::FEATURE_MANIFEST), manifest.to_string()).unwrap();
        let lib = t.path().join(format!("l.{}", std::env::consts::DLL_EXTENSION));
        std::fs::write(&lib, b"lib").unwrap();
        let zip = t.path().join("p.zip");
        fm_install::feature::pack(&src, &[lib], &mut std::fs::File::create(&zip).unwrap()).unwrap();
        let (zip_s, sig) = (zip.to_string_lossy().into_owned(), t.path().join("p.zip.sig"));
        let feed = t.path().join("feed").to_string_lossy().into_owned();
        let dir = t.path().join("feed/features");
        assert!(super::catalog_with(&feed, &zip_s, &key.public()).unwrap_err().contains("sign the package first"));
        std::fs::write(&sig, fm_install::test_signing::TestKey::default().sign(&std::fs::read(&zip).unwrap(), "1.0.0")).unwrap();
        assert!(super::catalog_with(&feed, &zip_s, &key.public()).unwrap_err().contains("not valid"));
        assert!(super::catalog(&feed, &zip_s).is_err(), "the feed key did not sign it");
        assert!(!dir.exists(), "nothing written");
        let good = key.sign(&std::fs::read(&zip).unwrap(), "1.0.0");
        std::fs::write(&sig, &good).unwrap();
        super::catalog_with(&feed, &zip_s, &key.public()).unwrap();
        let catalog: Catalog = serde_json::from_slice(&std::fs::read(dir.join("catalog.json")).unwrap()).unwrap();
        assert_eq!(catalog.features[0].signature.as_deref(), Some(good.as_str()));
        // A feed from before signing: drop the signature, then add the same package again.
        let file = dir.join("sig-test/versions.json");
        let mut h: History = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        h.versions[0].entry.signature = None;
        std::fs::write(&file, serde_json::to_vec(&h).unwrap()).unwrap();
        super::catalog_with(&feed, &zip_s, &key.public()).unwrap();
        let h: History = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(h.versions[0].entry.signature.as_deref(), Some(good.as_str()));
    }

    // Every published version stays on the feed (file and history); the catalog shows the
    // newest one that is not withdrawn and points to the history. A withdraw moves the catalog
    // back and keeps the file. A feed made before the history file keeps its old version.
    #[test]
    fn the_feed_keeps_every_version_and_withdraw_moves_the_catalog_back() {
        use fm_install::feature::{Catalog, History};
        let key = fm_install::test_signing::TestKey::default();
        let root = std::env::temp_dir().join(format!("fm_pack_history_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let package = |version: &str| {
            let src = root.join(format!("src-{version}"));
            std::fs::create_dir_all(&src).unwrap();
            let manifest = serde_json::json!({ "id": "hist-test", "name": "History test", "version": version, "description": "d", "api": fm_install::feature::FEATURE_API, "games": ["msfs2024"], "capabilities": [] });
            std::fs::write(src.join(fm_install::FEATURE_MANIFEST), manifest.to_string()).unwrap();
            let l = root.join(format!("{version}.{}", std::env::consts::DLL_EXTENSION));
            std::fs::write(&l, version.as_bytes()).unwrap();
            let out = root.join(format!("{version}.zip"));
            fm_install::feature::pack(&src, &[l], &mut std::fs::File::create(&out).unwrap()).unwrap();
            std::fs::write(root.join(format!("{version}.zip.sig")), key.sign(&std::fs::read(&out).unwrap(), version)).unwrap();
            out.to_string_lossy().into_owned()
        };
        let feed = root.join("feed").to_string_lossy().into_owned();
        let add = |zip: &str| super::catalog_with(&feed, zip, &key.public());
        let dir = root.join("feed/features");
        let catalog = || serde_json::from_slice::<Catalog>(&std::fs::read(dir.join("catalog.json")).unwrap()).unwrap();
        let history = || serde_json::from_slice::<History>(&std::fs::read(dir.join("hist-test/versions.json")).unwrap()).unwrap();
        let listed = |h: &History| h.versions.iter().map(|v| (v.entry.manifest.version.clone(), v.withdrawn)).collect::<Vec<_>>();
        // A feed of the old tool: catalog entry 1.0.0 with no history file.
        add(&package("1.0.0")).unwrap();
        std::fs::remove_dir_all(dir.join("hist-test")).unwrap();
        add(&package("1.1.0")).unwrap();
        add(&package("2.0.0")).unwrap();
        assert_eq!(listed(&history()), [("2.0.0".into(), false), ("1.1.0".into(), false), ("1.0.0".into(), false)]);
        let c = catalog();
        assert_eq!((c.features.len(), c.features[0].manifest.version.as_str(), c.features[0].history.as_deref()), (1, "2.0.0", Some("hist-test/versions.json")));
        for v in ["1.0.0", "1.1.0", "2.0.0"] {
            assert!(dir.join(format!("files/hist-test-{v}.zip")).is_file(), "{v} kept");
        }
        super::withdraw(&feed, "hist-test", "2.0.0").unwrap();
        assert_eq!(catalog().features[0].manifest.version, "1.1.0");
        assert_eq!(listed(&history())[0], ("2.0.0".into(), true));
        assert!(dir.join("files/hist-test-2.0.0.zip").is_file(), "a withdrawn file stays");
        assert!(super::withdraw(&feed, "hist-test", "9.9.9").is_err());
        super::withdraw(&feed, "hist-test", "1.1.0").unwrap();
        super::withdraw(&feed, "hist-test", "1.0.0").unwrap();
        assert!(catalog().features.is_empty(), "all withdrawn: not in the catalog");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
