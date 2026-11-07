// Release channels of the feature feed (docs/plans/2026-10-28-release-admin.md, slice 8). The v2
// history of a feature (v2/features/<id>/versions.json) holds every version with its channel and
// is the source of truth. The v2 catalog and the legacy files that apps 0.1.x read are derived
// from it; the legacy ones get only Stable versions that fit LEGACY_APP, because those apps
// cannot filter a Preview version or one that needs a newer app.
use fm_install::feature::{history_path, Catalog, CatalogEntry, History};
use fm_install::release::{fits_app, Channel};
use std::path::{Path, PathBuf};

/// The newest app that ignores `channel` and `min_app` (plan, "Feature and module channels").
pub const LEGACY_APP: &str = "0.1.1";
/// The newest feature API that those apps load. A newer API also stays out of the legacy views,
/// with or without `min_app`: else its entry replaces the last version that they can run.
pub const LEGACY_API: u32 = 5;

/// Debug builds only: a test feed key (tools/publish/test/Test-PublishFeed.ps1 signs a test feed
/// with a throwaway key). The publish job builds release, which ignores it, as the app ignores
/// FM_TEST_BROWSER.
pub const TEST_KEY_ENV: &str = "FM_PACK_TEST_FEED_KEY";

/// The key that checks feed signatures: the compiled feed key, or the test key in a debug build.
pub fn key() -> &'static str {
    static TEST: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let test = TEST.get_or_init(|| std::env::var(TEST_KEY_ENV).ok().filter(|_| cfg!(debug_assertions)));
    test.as_deref().unwrap_or_else(fm_install::feature::feed_key)
}

pub fn legacy_dir(feed: &str) -> PathBuf {
    Path::new(feed).join("features")
}

pub fn v3_dir(feed: &str) -> PathBuf { Path::new(feed).join("v3/features") }

fn compatible(manifest: &fm_install::feature::Manifest) -> bool {
    manifest.format == 1 && manifest.games.iter().all(|id| matches!(*id, fm_platform::ApplicationId::ATS | fm_platform::ApplicationId::MSFS2024))
}

pub fn v3_history(feed: &str, id: &str) -> Result<History, String> {
    let file = v3_dir(feed).join(history_path(id));
    if file.exists() { read_json(&file) } else { v2_history(feed, id) }
}

pub fn v2_dir(feed: &str) -> PathBuf {
    Path::new(feed).join("v2/features")
}

/// The package file name of an entry URL ("files/x.zip" or "../../features/files/x.zip").
pub fn file_name(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

/// v2 entries point into the shared legacy files folder, relative to the v2 file, so the app's
/// Url::join gives <feed>/features/files/<name>, also under the staging prefix.
fn v2_url(name: &str) -> String {
    format!("../../features/files/{name}")
}

fn legacy_url(name: &str) -> String {
    format!("files/{name}")
}

pub fn read_catalog(dir: &Path) -> Result<Catalog, String> {
    read_json(&dir.join("catalog.json"))
}

pub fn read_history(dir: &Path, id: &str) -> Result<History, String> {
    read_json(&dir.join(history_path(id)))
}

// Only a missing file is empty: any other read error stops before a write, so a locked or
// unreadable file never makes the feed forget versions.
fn read_json<T: serde::de::DeserializeOwned + Default>(file: &Path) -> Result<T, String> {
    match std::fs::read(file) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(format!("{}: {e}", file.display())),
    }
}

// A file whose decoded content is equal keeps its bytes (other whitespace or field order from an
// older writer), so a derived view changes on the CDN only when its content changes (review r2).
fn write_json<T: serde::Serialize + serde::de::DeserializeOwned + PartialEq>(file: &Path, value: &T) -> Result<(), String> {
    if std::fs::read(file).ok().and_then(|b| serde_json::from_slice::<T>(&b).ok()).is_some_and(|old| old == *value) {
        return Ok(());
    }
    std::fs::create_dir_all(file.parent().ok_or("bad path")?).map_err(|e| e.to_string())?;
    std::fs::write(file, serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?).map_err(|e| format!("{}: {e}", file.display()))
}

/// Every feature id that has a v2 history (v2/features/index.json). The publish job uploads it
/// before the histories and reads it first, so a run that stopped after the history of a new id
/// (no catalog lists it yet) still finds that history (review r2). Not read by the app.
#[derive(Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct Index {
    ids: Vec<String>,
}

fn index_file(feed: &str) -> PathBuf {
    v2_dir(feed).join("index.json")
}

