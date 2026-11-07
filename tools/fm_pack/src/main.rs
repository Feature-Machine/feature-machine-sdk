// Release packaging tool, never shipped (plan M8); the usage line in main lists the commands.
// "setup" and "catalog" check the signature first, so no setup exe or feed carries a package
// that the app would refuse. "feature" and "shaders" compile shaders/*.hlsl with the pinned
// compiler. Review pipeline: build or prepare + build-prepared (build.rs), deps, review-run,
// review-brief (review.rs); "repack" adds the signed library; "map" writes memory maps.
mod build;
mod deps;
mod feed;
mod map;
mod module;
mod notice;
mod policy;
mod repack;
mod review;
mod review_eval;
mod review_run;
mod shaders;
use sha2::Digest;
use std::process::ExitCode;

fn main() -> ExitCode {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = a.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["zip", dir, out] => zip(dir, out),
        ["setup", stub, zip, sig, version, out] => setup(stub, zip, sig, version, out),
        ["setup-check", setup, zip, version] => setup_check(setup, zip, version),
        ["feature", dir, out, libs @ ..] if !libs.is_empty() => feature(dir, out, libs),
        ["build", repo, commit, dir, out] => build::command(repo, commit, dir, out),
        ["prepare", repo, commit, dir, out] => build::prepare(repo, commit, dir, out),
        ["build-prepared", dir] => build::build_prepared(dir),
        ["review-run", dir, repository] => review_run::command(dir, repository),
        ["review-eval", rest @ ..] => review_eval::command(rest),
        ["deps", dir] => deps::command(dir),
        ["review-brief", dir, verdict] => review::command(dir, verdict),
        ["repack", zip, lib, out] => repack::command(zip, lib, out),
        ["shaders", dir, out] => shaders::write(std::path::Path::new(dir), std::path::Path::new(out)),
        ["dxc"] => shaders::compiler().map(|p| println!("{}", p.display())),
        ["catalog", feed, zip] => catalog(feed, zip, fm_install::release::Channel::Stable),
        ["catalog", feed, zip, "preview"] => catalog(feed, zip, fm_install::release::Channel::Preview),
        ["inspect", zip] => inspect(zip),
        ["feed-init", feed] => feed::init(feed).and_then(|()| module::init(feed)),
        // A module id has a dot; a feature id cannot (check_id), so one command serves both.
        ["promote", feed, id, version] if id.contains('.') => module::command(&["module-promote", feed, id, version]),
        ["promote", feed, id, version] => feed::promote(feed, id, version),
        ["withdraw", feed, id, version] if id.contains('.') => module::command(&["module-yank", feed, id, version]),
        ["withdraw", feed, id, version] => feed::withdraw(feed, id, version),
        ["memory-session", dir, libs @ ..] if !libs.is_empty() => memory_session(dir, libs),
        ["map", rest @ ..] => map::command(rest),
        [cmd, ..] if cmd.starts_with("module") => module::command(&args),
        [cmd, ..] if cmd.starts_with("policy") => policy::command(&args),
        [cmd, ..] if cmd.starts_with("notice") => notice::command(&args),
        _ => Err(format!("usage: fm_pack zip <dir> <out.zip> | fm_pack setup <stub> <zip> <sig> <version> <out> | fm_pack setup-check <setup> <zip> <version> | fm_pack feature <dir> <out.zip> <library>... | fm_pack build <repo> <commit> <feature dir> <out dir> | fm_pack prepare <repo> <commit> <feature dir> <out dir> | fm_pack build-prepared <out dir of prepare> | fm_pack review-run <out dir of prepare> <owner/repository> (OPENROUTER_API_KEY, FM_REVIEW_MODEL) | {} | fm_pack deps <build dir> | fm_pack review-brief <build dir> <verdict.json> | fm_pack repack <zip> <signed feature.dll> <out.zip> | fm_pack shaders <dir> <out dir> | fm_pack dxc (path of the pinned compiler) | fm_pack catalog <feed> <zip> [preview] (needs <zip>.sig) | fm_pack inspect <zip> | fm_pack feed-init <feed> | fm_pack promote <feed> <id> <version> | fm_pack withdraw <feed> <id> <version> (a module: yank) | {} | {} | {} | {}", review_eval::USAGE, map::USAGE, module::USAGE, policy::USAGE, notice::USAGE)),
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

/// The payload of a setup exe (also after Authenticode signing) is exactly `zip` for `version`,
/// with a valid app signature: what the setup installs. package.mjs runs it after the signing.
fn setup_check(setup: &str, zip: &str, version: &str) -> Result<(), String> {
    let mut f = std::fs::File::open(setup).map_err(|e| format!("{setup}: {e}"))?;
    let p = fm_install::package::read_payload(&mut f).map_err(|e| format!("{setup}: {e}"))?.ok_or_else(|| format!("{setup}: no package in the setup"))?;
    if p.zip != std::fs::read(zip).map_err(|e| format!("{zip}: {e}"))? {
        return Err(format!("{setup}: the package in the setup is not {zip}"));
    }
    fm_install::package::verify(&p.zip, &p.signature, fm_install::package::app_key(), version)?;
    println!("setup payload: {version}, {} bytes, app signature valid", p.zip.len());
    Ok(())
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
/// A fake HTTP server for the tests: it answers each request with the next status and JSON body
/// and keeps the request head (request line and headers) and the JSON body.
#[cfg(test)]
pub(crate) mod test_http {
    use std::io::{BufRead, Read, Write};
    pub fn serve(answers: Vec<(u16, serde_json::Value)>) -> (String, std::thread::JoinHandle<Vec<(String, serde_json::Value)>>) {
        serve_raw(answers.into_iter().map(|(s, a)| (s, String::new(), a.to_string())).collect())
    }

    /// As `serve`, with each answer as (status, extra header lines ending in \r\n, raw body), for
    /// answers that are not JSON (a gateway page) or that carry headers (Retry-After).
    pub fn serve_raw(answers: Vec<(u16, String, String)>) -> (String, std::thread::JoinHandle<Vec<(String, serde_json::Value)>>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let mut seen = vec![];
            for (status, extra, out) in answers {
                let (s, _) = l.accept().unwrap();
                let mut r = std::io::BufReader::new(s.try_clone().unwrap());
                let (mut len, mut head) = (0, String::new());
                loop {
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") { len = v.trim().parse().unwrap(); }
                    if line == "\r\n" { break; }
                    head += &line;
                }
                let mut body = vec![0; len];
                r.read_exact(&mut body).unwrap();
                seen.push((head, serde_json::from_slice(&body).unwrap()));
                write!(&s, "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{out}", out.len()).unwrap();
            }
            seen
        });
        (url, h)
    }
}

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
/// to <feed>/features/files/<id>-<version>.zip and adds it on `channel` to the v2 history, from
/// which feed.rs writes the catalogs and the legacy history. Nothing is deleted, so every version
/// stays installable (version history plan, slice 1). A Preview min_app needs the pre-release
/// form: 0.2.0-preview.1 < 0.2.0 (semver precedence), so "0.2.0" excludes the 0.2.0 Previews.
fn catalog(feed: &str, zip: &str, channel: fm_install::release::Channel) -> Result<(), String> {
    catalog_with(feed, zip, crate::feed::key(), channel)
}

