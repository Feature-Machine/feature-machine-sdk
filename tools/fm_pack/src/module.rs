// Module packages and the module catalog on a feed (module layer plan 8.1, signing path). The
// rules are fm_install::module (the app and the host use the same code). Each change of the
// catalog raises its revision and deletes the old catalog signature, so a feed never serves a
// signature of other bytes; sign the catalog again (app/scripts/sign-feature.mjs) before upload.
// Release channels (release-admin slice 8): the v2 catalog holds both channels and is the source;
// the legacy catalog for apps 0.1.x gets its Stable entries that fit LEGACY_APP.
use fm_install::module::{self, Catalog, CatalogEntry, Revoked};
use fm_install::release::{fits_app, Channel};
use std::path::Path;

pub const USAGE: &str = "fm_pack module <dir> <out.zip> | fm_pack module-verify <feed> | fm_pack module-verify-v2 <feed> | fm_pack module-publish <feed> <zip> [preview] (needs <zip>.sig) | fm_pack module-promote <feed> <id> <version> | fm_pack module-yank <feed> <id> <version> | fm_pack module-revoke <feed> <id> <version> <reason>";

/// The v2 catalog: schema 2 with the signed field "view": "v2". So the legacy catalog can never
/// be replayed at the v2 path, nor this one at the legacy path (apps 0.1.x take schema 1 only).
pub const V3_CATALOG: &str = "v3/modules/catalog.json";
pub const V3_CATALOG_SIG: &str = "v3/modules/catalog.json.sig";
pub const V2_CATALOG: &str = "v2/modules/catalog.json";
pub const V2_CATALOG_SIG: &str = "v2/modules/catalog.json.sig";
const V2_SCHEMA: u32 = 2;

#[derive(serde::Serialize, serde::Deserialize)]
struct V2 {
    #[serde(flatten)]
    c: Catalog,
    view: String,
}

/// How long a catalog gives new selections. An expired one never stops a lock that works (plan
/// 7.3), but it stops new installs until a new catalog is published. One year, because no job
/// renews it yet (production feed plan F8); the TUF timestamp role gives freshness later.
const VALID_SECONDS: u64 = 365 * 24 * 3600;

pub fn command(args: &[&str]) -> Result<(), String> {
    match args {
        ["module", dir, out] => pack(dir, out),
        ["module-verify", feed] => verify(feed, crate::feed::key()),
        ["module-verify-v2", feed] => verify_v2(feed, crate::feed::key()),
        ["module-verify-v3", feed] => verify_v3(feed, crate::feed::key()),
        ["module-publish", feed, zip] => publish(feed, zip, crate::feed::key(), Channel::Stable),
        ["module-publish", feed, zip, "preview"] => publish(feed, zip, crate::feed::key(), Channel::Preview),
        ["module-promote", feed, id, version] => change(feed, |c| promote(c, id, version)),
        ["module-yank", feed, id, version] => change(feed, |c| set_yanked(c, id, version)),
        ["module-revoke", feed, id, version, reason] => change(feed, |c| revoke(c, id, version, reason)),
        _ => Err(format!("usage: {USAGE}")),
    }
}

/// Checks the module source and writes its package (module::pack; nothing on a failure).
fn pack(dir: &str, out: &str) -> Result<(), String> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let m = module::pack(Path::new(dir), &mut buf)?;
    std::fs::write(out, buf.into_inner()).map_err(|e| format!("{out}: {e}"))?;
    println!("module: {} {} -> {out}", m.id, m.version);
    Ok(())
}

fn catalog_file(feed: &str) -> std::path::PathBuf {
    Path::new(feed).join(module::CATALOG)
}