/// The v2 history of a feature. A feature that has none yet starts from its legacy history (all
/// Stable), or from its legacy catalog entry when the feed is older than history files.
pub fn v2_history(feed: &str, id: &str) -> Result<History, String> {
    let file = v2_dir(feed).join(history_path(id));
    if file.exists() {
        return read_json(&file);
    }
    let legacy = legacy_dir(feed);
    let mut h = read_history(&legacy, id)?;
    if h.versions.is_empty() {
        if let Some(old) = read_catalog(&legacy)?.features.into_iter().find(|f| f.manifest.id == id) {
            h.add(CatalogEntry { history: None, ..old }, 0)?;
        }
    }
    for v in &mut h.versions {
        v.entry.url = v2_url(file_name(&v.entry.url));
    }
    Ok(h)
}

fn newest(h: &History, channel: Channel) -> Option<&CatalogEntry> {
    h.versions.iter().find(|v| !v.withdrawn && v.entry.channel == channel).map(|v| &v.entry)
}

/// Writes the v2 history of `id` and every view derived from it, histories before catalogs, so
/// a reader never sees a catalog entry whose history file is missing.
pub fn write(feed: &str, id: &str, v2: &History) -> Result<(), String> {
    let (legacy, v2dir, v3dir) = (legacy_dir(feed), v2_dir(feed), v3_dir(feed));
    let path = history_path(id);
    let canonical_index = v3dir.join("index.json");
    let mut index: Index = read_json(&canonical_index)?;
    if !index.ids.iter().any(|i| i == id) { index.ids.push(id.to_string()); index.ids.sort(); }
    write_json(&canonical_index, &index)?;
    write_json(&v3dir.join(&path), v2)?;
    write_current_catalog(&v3dir, id, v2)?;
    let old = History { versions: v2.versions.iter().filter(|v| compatible(&v.entry.manifest)).cloned().collect() };
    let v2 = &old;
    let mut index: Index = read_json(&index_file(feed))?;
    if !v2.versions.is_empty() && !index.ids.iter().any(|i| i == id) { index.ids.push(id.to_string()); index.ids.sort(); }
    if !index.ids.is_empty() || index_file(feed).exists() { write_json(&index_file(feed), &index)?; }
    if !v2.versions.is_empty() || v2dir.join(&path).exists() { write_json(&v2dir.join(&path), v2)?; }
    // Legacy: Stable versions that an app 0.1.x can run. A Preview-only feature gets no file.
    let mut old = History::default();
    for v in v2.versions.iter().filter(|v| v.entry.channel == Channel::Stable && v.entry.manifest.api <= LEGACY_API && fits_app(v.entry.manifest.min_app.as_deref(), LEGACY_APP)) {
        let mut v = v.clone();
        v.entry.url = legacy_url(file_name(&v.entry.url));
        v.entry.art = v.entry.art.map(|a| a.map_urls(|u| legacy_url(file_name(u))));
        old.versions.push(v);
    }
    if !old.versions.is_empty() || legacy.join(&path).exists() {
        write_json(&legacy.join(&path), &old)?;
    }
    let mut cat = read_catalog(&legacy)?;
    cat.features.retain(|f| f.manifest.id != id);
    cat.features.extend(old.latest().map(|e| CatalogEntry { history: Some(path.clone()), ..e.clone() }));
    cat.features.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    if !cat.features.is_empty() || legacy.join("catalog.json").exists() {
        write_json(&legacy.join("catalog.json"), &cat)?;
    }
    write_current_catalog(&v2dir, id, v2)
}

fn write_current_catalog(dir: &Path, id: &str, history: &History) -> Result<(), String> {
    let mut cat = read_catalog(dir)?;
    cat.features.retain(|f| f.manifest.id != id);
    let stable = newest(history, Channel::Stable);
    let preview = newest(history, Channel::Preview).filter(|p| stable.is_none_or(|s| newer(&p.manifest.version, &s.manifest.version)));
    cat.features.extend([stable, preview].into_iter().flatten().map(|e| CatalogEntry { history: Some(history_path(id)), ..e.clone() }));
    cat.features.sort_by(|a, b| (&a.manifest.id, a.channel != Channel::Stable).cmp(&(&b.manifest.id, b.channel != Channel::Stable)));
    write_json(&dir.join("catalog.json"), &cat)
}

/// a > b in semver precedence, through the app's own rule (fits_app: app >= min_app).
fn newer(a: &str, b: &str) -> bool {
    fits_app(Some(b), a) && !fits_app(Some(a), b)
}