/// `catalog` with the key that checks `<zip>.sig` (tests use a throwaway key). The signature
/// goes into the entry; the app refuses a package without it, so the feed never gets one
/// (publishing pipeline plan, L1: signing fails closed).
fn catalog_with(feed: &str, zip: &str, key: &str, channel: fm_install::release::Channel) -> Result<(), String> {
    use fm_install::feature::{inspect, verify_package, CatalogEntry};
    let bytes = std::fs::read(zip).map_err(|e| format!("{zip}: {e}"))?;
    let (manifest, parts) = inspect(std::io::Cursor::new(&bytes)).map_err(|e| e.to_string())?;
    let sig_file = format!("{zip}.sig");
    let signature = std::fs::read_to_string(&sig_file).map_err(|e| format!("{sig_file}: {e} (sign the package first: app/scripts/sign-feature.mjs)"))?.trim().to_string();
    let name = format!("{}-{}.zip", manifest.id, manifest.version);
    let published = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let art = catalog_art(&bytes, &manifest)?;
    let mut entry = CatalogEntry { manifest, url: name.clone(), sha256: sha256_hex(&bytes), size: bytes.len() as u64, parts, history: None, signature: Some(signature), channel, art: None };
    verify_package(&bytes, &entry, key).map_err(|e| format!("{sig_file}: {e}"))?;
    // Members who have this version must not get other bytes with the same version (feed::add).
    // The files go first, so no view names a file that is not there. An existing file (also
    // one that a stopped run left with no entry) must have these bytes: the entry gets their hash.
    let files = feed::legacy_dir(feed).join("files");
    std::fs::create_dir_all(&files).map_err(|e| e.to_string())?;
    put_file(&files, &name, &bytes)?;
    // The art (surfaces plan, section 1): next to the package, named by feature and version.
    let mut copied = fm_install::feature::CatalogArt::default();
    for (kind, file, slot) in [("icon", &art.0, &mut copied.icon), ("banner", &art.1, &mut copied.banner)] {
        let Some((ext, data)) = file else { continue };
        let art_name = format!("{}-{}-{kind}.{ext}", entry.manifest.id, entry.manifest.version);
        put_file(&files, &art_name, data)?;
        *slot = Some(fm_install::feature::ArtFile { url: art_name, sha256: sha256_hex(data) });
    }
    entry.art = (copied != Default::default()).then_some(copied);
    let added = feed::add(feed, entry, channel, published)?;
    println!("catalog: {name} ({} bytes, {channel:?}){}", bytes.len(), if added { "" } else { ", already on the feed" });
    Ok(())
}