/// Checks the signature of the feed's module catalog with `key` (the app check). The publish job
/// runs it on the live catalog before a change: a new signature must never extend the trust of
/// a catalog that the key did not sign (publish job review r1).
fn verify(feed: &str, key: &str) -> Result<(), String> {
    let (file, sig) = (catalog_file(feed), Path::new(feed).join(module::CATALOG_SIG));
    let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let signature = std::fs::read_to_string(&sig).map_err(|e| format!("{}: {e}", sig.display()))?;
    let c = Catalog::verify(&bytes, signature.trim(), key)?;
    // fm_install accepts both views now (item 7); the legacy path takes the legacy view only.
    if c.view.is_some() {
        return Err("module catalog: a v2 catalog at the legacy path".into());
    }
    println!("module catalog: revision {} signed by the feed key", c.revision);
    Ok(())
}

/// The feed's module catalog, or a new empty one. Any read error other than a missing file stops
/// before a write, so a locked file never makes the feed forget a version or a revocation.
fn read(feed: &str) -> Result<Catalog, String> {
    let file = catalog_file(feed);
    match std::fs::read(&file) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Catalog { schema: 1, revision: 0, issued: 0, expires: 0, modules: vec![], revoked: vec![], view: None }),
        Err(e) => Err(format!("{}: {e}", file.display())),
    }
}

/// Checks the v2 catalog pair like `verify`, and that it is the v2 view (schema 2, view v2).
fn verify_v2(feed: &str, key: &str) -> Result<(), String> {
    let (file, sig) = (Path::new(feed).join(V2_CATALOG), Path::new(feed).join(V2_CATALOG_SIG));
    let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let signature = std::fs::read_to_string(&sig).map_err(|e| format!("{}: {e}", sig.display()))?;
    let v: V2 = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", file.display()))?;
    fm_install::package::verify(&bytes, signature.trim(), key, &v.c.revision.to_string()).map_err(|e| format!("v2 module catalog: {e}"))?;
    if v.c.schema != V2_SCHEMA || v.view != "v2" {
        return Err("v2 module catalog: not schema 2 with view v2".into());
    }
    println!("v2 module catalog: revision {} signed by the feed key", v.c.revision);
    Ok(())
}

fn verify_v3(feed: &str, key: &str) -> Result<(), String> {
    let bytes = std::fs::read(Path::new(feed).join(V3_CATALOG)).map_err(|e| e.to_string())?;
    let signature = std::fs::read_to_string(Path::new(feed).join(V3_CATALOG_SIG)).map_err(|e| e.to_string())?;
    let c = Catalog::verify(&bytes, signature.trim(), key)?;
    if c.view.as_deref() != Some("v3") { return Err("v3 module catalog: not schema 3 with view v3".into()); }
    Ok(())
}

fn read_v3(feed: &str) -> Result<Catalog, String> {
    let file = Path::new(feed).join(V3_CATALOG);
    match std::fs::read(&file) {
        Ok(bytes) => {
            let c: Catalog = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            if c.schema != 3 || c.view.as_deref() != Some("v3") { return Err("not schema 3 with view v3".into()); }
            Ok(c)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Catalog { schema:3, view:Some("v3".into()), revision:0, ..read_v2(feed)? }),
        Err(e) => Err(format!("{}: {e}", file.display())),
    }
}

/// The v2 catalog, or one made from the legacy catalog (all Stable) when the feed has none yet.
fn read_v2(feed: &str) -> Result<Catalog, String> {
    let file = Path::new(feed).join(V2_CATALOG);
    match std::fs::read(&file) {
        Ok(b) => {
            let v: V2 = serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", file.display()))?;
            if v.c.schema != V2_SCHEMA || v.view != "v2" {
                return Err(format!("{}: not schema 2 with view v2", file.display()));
            }
            Ok(v.c)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut c = read(feed)?;
            for e in &mut c.modules {
                e.url = format!("../../modules/files/{}", crate::feed::file_name(&e.url));
            }
            Ok(Catalog { schema: V2_SCHEMA, revision: 0, ..c })
        }
        Err(e) => Err(format!("{}: {e}", file.display())),
    }
}