/// Makes the v2 view of each feature that has none (from the legacy files, all Stable, so no
/// legacy byte changes) and writes every derived view again from its v2 history. A run that
/// stopped after the v2 history upload left stale catalogs or a stale legacy history; the next
/// run of any kind repairs them (code review r1). Views that are current keep their bytes.
pub fn init(feed: &str) -> Result<(), String> {
    let mut ids: Vec<String> = read_json::<Index>(&v3_dir(feed).join("index.json"))?.ids;
    ids.extend(read_json::<Index>(&index_file(feed))?.ids);
    for dir in [legacy_dir(feed), v2_dir(feed), v3_dir(feed)] {
        ids.extend(read_catalog(&dir)?.features.into_iter().map(|f| f.manifest.id));
        if let Ok(dirs) = std::fs::read_dir(&dir) {
            ids.extend(dirs.flatten().filter(|d| d.path().join("versions.json").is_file()).map(|d| d.file_name().to_string_lossy().into_owned()));
        }
    }
    ids.sort();
    ids.dedup();
    let mut made = 0;
    for id in ids {
        fm_install::feature::check_id(&id).map_err(|e| e.to_string())?;
        // An indexed id whose history upload never happened has nothing to write.
        let h = v3_history(feed, &id)?;
        if h.versions.is_empty() { continue; }
        made += usize::from(!v2_dir(feed).join(history_path(&id)).exists());
        write(feed, &id, &h)?;
    }
    if !v2_dir(feed).join("catalog.json").exists() {
        write_json(&v2_dir(feed).join("catalog.json"), &Catalog::default())?;
    }
    if !v3_dir(feed).join("catalog.json").exists() { write_json(&v3_dir(feed).join("catalog.json"), &Catalog::default())?; }
    println!("feed-init: {made} feature histories made in v2");
    Ok(())
}

/// Adds a published package entry (URL already a file name) on `channel`.
pub fn add(feed: &str, entry: CatalogEntry, channel: Channel, published: u64) -> Result<bool, String> {
    let id = entry.manifest.id.clone();
    let mut h = v3_history(feed, &id)?;
    let name = file_name(&entry.url).to_string();
    let art = entry.art.as_ref().map(|a| a.map_urls(|u| v2_url(file_name(u))));
    let added = h.add(CatalogEntry { url: v2_url(&name), channel, art, ..entry }, published)?;
    write(feed, &id, &h)?;
    Ok(added)
}

/// Promotes a Preview version to Stable: the same bytes, now offered to Stable apps too. A
/// version that is Stable already is done (the retry of a stopped run), and its views are
/// written again.
pub fn promote(feed: &str, id: &str, version: &str) -> Result<(), String> {
    fm_install::feature::check_id(id).map_err(|e| e.to_string())?;
    let mut h = v3_history(feed, id)?;
    let v = h.versions.iter_mut().find(|v| v.entry.manifest.version == version).ok_or_else(|| format!("{id} {version} is not on the feed"))?;
    if v.withdrawn { return Err(format!("{id} {version} is withdrawn")); }
    match v.entry.channel {
        Channel::Preview => v.entry.channel = Channel::Stable,
        Channel::Stable => println!("{id} {version} is Stable already"),
        Channel::Unknown => return Err(format!("{id} {version} has a channel that this tool does not know")),
    }
    println!("promoted: {id} {version}");
    write(feed, id, &h)
}