/// Writes `bytes` as `name` in the feed folder `files`, or checks that the file there has them:
/// a published name never gets other bytes.
fn put_file(files: &std::path::Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    match std::fs::read(files.join(name)) {
        Ok(old) if old == bytes => Ok(()),
        Ok(_) => Err(format!("{name} is on the feed with other bytes; a changed feature needs a new version")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::write(files.join(name), bytes).map_err(|e| e.to_string()),
        Err(e) => Err(format!("{name}: {e}")),
    }
}

/// The icon and the banner of a checked package (inspect passed): (extension, bytes) of each.
type Art = (Option<(String, Vec<u8>)>, Option<(String, Vec<u8>)>);
fn catalog_art(zip: &[u8], m: &fm_install::feature::Manifest) -> Result<Art, String> {
    use std::io::Read;
    let mut a = zip::ZipArchive::new(std::io::Cursor::new(zip)).map_err(|e| e.to_string())?;
    let mut read = |path: &Option<String>| -> Result<Option<(String, Vec<u8>)>, String> {
        let Some(path) = path else { return Ok(None) };
        let ext = path.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default();
        let mut out = vec![];
        a.by_name(path).map_err(|e| format!("{path}: {e}"))?.take(fm_install::surfaces::limits::BANNER + 1).read_to_end(&mut out).map_err(|e| e.to_string())?;
        Ok(Some((ext, out)))
    };
    Ok((read(&m.surfaces.icon)?, read(&m.surfaces.banner)?))
}

/// The id, version and parts of a feature package, after the same checks as `catalog`. A module
/// package or a module catalog gives the version that its signature is for (module.rs).
fn inspect(zip: &str) -> Result<(), String> {
    let bytes = std::fs::read(zip).map_err(|e| format!("{zip}: {e}"))?;
    if let Some(v) = policy::inspect(&bytes).or_else(|| notice::inspect(&bytes)).or_else(|| module::inspect(&bytes)) {
        println!("{v}");
        return Ok(());
    }
    let (m, parts) = fm_install::feature::inspect(std::io::Cursor::new(&bytes)).map_err(|e| e.to_string())?;
    println!("{}", serde_json::json!({ "id": m.id, "version": m.version, "parts": parts }));
    Ok(())
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
        let catalog = |zip: &str| super::catalog_with(&feed, zip, &key.public(), fm_install::release::Channel::Stable);
        catalog(&a).unwrap();
        catalog(&a).unwrap();
        assert!(catalog(&b).unwrap_err().contains("new version"));
        let kept = std::fs::read(root.join("feed/features/files/feed-test-1.0.0.zip")).unwrap();
        assert_eq!(kept, std::fs::read(&a).unwrap());
        std::fs::remove_dir_all(&root).unwrap();
    }

    // Surfaces plan section 1: the catalog step copies the icon and the banner next to the
    // package, with their digests in the entry; the v2 view points to the shared files folder.
    #[test]
    fn the_catalog_copies_the_art_of_the_package() {
        use fm_install::feature::Catalog;
        let key = fm_install::test_signing::TestKey::default();
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        std::fs::create_dir_all(src.join("art")).unwrap();
        std::fs::create_dir_all(src.join("ui")).unwrap();
        let manifest = serde_json::json!({ "id": "art-test", "name": "Art test", "version": "1.0.0", "description": "d", "api": fm_install::feature::FEATURE_API, "games": ["msfs2024"],
            "capabilities": ["ui_app"], "min_app": fm_install::surfaces::MIN_APP, "page": { "entry": "index.html" }, "icon": "art/icon.svg" });
        std::fs::write(src.join(fm_install::FEATURE_MANIFEST), manifest.to_string()).unwrap();
        std::fs::write(src.join("ui/index.html"), "<p>").unwrap();
        let svg = b"<svg xmlns='http://www.w3.org/2000/svg'/>";
        std::fs::write(src.join("art/icon.svg"), svg).unwrap();
        let lib = t.path().join(format!("l.{}", std::env::consts::DLL_EXTENSION));
        std::fs::write(&lib, b"lib").unwrap();
        let zip = t.path().join("p.zip");
        fm_install::feature::pack(&src, &[lib], &mut std::fs::File::create(&zip).unwrap()).unwrap();
        std::fs::write(t.path().join("p.zip.sig"), key.sign(&std::fs::read(&zip).unwrap(), "1.0.0")).unwrap();
        let feed = t.path().join("feed").to_string_lossy().into_owned();
        super::catalog_with(&feed, &zip.to_string_lossy(), &key.public(), fm_install::release::Channel::Stable).unwrap();
        assert_eq!(std::fs::read(t.path().join("feed/features/files/art-test-1.0.0-icon.svg")).unwrap(), svg);
        let v2: Catalog = serde_json::from_slice(&std::fs::read(t.path().join("feed/v2/features/catalog.json")).unwrap()).unwrap();
        let icon = v2.features[0].art.as_ref().and_then(|a| a.icon.clone()).unwrap();
        assert_eq!(icon.url, "../../features/files/art-test-1.0.0-icon.svg");
        assert_eq!(icon.sha256, super::sha256_hex(svg));
        assert!(v2.features[0].art.as_ref().unwrap().banner.is_none());
        let legacy: Catalog = serde_json::from_slice(&std::fs::read(t.path().join("feed/features/catalog.json")).unwrap_or_else(|_| b"{\"features\":[]}".to_vec())).unwrap();
        assert!(legacy.features.is_empty(), "min_app 0.2.0: not for apps 0.1.x");
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
        // LEGACY_API: the test reads the legacy view (features/), which takes no API 6 entry.
        let manifest = serde_json::json!({ "id": "sig-test", "name": "Signature test", "version": "1.0.0", "description": "d", "api": super::feed::LEGACY_API, "games": ["msfs2024"], "capabilities": [] });
        std::fs::write(src.join(fm_install::FEATURE_MANIFEST), manifest.to_string()).unwrap();
        let lib = t.path().join(format!("l.{}", std::env::consts::DLL_EXTENSION));
        std::fs::write(&lib, b"lib").unwrap();
        let zip = t.path().join("p.zip");
        fm_install::feature::pack(&src, &[lib], &mut std::fs::File::create(&zip).unwrap()).unwrap();
        let (zip_s, sig) = (zip.to_string_lossy().into_owned(), t.path().join("p.zip.sig"));
        let feed = t.path().join("feed").to_string_lossy().into_owned();
        let dir = t.path().join("feed/features");
        assert!(super::catalog_with(&feed, &zip_s, &key.public(), fm_install::release::Channel::Stable).unwrap_err().contains("sign the package first"));
        std::fs::write(&sig, fm_install::test_signing::TestKey::default().sign(&std::fs::read(&zip).unwrap(), "1.0.0")).unwrap();
        assert!(super::catalog_with(&feed, &zip_s, &key.public(), fm_install::release::Channel::Stable).unwrap_err().contains("not valid"));
        assert!(super::catalog(&feed, &zip_s, fm_install::release::Channel::Stable).is_err(), "the feed key did not sign it");
        assert!(!dir.exists(), "nothing written");
        let good = key.sign(&std::fs::read(&zip).unwrap(), "1.0.0");
        std::fs::write(&sig, &good).unwrap();
        super::catalog_with(&feed, &zip_s, &key.public(), fm_install::release::Channel::Stable).unwrap();
        let catalog: Catalog = serde_json::from_slice(&std::fs::read(dir.join("catalog.json")).unwrap()).unwrap();
        assert_eq!(catalog.features[0].signature.as_deref(), Some(good.as_str()));
        // A feed from before signing: drop the signature, then add the same package again.
        // The v2 history is the source; the legacy history is written from it.
        let file = t.path().join("feed/v2/features/sig-test/versions.json");
        let mut h: History = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        h.versions[0].entry.signature = None;
        std::fs::write(&file, serde_json::to_vec(&h).unwrap()).unwrap();
        super::catalog_with(&feed, &zip_s, &key.public(), fm_install::release::Channel::Stable).unwrap();
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
            // LEGACY_API: this test reads the legacy view (features/) too.
            let manifest = serde_json::json!({ "id": "hist-test", "name": "History test", "version": version, "description": "d", "api": super::feed::LEGACY_API, "games": ["msfs2024"], "capabilities": [] });
            std::fs::write(src.join(fm_install::FEATURE_MANIFEST), manifest.to_string()).unwrap();
            let l = root.join(format!("{version}.{}", std::env::consts::DLL_EXTENSION));
            std::fs::write(&l, version.as_bytes()).unwrap();
            let out = root.join(format!("{version}.zip"));
            fm_install::feature::pack(&src, &[l], &mut std::fs::File::create(&out).unwrap()).unwrap();
            std::fs::write(root.join(format!("{version}.zip.sig")), key.sign(&std::fs::read(&out).unwrap(), version)).unwrap();
            out.to_string_lossy().into_owned()
        };
        let feed = root.join("feed").to_string_lossy().into_owned();
        let add = |zip: &str| super::catalog_with(&feed, zip, &key.public(), fm_install::release::Channel::Stable);
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
        super::feed::withdraw(&feed, "hist-test", "2.0.0").unwrap();
        assert_eq!(catalog().features[0].manifest.version, "1.1.0");
        assert_eq!(listed(&history())[0], ("2.0.0".into(), true));
        assert!(dir.join("files/hist-test-2.0.0.zip").is_file(), "a withdrawn file stays");
        assert!(super::feed::withdraw(&feed, "hist-test", "9.9.9").is_err());
        super::feed::withdraw(&feed, "hist-test", "1.1.0").unwrap();
        super::feed::withdraw(&feed, "hist-test", "1.0.0").unwrap();
        assert!(catalog().features.is_empty(), "all withdrawn: not in the catalog");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