/// Makes the v2 catalog from the legacy one when the feed has none, else derives the legacy
/// catalog from the v2 one again (feed-init). A run that stopped after the v2 upload left a
/// stale legacy catalog (a revocation missing); the next run of any kind repairs it (code
/// review r1). A current legacy catalog keeps its bytes.
pub fn init(feed: &str) -> Result<(), String> {
    if Path::new(feed).join(V3_CATALOG).exists() { derive_older(feed, &read_v3(feed)?) }
    else if Path::new(feed).join(V2_CATALOG).exists() || catalog_file(feed).exists() { change(feed, |_| Ok(())) }
    else { Ok(()) }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn write_catalog(feed: &str, file: &str, sig: &str, value: &impl serde::Serialize) -> Result<(), String> {
    let path = Path::new(feed).join(file);
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new(feed))).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(Path::new(feed).join(sig));
    std::fs::write(&path, serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// Applies `edit` to the v2 catalog, raises its revision, sets issued and expires, writes it and
/// deletes its old signature. The legacy catalog then gets the Stable entries that fit
/// LEGACY_APP and every revocation. It changes (and loses its signature) only when that content
/// changes, so a Preview module never gives old apps a new catalog.
fn change(feed: &str, edit: impl FnOnce(&mut Catalog) -> Result<(), String>) -> Result<(), String> {
    let mut c = read_v3(feed)?;
    edit(&mut c)?;
    let now = now();
    (c.revision, c.issued, c.expires) = (c.revision + 1, now, now + VALID_SECONDS);
    write_catalog(feed, V3_CATALOG, V3_CATALOG_SIG, &c)?;
    println!("v3 module catalog: revision {} (sign it)", c.revision);
    derive_older(feed, &c)
}

// Both old catalogs must stay parseable before their app-version filters run. Keep every
// revocation, but exclude neutral manifests and targets unknown to their closed enum.
fn compatible(manifest: &module::ModuleManifest) -> bool {
    manifest.schema == 1 && manifest.games.iter().all(|id| matches!(*id, fm_platform::ApplicationId::ATS | fm_platform::ApplicationId::MSFS2024))
}

fn derive_older(feed: &str, source: &Catalog) -> Result<(), String> {
    let mut v2 = read_v2(feed)?;
    let modules: Vec<_> = source.modules.iter().filter(|entry| compatible(&entry.manifest)).cloned().collect();
    if v2.modules != modules || v2.revoked != source.revoked || !Path::new(feed).join(V2_CATALOG).exists() {
        v2.modules = modules;
        v2.revoked = source.revoked.clone();
        (v2.schema, v2.view) = (2, Some("v2".into()));
        (v2.revision, v2.issued, v2.expires) = (v2.revision + 1, now(), source.expires);
        write_catalog(feed, V2_CATALOG, V2_CATALOG_SIG, &v2)?;
    }
    derive_legacy(feed, &v2)
}

/// The legacy catalog from the v2 one: Stable entries that fit LEGACY_APP and every revocation.
/// Written (new revision, no signature) only when that content differs.
fn derive_legacy(feed: &str, c: &Catalog) -> Result<(), String> {
    let now = now();
    let mut legacy = read(feed)?;
    let fits = |e: &&CatalogEntry| compatible(&e.manifest) && e.channel == Channel::Stable && fits_app(e.manifest.min_app.as_deref(), crate::feed::LEGACY_APP);
    let modules: Vec<CatalogEntry> = c.modules.iter().filter(fits).map(|e| CatalogEntry { url: format!("files/{}", crate::feed::file_name(&e.url)), ..e.clone() }).collect();
    if legacy.modules != modules || legacy.revoked != c.revoked || !catalog_file(feed).exists() {
        (legacy.modules, legacy.revoked) = (modules, c.revoked.clone());
        (legacy.revision, legacy.issued, legacy.expires) = (legacy.revision + 1, now, now + VALID_SECONDS);
        write_catalog(feed, module::CATALOG, module::CATALOG_SIG, &legacy)?;
        println!("module catalog: revision {} (sign it: node app/scripts/sign-feature.mjs {})", legacy.revision, catalog_file(feed).display());
    }
    Ok(())
}

/// Puts a signed module package on the feed: <feed>/modules/files/<id>-<version>.zip and its
/// catalog entry on `channel`. The same version with other bytes is refused (members who have it
/// keep those). The v2 entry URL points into the shared modules/files/ folder.
fn publish(feed: &str, zip: &str, key: &str, channel: Channel) -> Result<(), String> {
    let bytes = std::fs::read(zip).map_err(|e| format!("{zip}: {e}"))?;
    let manifest = module::inspect(&bytes)?;
    let sig_file = format!("{zip}.sig");
    let signature = std::fs::read_to_string(&sig_file).map_err(|e| format!("{sig_file}: {e} (sign the package first: app/scripts/sign-feature.mjs)"))?.trim().to_string();
    let name = format!("{}-{}.zip", manifest.id, manifest.version);
    let entry = CatalogEntry { manifest, url: format!("../../modules/files/{name}"), sha256: crate::sha256_hex(&bytes), size: bytes.len() as u64, signature, yanked: false, channel };
    module::verify_package(&bytes, &entry, key).map_err(|e| format!("{sig_file}: {e}"))?;
    let c = read_v3(feed)?;
    if let Some(old) = c.entry(&entry.manifest.id, &entry.manifest.version) {
        if old.sha256 != entry.sha256 { return Err(format!("{name} is on the feed with other bytes; a changed module needs a new version")); }
        println!("module: {name} is already on the feed");
        return Ok(());
    }
    let files = catalog_file(feed).parent().map(|p| p.join("files")).ok_or("bad feed")?;
    std::fs::create_dir_all(&files).map_err(|e| e.to_string())?;
    std::fs::write(files.join(&name), &bytes).map_err(|e| e.to_string())?;
    change(feed, |c| {
        c.modules.push(entry);
        c.modules.sort_by(|a, b| (&a.manifest.id, &a.manifest.version).cmp(&(&b.manifest.id, &b.manifest.version)));
        Ok(())
    })
}

/// Moves a Preview version to Stable: the same bytes, now in the legacy catalog too. A Stable
/// version is done already (the retry of a stopped run).
fn promote(c: &mut Catalog, id: &str, version: &str) -> Result<(), String> {
    let e = c.modules.iter_mut().find(|e| e.manifest.id == id && e.manifest.version == version).ok_or_else(|| format!("{id} {version} is not on the feed"))?;
    if e.channel == Channel::Unknown {
        return Err(format!("{id} {version} has a channel that this tool does not know"));
    }
    e.channel = Channel::Stable;
    println!("module promoted: {id} {version}");
    Ok(())
}

fn set_yanked(c: &mut Catalog, id: &str, version: &str) -> Result<(), String> {
    let e = c.modules.iter_mut().find(|e| e.manifest.id == id && e.manifest.version == version).ok_or_else(|| format!("{id} {version} is not on the feed"))?;
    e.yanked = true;
    println!("module yanked: {id} {version}");
    Ok(())
}

fn revoke(c: &mut Catalog, id: &str, version: &str, reason: &str) -> Result<(), String> {
    if c.entry(id, version).is_none() { return Err(format!("{id} {version} is not on the feed")); }
    if reason.trim().is_empty() { return Err("a revocation needs a reason (the card shows it)".into()); }
    if c.revocation(id, version).is_none() { c.revoked.push(Revoked { id: id.into(), version: version.into(), reason: reason.into() }); }
    println!("module revoked: {id} {version}");
    Ok(())
}

/// For `fm_pack inspect` (sign-feature.mjs): the id and the version to sign for. A module
/// package signs for its version; the module catalog signs for its revision.
pub fn inspect(bytes: &[u8]) -> Option<serde_json::Value> {
    if let Ok(c) = serde_json::from_slice::<Catalog>(bytes) {
        return Some(serde_json::json!({ "id": "module-catalog", "version": c.revision.to_string(), "kind": "module-catalog" }));
    }
    let m = module::inspect(bytes).ok()?;
    Some(serde_json::json!({ "id": m.id, "version": m.version, "kind": "module" }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_install::test_signing::TestKey;

    // The signing path: an unsigned package or one signed by another key is refused with
    // nothing written; a signed one goes in; the same version with other bytes is refused. Each
    // change raises the revision and deletes the stale catalog signature; inspect gives the
    // version to sign for (the package version, the catalog revision).
    #[test]
    fn publish_yank_and_revoke_change_a_signed_feed() {
        let key = TestKey::default();
        let t = tempfile::tempdir().unwrap();
        let feed = t.path().join("feed").to_string_lossy().into_owned();
        let (entry, bytes) = key.module_entry("p42.test-camera", "1.0.0", 0.005);
        let zip = t.path().join("m.zip");
        std::fs::write(&zip, &bytes).unwrap();
        let zip_s = zip.to_string_lossy().into_owned();
        assert!(publish(&feed, &zip_s, &key.public(), Channel::Stable).unwrap_err().contains("sign the package first"));
        std::fs::write(format!("{zip_s}.sig"), TestKey::default().sign(&bytes, "1.0.0")).unwrap();
        assert!(publish(&feed, &zip_s, &key.public(), Channel::Stable).is_err());
        assert!(!catalog_file(&feed).exists(), "nothing written");
        std::fs::write(format!("{zip_s}.sig"), &entry.signature).unwrap();
        publish(&feed, &zip_s, &key.public(), Channel::Stable).unwrap();
        let c = read(&feed).unwrap();
        assert_eq!((c.revision, c.modules.len(), c.modules[0].url.as_str()), (1, 1, "files/p42.test-camera-1.0.0.zip"));
        assert_eq!(c.expires - c.issued, 365 * 24 * 3600, "valid for one year (production feed plan F8)");
        assert!(t.path().join("feed/modules/files/p42.test-camera-1.0.0.zip").is_file());
        assert_eq!(inspect(&std::fs::read(catalog_file(&feed)).unwrap()).unwrap()["version"], "1");
        assert_eq!(inspect(&bytes).unwrap()["version"], "1.0.0");
        // Same version, other bytes.
        let (other, other_bytes) = key.module_entry("p42.test-camera", "1.0.0", 0.006);
        std::fs::write(&zip, &other_bytes).unwrap();
        std::fs::write(format!("{zip_s}.sig"), &other.signature).unwrap();
        assert!(publish(&feed, &zip_s, &key.public(), Channel::Stable).unwrap_err().contains("other bytes"));
        std::fs::write(t.path().join("feed/modules/catalog.json.sig"), "old").unwrap();
        command(&["module-yank", &feed, "p42.test-camera", "1.0.0"]).unwrap();
        assert!(!t.path().join("feed/modules/catalog.json.sig").exists(), "the stale signature goes");
        assert!(command(&["module-revoke", &feed, "p42.test-camera", "1.0.0", " "]).is_err());
        command(&["module-revoke", &feed, "p42.test-camera", "1.0.0", "reads the wrong memory"]).unwrap();
        let c = read(&feed).unwrap();
        assert_eq!((c.revision, c.modules[0].yanked, c.revoked.len()), (3, true, 1));
        assert!(command(&["module-yank", &feed, "p42.none", "1.0.0"]).is_err());
        // module-verify: the catalog needs its signature, by the key, for these bytes.
        assert!(verify(&feed, &key.public()).unwrap_err().contains("catalog.json.sig"));
        let signed = std::fs::read(catalog_file(&feed)).unwrap();
        let sig_file = t.path().join("feed/modules/catalog.json.sig");
        std::fs::write(&sig_file, key.sign(&signed, "3")).unwrap();
        verify(&feed, &key.public()).unwrap();
        std::fs::write(&sig_file, TestKey::default().sign(&signed, "3")).unwrap();
        assert!(verify(&feed, &key.public()).is_err(), "another key");
        std::fs::write(&sig_file, key.sign(&signed, "2")).unwrap();
        assert!(verify(&feed, &key.public()).is_err(), "signed for another revision");
        // The app accepts the signed result, and an older signed revision is a replay.
        let bytes = std::fs::read(catalog_file(&feed)).unwrap();
        let root = module::Root::new(t.path().join("app"));
        root.accept_catalog(&root.writer().unwrap(), &bytes, &key.sign(&bytes, "3"), &key.public(), None).unwrap();
        let (old, sig) = key.sign_catalog(&Catalog { revision: 2, ..c });
        assert!(root.accept_catalog(&root.writer().unwrap(), &old, &sig, &key.public(), None).unwrap_err().contains("replay"));
    }

    // Plan slice 8: the v2 catalog (schema 2, view v2, own revision) holds both channels; the
    // legacy catalog gets only Stable entries and every revocation, and keeps its bytes and its
    // signature when only Preview changes. module-verify-v2 refuses the legacy bytes at the v2
    // path, and the app refuses the v2 bytes at the legacy path (schema 1 only).
    #[test]
    fn channels_split_the_module_catalogs() {
        let key = TestKey::default();
        let t = tempfile::tempdir().unwrap();
        let feed = t.path().join("feed").to_string_lossy().into_owned();
        let put = |version: &str, channel: Channel| {
            let (entry, bytes) = key.module_entry("p42.test-camera", version, 0.005);
            let zip = t.path().join(format!("m-{version}.zip"));
            std::fs::write(&zip, &bytes).unwrap();
            std::fs::write(format!("{}.sig", zip.display()), &entry.signature).unwrap();
            publish(&feed, &zip.to_string_lossy(), &key.public(), channel).unwrap();
        };
        let v2 = || -> V2 { serde_json::from_slice(&std::fs::read(t.path().join("feed").join(V2_CATALOG)).unwrap()).unwrap() };
        let listed = |c: &Catalog| c.modules.iter().map(|e| (e.manifest.version.clone(), e.channel)).collect::<Vec<_>>();
        put("1.0.0", Channel::Stable);
        let (legacy, new) = (read(&feed).unwrap(), v2());
        assert_eq!((legacy.revision, legacy.schema, new.c.revision, new.c.schema, new.view.as_str()), (1, 1, 1, 2, "v2"));
        assert_eq!((legacy.modules[0].url.as_str(), new.c.modules[0].url.as_str()), ("files/p42.test-camera-1.0.0.zip", "../../modules/files/p42.test-camera-1.0.0.zip"));

        // Preview only: the legacy catalog and its signature stay as they are.
        let legacy_sig = t.path().join("feed").join(module::CATALOG_SIG);
        std::fs::write(&legacy_sig, "signed").unwrap();
        let before = std::fs::read(catalog_file(&feed)).unwrap();
        put("1.1.0", Channel::Preview);
        assert_eq!((std::fs::read(catalog_file(&feed)).unwrap(), legacy_sig.exists()), (before, true));
        assert_eq!((v2().c.revision, listed(&v2().c)), (2, vec![("1.0.0".into(), Channel::Stable), ("1.1.0".into(), Channel::Preview)]));

        command(&["module-promote", &feed, "p42.test-camera", "1.1.0"]).unwrap();
        assert!(!legacy_sig.exists(), "a new legacy catalog loses the old signature");
        assert_eq!((read(&feed).unwrap().revision, listed(&read(&feed).unwrap()).len(), v2().c.revision), (2, 2, 3));
        command(&["module-promote", &feed, "p42.test-camera", "1.1.0"]).unwrap();
        assert_eq!((listed(&read(&feed).unwrap()).len(), v2().c.revision, read_v3(&feed).unwrap().revision), (2, 3, 4), "a retry advances the source and keeps unchanged old views");
        let stale = std::fs::read(catalog_file(&feed)).unwrap();
        command(&["module-revoke", &feed, "p42.test-camera", "1.0.0", "reads the wrong memory"]).unwrap();
        assert_eq!((read(&feed).unwrap().revoked.len(), v2().c.revoked.len()), (1, 1), "a revocation reaches old apps too");
        // A run that stopped after the v2 upload left the legacy catalog without the revocation:
        // the next run (feed-init) derives it again from v2 (code review r1).
        let done = std::fs::read(catalog_file(&feed)).unwrap();
        std::fs::write(catalog_file(&feed), &stale).unwrap();
        init(&feed).unwrap();
        assert_eq!(read(&feed).unwrap().revoked.len(), 1);
        assert_eq!(read(&feed).unwrap().revision, serde_json::from_slice::<Catalog>(&done).unwrap().revision);
        init(&feed).unwrap();
        assert_eq!(read(&feed).unwrap().revision, serde_json::from_slice::<Catalog>(&done).unwrap().revision, "a current legacy catalog keeps its revision");

        // The v2 pair: signed for its revision; the legacy bytes at the v2 path are refused.
        let (v2_file, v2_sig) = (t.path().join("feed").join(V2_CATALOG), t.path().join("feed").join(V2_CATALOG_SIG));
        let (bytes, rev) = (std::fs::read(&v2_file).unwrap(), v2().c.revision.to_string());
        std::fs::write(&v2_sig, key.sign(&bytes, &rev)).unwrap();
        verify_v2(&feed, &key.public()).unwrap();
        assert_eq!(inspect(&bytes).unwrap()["version"], rev);
        let legacy_bytes = std::fs::read(catalog_file(&feed)).unwrap();
        std::fs::write(&v2_file, &legacy_bytes).unwrap();
        std::fs::write(&v2_sig, key.sign(&legacy_bytes, &read(&feed).unwrap().revision.to_string())).unwrap();
        assert!(verify_v2(&feed, &key.public()).is_err(), "a legacy catalog at the v2 path");
        // Apps before the contract check schema 1, so they refuse these bytes; the legacy check
        // of this tool refuses them at the legacy path.
        assert_eq!(serde_json::from_slice::<Catalog>(&bytes).unwrap().schema, V2_SCHEMA);
        let legacy_sig = Path::new(&feed).join(module::CATALOG_SIG);
        let kept = (std::fs::read(catalog_file(&feed)).unwrap(), std::fs::read(&legacy_sig).ok());
        std::fs::write(catalog_file(&feed), &bytes).unwrap();
        std::fs::write(&legacy_sig, key.sign(&bytes, &rev)).unwrap();
        assert!(verify(&feed, &key.public()).unwrap_err().contains("v2 catalog at the legacy path"));
        std::fs::write(catalog_file(&feed), &kept.0).unwrap();
        match kept.1 {
            Some(sig) => std::fs::write(&legacy_sig, sig).unwrap(),
            None => std::fs::remove_file(&legacy_sig).unwrap(),
        }

        // feed-init on a legacy-only feed: a v2 catalog with the same entries, the legacy bytes kept.
        std::fs::remove_file(&v2_file).unwrap();
        init(&feed).unwrap();
        assert_eq!(std::fs::read(catalog_file(&feed)).unwrap(), legacy_bytes);
        assert_eq!((v2().c.revision, listed(&v2().c).len()), (1, 2));
        init(&feed).unwrap();
        assert_eq!(v2().c.revision, 1, "a second init changes nothing");
    }
}