/// Withdraws a version on every view: it stays in the histories and in files/.
pub fn withdraw(feed: &str, id: &str, version: &str) -> Result<(), String> {
    fm_install::feature::check_id(id).map_err(|e| e.to_string())?;
    let mut h = v3_history(feed, id)?;
    h.withdraw(version)?;
    println!("withdrawn: {id} {version}");
    write(feed, id, &h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_install::test_signing::TestKey;

    struct Feed {
        t: tempfile::TempDir,
        key: TestKey,
    }

    impl Feed {
        fn new() -> Feed {
            Feed { t: tempfile::tempdir().unwrap(), key: TestKey::default() }
        }
        fn root(&self) -> String {
            self.t.path().join("feed").to_string_lossy().into_owned()
        }
        /// A signed feature package (the library of this system, as `inspect` requires).
        /// API LEGACY_API, so a legacy app can run it unless `min_app` says otherwise.
        fn package(&self, id: &str, version: &str, min_app: Option<&str>) -> String {
            self.package_api(id, version, min_app, LEGACY_API)
        }
        fn package_api(&self, id: &str, version: &str, min_app: Option<&str>, api: u32) -> String {
            let src = self.t.path().join(format!("src-{id}-{version}"));
            std::fs::create_dir_all(&src).unwrap();
            let mut m = serde_json::json!({ "id": id, "name": "Channel test", "version": version, "description": "d", "api": api, "games": ["msfs2024"], "capabilities": [] });
            if let Some(v) = min_app { m["min_app"] = v.into(); }
            std::fs::write(src.join(fm_install::FEATURE_MANIFEST), m.to_string()).unwrap();
            let lib = self.t.path().join(format!("{id}-{version}.{}", std::env::consts::DLL_EXTENSION));
            std::fs::write(&lib, format!("{id}{version}")).unwrap();
            let out = self.t.path().join(format!("{id}-{version}.zip"));
            fm_install::feature::pack(&src, &[lib], &mut std::fs::File::create(&out).unwrap()).unwrap();
            std::fs::write(format!("{}.sig", out.display()), self.key.sign(&std::fs::read(&out).unwrap(), version)).unwrap();
            out.to_string_lossy().into_owned()
        }
        fn add(&self, id: &str, version: &str, min_app: Option<&str>, channel: Channel) {
            crate::catalog_with(&self.root(), &self.package(id, version, min_app), &self.key.public(), channel).unwrap();
        }
        fn bytes(&self, rel: &str) -> Vec<u8> {
            std::fs::read(self.t.path().join("feed").join(rel)).unwrap()
        }
        /// (version, channel) of each catalog entry of one view.
        fn listed(&self, rel: &str) -> Vec<(String, Channel)> {
            let c: Catalog = serde_json::from_slice(&self.bytes(rel)).unwrap();
            c.features.iter().map(|f| (f.manifest.version.clone(), f.channel)).collect()
        }
    }

    // Plan slice 8: a Preview version and a version that needs a newer app never reach the
    // legacy view (not one byte changes); a promotion moves the same bytes to Stable; the v2
    // catalog has the newest Stable entry and a newer Preview entry; v2 URLs resolve with
    // Url::join to the shared files folder, also under the staging prefix.
    #[test]
    fn channels_split_the_views() {
        let f = Feed::new();
        let s = |v: &str| (v.to_string(), Channel::Stable);
        let p = |v: &str| (v.to_string(), Channel::Preview);
        f.add("chan-test", "1.0.0", None, Channel::Stable);
        assert_eq!(f.listed("features/catalog.json"), [s("1.0.0")]);
        assert_eq!(f.listed("v2/features/catalog.json"), [s("1.0.0")]);
        let (cat, hist) = (f.bytes("features/catalog.json"), f.bytes("features/chan-test/versions.json"));

        f.add("chan-test", "1.1.0", None, Channel::Preview);
        assert_eq!((f.bytes("features/catalog.json"), f.bytes("features/chan-test/versions.json")), (cat.clone(), hist.clone()), "legacy bytes unchanged");
        assert_eq!(f.listed("v2/features/catalog.json"), [s("1.0.0"), p("1.1.0")]);

        promote(&f.root(), "chan-test", "1.1.0").unwrap();
        assert_eq!(f.listed("features/catalog.json"), [s("1.1.0")]);
        assert_eq!(f.listed("v2/features/catalog.json"), [s("1.1.0")], "the Preview entry is not newer any more");
        let cat = f.bytes("features/catalog.json");
        promote(&f.root(), "chan-test", "1.1.0").unwrap();
        assert_eq!(f.bytes("features/catalog.json"), cat, "a second promotion (a retry) changes nothing");
        assert!(promote(&f.root(), "chan-test", "9.0.0").is_err());

        // Needs a newer app than 0.1.x: Stable on v2, never on the legacy view.
        let cat = f.bytes("features/catalog.json");
        f.add("chan-test", "1.2.0", Some("0.2.0-preview.1"), Channel::Stable);
        assert_eq!(f.bytes("features/catalog.json"), cat);
        let legacy: History = serde_json::from_slice(&f.bytes("features/chan-test/versions.json")).unwrap();
        assert!(legacy.versions.iter().all(|v| v.entry.manifest.version != "1.2.0"));
        let v2: Catalog = serde_json::from_slice(&f.bytes("v2/features/catalog.json")).unwrap();
        assert_eq!((v2.features.len(), v2.features[0].manifest.min_app.as_deref()), (1, Some("0.2.0-preview.1")));
        let base = url::Url::parse("https://cdn.featuremachine.app/staging/v2/features/catalog.json").unwrap();
        assert_eq!(base.join(&v2.features[0].url).unwrap().as_str(), "https://cdn.featuremachine.app/staging/features/files/chan-test-1.2.0.zip");
        assert_eq!(base.join(v2.features[0].history.as_deref().unwrap()).unwrap().as_str(), "https://cdn.featuremachine.app/staging/v2/features/chan-test/versions.json");
        assert!(f.t.path().join("feed/features/files/chan-test-1.2.0.zip").is_file());

        withdraw(&f.root(), "chan-test", "1.2.0").unwrap();
        assert_eq!(f.listed("v2/features/catalog.json"), [s("1.1.0")]);

        // Review r1 (slice 2 of the surfaces plan): a newer feature API with no min_app is Stable
        // on v2 but stays out of the legacy view, which keeps the last version that it can run.
        let cat = f.bytes("features/catalog.json");
        crate::catalog_with(&f.root(), &f.package_api("chan-test", "1.3.0", None, fm_install::feature::FEATURE_API), &f.key.public(), Channel::Stable).unwrap();
        assert_eq!(f.bytes("features/catalog.json"), cat);
        assert_eq!(f.listed("v2/features/catalog.json"), [s("1.3.0")]);
        withdraw(&f.root(), "chan-test", "1.3.0").unwrap();

        // A Preview-only feature: no legacy history and no legacy catalog entry.
        f.add("new-test", "0.1.0", None, Channel::Preview);
        assert!(!f.t.path().join("feed/features/new-test").exists());
        assert_eq!(f.listed("features/catalog.json"), [s("1.1.0")]);
        assert_eq!(f.listed("v2/features/catalog.json"), [s("1.1.0"), p("0.1.0")]);
    }

    // The first run on the live feed: feed-init makes the v2 view from the legacy files, all
    // Stable, and changes no legacy byte; a second run changes nothing.
    #[test]
    fn init_makes_v2_from_a_legacy_feed() {
        let f = Feed::new();
        f.add("old-test", "1.0.0", None, Channel::Stable);
        f.add("old-test", "1.1.0", None, Channel::Stable);
        withdraw(&f.root(), "old-test", "1.1.0").unwrap();
        std::fs::remove_dir_all(f.t.path().join("feed/v2")).unwrap();
        let (cat, hist) = (f.bytes("features/catalog.json"), f.bytes("features/old-test/versions.json"));
        init(&f.root()).unwrap();
        assert_eq!((f.bytes("features/catalog.json"), f.bytes("features/old-test/versions.json")), (cat, hist));
        let h: History = serde_json::from_slice(&f.bytes("v2/features/old-test/versions.json")).unwrap();
        assert_eq!(h.versions.iter().map(|v| (v.entry.manifest.version.as_str(), v.withdrawn, v.entry.url.as_str())).collect::<Vec<_>>(), [("1.1.0", true, "../../features/files/old-test-1.1.0.zip"), ("1.0.0", false, "../../features/files/old-test-1.0.0.zip")]);
        let v2 = f.bytes("v2/features/catalog.json");
        init(&f.root()).unwrap();
        assert_eq!(f.bytes("v2/features/catalog.json"), v2);
    }

    // A promotion that stopped after the v2 history upload (code review r1): the live catalogs and
    // the legacy history still show the version as Preview. The next run (feed-init) writes them
    // again from the v2 history, and the retried promotion passes.
    #[test]
    fn init_repairs_the_views_of_a_stopped_promotion() {
        let f = Feed::new();
        f.add("stop-test", "1.0.0", None, Channel::Stable);
        f.add("stop-test", "1.1.0", None, Channel::Preview);
        let stale: Vec<(String, Vec<u8>)> = ["features/catalog.json", "features/stop-test/versions.json", "v2/features/catalog.json"].iter().map(|k| (k.to_string(), f.bytes(k))).collect();
        promote(&f.root(), "stop-test", "1.1.0").unwrap();
        let done: Vec<Vec<u8>> = stale.iter().map(|(k, _)| f.bytes(k)).collect();
        for (k, b) in &stale {
            std::fs::write(f.t.path().join("feed").join(k), b).unwrap();
        }
        init(&f.root()).unwrap();
        assert_eq!(stale.iter().map(|(k, _)| f.bytes(k)).collect::<Vec<_>>(), done);
        promote(&f.root(), "stop-test", "1.1.0").unwrap();
        assert_eq!(f.listed("features/catalog.json"), [(String::from("1.1.0"), Channel::Stable)]);
    }

    // A first publish of a new id that stopped after its v2 history upload: no catalog lists it
    // and its legacy history is missing. The index names it, so the next run lists it (review
    // r2). A legacy file from another writer (compact JSON) keeps its bytes when equal.
    #[test]
    fn init_finds_a_new_id_through_the_index_and_keeps_equal_bytes() {
        let f = Feed::new();
        f.add("old-test", "1.0.0", None, Channel::Stable);
        let (cat, v2cat) = (f.bytes("features/catalog.json"), f.bytes("v2/features/catalog.json"));
        f.add("new-test", "1.0.0", None, Channel::Stable);
        std::fs::write(f.t.path().join("feed/features/catalog.json"), &cat).unwrap();
        std::fs::write(f.t.path().join("feed/v2/features/catalog.json"), &v2cat).unwrap();
        std::fs::remove_file(f.t.path().join("feed/features/new-test/versions.json")).unwrap();
        init(&f.root()).unwrap();
        let s = |v: &str| (v.to_string(), Channel::Stable);
        assert_eq!(f.listed("features/catalog.json"), [s("1.0.0"), s("1.0.0")]);
        assert!(f.t.path().join("feed/features/new-test/versions.json").is_file());

        let compact = serde_json::to_vec(&serde_json::from_slice::<Catalog>(&f.bytes("features/catalog.json")).unwrap()).unwrap();
        std::fs::write(f.t.path().join("feed/features/catalog.json"), &compact).unwrap();
        init(&f.root()).unwrap();
        assert_eq!(f.bytes("features/catalog.json"), compact);
    }

    // A stopped run can leave a package file with no entry. A package with other bytes for that
    // version is refused: its entry would name a hash that the file does not have (review r1).
    #[test]
    fn an_orphan_file_with_other_bytes_is_refused() {
        let f = Feed::new();
        let zip = f.package("orphan-test", "1.0.0", None);
        let file = f.t.path().join("feed/features/files/orphan-test-1.0.0.zip");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"other bytes").unwrap();
        let e = crate::catalog_with(&f.root(), &zip, &f.key.public(), Channel::Stable).unwrap_err();
        assert!(e.contains("other bytes"), "{e}");
        std::fs::copy(&zip, &file).unwrap();
        crate::catalog_with(&f.root(), &zip, &f.key.public(), Channel::Stable).unwrap();
    }
}

#[cfg(test)]
mod application_views {
    use super::*;
    #[derive(serde::Deserialize)]
    enum OldGame { #[serde(rename="ats")] Ats, #[serde(rename="msfs2024")] Msfs }
    #[derive(serde::Deserialize)]
    struct OldEntry { games: Vec<OldGame> }
    #[derive(serde::Deserialize)]
    struct OldCatalog { features: Vec<OldEntry> }

    #[test]
    fn neutral_packages_never_reach_old_catalogs_or_histories() {
        let temp = tempfile::tempdir().unwrap();
        let feed = temp.path().to_str().unwrap();
        let entry = |id, target, format| serde_json::from_value::<CatalogEntry>(serde_json::json!({
            "format":format,"id":id,"name":"Status","description":"Status data","version":"1.0.0","api":6,
            "games":[target],"capabilities":[],"url":"files/status.zip","sha256":"a".repeat(64),"size":1,"signature":"s"
        })).unwrap();
        add(feed, entry("old-probe", "ats", 1), Channel::Stable, 1).unwrap();
        add(feed, entry("new-probe", "dbox-haptisync-center", 2), Channel::Stable, 1).unwrap();
        add(feed, entry("new-ats", "ats", 2), Channel::Stable, 1).unwrap();
        let v2 = std::fs::read(temp.path().join("v2/features/catalog.json")).unwrap();
        let old: OldCatalog = serde_json::from_slice(&v2).unwrap();
        assert_eq!(old.features.len(), 1);
        assert_eq!(old.features[0].games.len(), 1);
        assert!(!temp.path().join("v2/features/new-probe/versions.json").exists());
        let v3 = read_catalog(&temp.path().join("v3/features")).unwrap();
        assert_eq!(v3.features.len(), 3);
        promote(feed, "new-probe", "1.0.0").unwrap();
        withdraw(feed, "new-probe", "1.0.0").unwrap();
        assert_eq!(read_catalog(&temp.path().join("v3/features")).unwrap().features.len(), 2);
        assert_eq!(std::fs::read(temp.path().join("v2/features/catalog.json")).unwrap(), v2);
    }
}
