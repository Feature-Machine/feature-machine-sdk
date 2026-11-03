// Modules (module layer plan 2026-10-27, section 8.1, slice P1): data packages that features
// need, signed by Feature Machine and listed in a signed catalog. The app selects one immutable
// session lock (resolver, consent, store); the MSFS host reads that lock once at its start and
// takes each required map from it only, never from an embedded copy. One file, so the app, the
// host and fm_pack share every rule. In-sim WASM code is never a "module" here (M11).
use crate::feature::{Manifest, MapDependency, Requirement};
use crate::memory::{self, Signed};
use crate::Install;
use fm_platform::Game;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The manifest at the top of a module package.
pub const MANIFEST: &str = "module.json";
/// The namespace of Parallel 42 modules; no other author gets it (plan 7.2).
pub const OFFICIAL: &str = "p42";
/// The module license of decision M5.
pub const LICENSE: &str = "MIT OR Apache-2.0";
/// The feed paths of the module catalog and its signature, relative to the feed root.
pub const CATALOG: &str = "modules/catalog.json";
pub const CATALOG_SIG: &str = "modules/catalog.json.sig";
/// The v2 view of the feed (release admin plan, slice 8): one signed catalog for both channels,
/// schema 2 with the signed field `"view": "v2"`, so a legacy catalog never passes as v2.
pub const V2_CATALOG: &str = "v2/modules/catalog.json";
pub const V2_CATALOG_SIG: &str = "v2/modules/catalog.json.sig";
pub const V2_VIEW: &str = "v2";
const V2_SCHEMA: u32 = 2;
const SCHEMA: u32 = 1;
const KEPT_LOCKS: usize = 4;
const LIMIT: u64 = 16 * 1024 * 1024;

/// What a module can do, shown at consent. A closed list: a power that this build does not know
/// refuses the package, so consent never hides one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Power {
    /// Read-only reads of game memory through a camera map (the 6c camera contracts).
    GameMemoryRead,
    /// Writes of game memory through a setter map (mirrored-setter-v2): only its symbols, within
    /// their limits and clamps (ground wetness plan).
    GameMemoryWrite,
    #[serde(other)]
    Unknown,
}

/// Where a module runs (plan 4.3). P1 has data modules only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Data,
    #[serde(other)]
    Unknown,
}

/// One interface that the module provides: a map contract (camera or setter) at an interface
/// version, given by one map file of the package (its pin).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provided {
    pub interface: String,
    pub version: String,
    pub map: MapDependency,
}

/// The reviewed source of a module (Feature Machine builds from it, M2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub repository: String,
    pub commit: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModuleManifest {
    pub schema: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: Kind,
    pub games: Vec<Game>,
    pub provides: Vec<Provided>,
    pub powers: Vec<Power>,
    pub license: String,
    pub source: Source,
    /// The oldest app version that this module runs on (release-admin item 6, crate::release).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_app: Option<String>,
}

/// A module id: "<namespace>.<name>", each part a feature id. The namespace is the provider
/// identity that consent shows; an id never changes, so a new provider is a new id.
pub fn check_module_id(id: &str) -> Result<(), String> {
    // check_id allows no '.', so a third part fails on the name.
    let ok = id.split_once('.').is_some_and(|(ns, name)| crate::feature::check_id(ns).is_ok() && crate::feature::check_id(name).is_ok());
    ok.then_some(()).ok_or_else(|| format!("not a module id (<namespace>.<name>): {id:?}"))
}

/// The game id of the hosts ("msfs2024"): the serde name of Game.
fn game_name(g: &Game) -> String {
    serde_json::to_value(g).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}

impl ModuleManifest {
    /// The provider identity (the namespace of the id), shown at consent.
    pub fn provider(&self) -> &str {
        self.id.split_once('.').map_or("", |(ns, _)| ns)
    }

    /// The provider name that consent shows.
    pub fn provider_name(&self) -> String {
        if self.provider() == OFFICIAL { "Parallel 42".into() } else { self.provider().to_string() }
    }

    /// Every manifest rule before a store or a lock sees the module: schema, id, version, kind,
    /// games, 1 to 16 map interfaces with valid pins (one per map id), known powers that are
    /// exactly the ones that the maps need, the M5 license and a source commit.
    pub fn check(&self) -> Result<(), String> {
        check_module_id(&self.id)?;
        crate::check_version(&self.version).map_err(|e| e.to_string())?;
        crate::release::check_min_app(&self.id, self.min_app.as_deref())?;
        if self.schema != SCHEMA || self.kind != Kind::Data {
            return Err(format!("{}: this app knows module schema {SCHEMA} of kind data only", self.id));
        }
        if self.name.trim().is_empty() || self.name.len() > 60 || self.games.is_empty() {
            return Err(format!("{}: a name of 1 to 60 characters and a game are needed", self.id));
        }
        if self.powers.contains(&Power::Unknown) {
            return Err(format!("{} needs a power that this version of Feature Machine does not know", self.id));
        }
        if self.license != LICENSE {
            return Err(format!("{}: the module license must be {LICENSE} (decision M5)", self.id));
        }
        let hex = |s: &str| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit());
        if self.source.repository.trim().is_empty() || !hex(&self.source.commit) {
            return Err(format!("{}: the source needs a repository and a 40-character commit", self.id));
        }
        if self.provides.is_empty() || self.provides.len() > 16 {
            return Err(format!("{}: a data module provides 1 to 16 maps", self.id));
        }
        let mut maps = BTreeSet::new();
        let mut needed = BTreeSet::new();
        for p in &self.provides {
            let pin = fm_memory::valid_id(&p.map.id) && crate::check_version(&p.map.version).is_ok() && memory::digest_valid(&p.map.sha256);
            let power = interface_power(&p.interface);
            if power.is_none() || semver::Version::parse(&p.version).is_err() || !pin || !maps.insert(&p.map.id) {
                return Err(format!("{}: bad provided map {:?} (a camera or setter contract, an interface version, a valid pin, once)", self.id, p.map.id));
            }
            needed.extend(power);
        }
        // Consent shows the powers, so they are exactly what the maps do: none missing, none extra.
        if self.powers.iter().copied().collect::<BTreeSet<_>>() != needed {
            return Err(format!("{}: the powers must be the ones that its maps need (camera maps game_memory_read, setter maps game_memory_write)", self.id));
        }
        Ok(())
    }
}

/// The power that a provided interface needs: camera contracts read, the build-independent
/// setter contract writes. None: a contract that a module cannot provide (the exact-build v1
/// contracts are for one exe, so they stay embedded and signed by us).
fn interface_power(interface: &str) -> Option<Power> {
    match interface {
        memory::CAMERA_CONTRACT | memory::LOCATOR_CONTRACT => Some(Power::GameMemoryRead),
        memory::SETTER_V2_CONTRACT => Some(Power::GameMemoryWrite),
        _ => None,
    }
}

/// The body rules of one provided map: the pinned digest, the 6c body rules, the pinned id and
/// version, a map whose contract is the interface (a camera map, or a setter map that is not a
/// camera map), and a game of the module.
fn check_map(m: &ModuleManifest, p: &Provided, bytes: &[u8]) -> Result<Signed, String> {
    if memory::digest(bytes) != p.map.sha256 {
        return Err(format!("{}: map {} differs from its pinned digest", m.id, p.map.id));
    }
    let (map, contract) = memory::parse_package_contract(bytes)?;
    if map.id() != p.map.id || map.version() != p.map.version {
        return Err(format!("{}: map {} has another id or version than its pin", m.id, p.map.id));
    }
    let fits = match interface_power(&p.interface) {
        Some(Power::GameMemoryRead) => map.is_camera() && map.camera_contract() == Some(p.interface.as_str()),
        Some(Power::GameMemoryWrite) => !map.is_camera() && contract == p.interface,
        _ => false,
    };
    if !fits {
        return Err(format!("{}: map {} does not have the contract {}", m.id, p.map.id, p.interface));
    }
    if !m.games.iter().any(|g| game_name(g) == map.game()) {
        return Err(format!("{}: map {} is for another game", m.id, p.map.id));
    }
    Ok(map)
}

fn read_entry<R: Read + Seek>(a: &mut zip::ZipArchive<R>, name: &str) -> Result<Vec<u8>, String> {
    let f = a.by_name(name).map_err(|e| format!("{name}: {e}"))?;
    if f.size() > LIMIT { return Err(format!("{name} exceeds the size limit")); }
    let mut b = Vec::new();
    f.take(LIMIT + 1).read_to_end(&mut b).map_err(|e| e.to_string())?;
    Ok(b)
}

/// Reads and checks a module package: the entry rules of the store, the manifest, each
/// provided map, and no file that the manifest does not name (nothing unreviewed rides along).
pub fn inspect_archive<R: Read + Seek>(a: &mut zip::ZipArchive<R>) -> Result<ModuleManifest, String> {
    let entries = crate::checked_entries(a, crate::Kind::Module).map_err(|e| e.to_string())?;
    let m: ModuleManifest = serde_json::from_slice(&read_entry(a, MANIFEST)?).map_err(|e| format!("{MANIFEST}: {e}"))?;
    m.check()?;
    let mut named: BTreeSet<String> = [MANIFEST.to_string()].into();
    for p in &m.provides {
        let path = memory::map_path(&p.map);
        check_map(&m, p, &read_entry(a, &path)?)?;
        named.insert(path);
    }
    for (rel, dir) in entries {
        let name = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
        if !dir && !named.contains(&name) { return Err(format!("{}: unreviewed file in the module package: {name}", m.id)); }
    }
    Ok(m)
}

/// inspect_archive on package bytes.
pub fn inspect(bytes: &[u8]) -> Result<ModuleManifest, String> {
    let mut a = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| format!("not a module package: {e}"))?;
    inspect_archive(&mut a)
}

/// The store unpack check (Install::unpack): the same as inspect, for the version it unpacks.
pub(crate) fn verify_archive<R: Read + Seek>(a: &mut zip::ZipArchive<R>, version: &str) -> Result<(), String> {
    let m = inspect_archive(a)?;
    (m.version == version).then_some(()).ok_or_else(|| format!("{}: package version {} differs from {version}", m.id, m.version))
}

/// Builds a module package (fm_pack module) from a source folder with module.json and the maps
/// that it provides; the result passes inspect, or nothing is written.
pub fn pack(src: &Path, out: &mut (impl std::io::Write + Seek)) -> Result<ModuleManifest, String> {
    let text = std::fs::read(src.join(MANIFEST)).map_err(|e| format!("{MANIFEST}: {e}"))?;
    let m: ModuleManifest = serde_json::from_slice(&text).map_err(|e| format!("{MANIFEST}: {e}"))?;
    m.check()?;
    let mut files = vec![(MANIFEST.to_string(), text)];
    for p in &m.provides {
        let path = memory::map_path(&p.map);
        let bytes = std::fs::read(src.join(crate::feature::safe_rel(&path).map_err(|e| e.to_string())?)).map_err(|e| format!("{path}: {e}"))?;
        check_map(&m, p, &bytes)?;
        files.push((path, bytes));
    }
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).unix_permissions(0o644);
    let mut z = zip::ZipWriter::new(out);
    for (name, bytes) in files {
        z.start_file(name, opts).map_err(|e| e.to_string())?;
        std::io::Write::write_all(&mut z, &bytes).map_err(|e| e.to_string())?;
    }
    z.finish().map_err(|e| e.to_string())?;
    Ok(m)
}

/// One published module version in the catalog. `yanked`: no new selection (plan 7.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub manifest: ModuleManifest,
    /// Package URL, relative to the catalog.
    pub url: String,
    pub sha256: String,
    pub size: u64,
    /// The feed signature of the package, for its version (publishing plan L1).
    pub signature: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub yanked: bool,
    /// The channel of this version (release-admin item 6): one signed catalog holds both
    /// channels (item 7). Missing: stable (legacy catalog).
    #[serde(default, skip_serializing_if = "crate::release::Channel::is_stable")]
    pub channel: crate::release::Channel,
}

/// A revoked module version: no lock that names it starts (plan 7.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revoked {
    pub id: String,
    pub version: String,
    pub reason: String,
}

/// The signed module catalog. `revision` grows with each change (replay protection); `expires`
/// (Unix seconds) ends new selections from it, never a lock that already works (plan 7.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub schema: u32,
    pub revision: u64,
    pub issued: u64,
    pub expires: u64,
    pub modules: Vec<CatalogEntry>,
    #[serde(default)]
    pub revoked: Vec<Revoked>,
    /// None: the legacy view (schema 1). Some("v2"): the v2 view (schema 2). Signed with the rest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
}

impl Catalog {
    /// Checks the signature of the exact catalog bytes with `key`: signed for "version:<revision>"
    /// in the Tauri minisign format (the feed key, no new key type). Then the schema and view
    /// pair: schema 1 with no view (legacy) or schema 2 with view v2 (client contract item 7).
    pub fn verify(bytes: &[u8], signature: &str, key: &str) -> Result<Catalog, String> {
        let c: Catalog = serde_json::from_slice(bytes).map_err(|e| format!("module catalog: {e}"))?;
        crate::package::verify(bytes, signature, key, &c.revision.to_string()).map_err(|e| format!("module catalog: {e}"))?;
        match (c.schema, c.view.as_deref()) {
            (SCHEMA, None) => Ok(c),
            (V2_SCHEMA, Some(V2_VIEW)) => Ok(c),
            _ => Err("module catalog: unknown schema".into()),
        }
    }

    /// True for the v2 view (the only view that apps with the release contract fetch).
    pub fn is_v2(&self) -> bool {
        self.view.as_deref() == Some(V2_VIEW)
    }

    /// True while new selections may come from this catalog.
    pub fn fresh(&self, now: u64) -> bool {
        now <= self.expires
    }

    pub fn revocation(&self, id: &str, version: &str) -> Option<&Revoked> {
        self.revoked.iter().find(|r| r.id == id && r.version == version)
    }

    pub fn entry(&self, id: &str, version: &str) -> Option<&CatalogEntry> {
        self.modules.iter().find(|e| e.manifest.id == id && e.manifest.version == version)
    }
}

/// The checks of a downloaded module package before the store sees it: size and SHA-256 of the
/// catalog, the feed signature for its version, and a manifest equal to the catalog entry.
pub fn verify_package(bytes: &[u8], entry: &CatalogEntry, key: &str) -> Result<(), String> {
    if bytes.len() as u64 != entry.size || memory::digest(bytes) != entry.sha256 {
        return Err(format!("{} {}: the download does not match the catalog (size or SHA-256)", entry.manifest.id, entry.manifest.version));
    }
    crate::package::verify(bytes, &entry.signature, key, &entry.manifest.version)?;
    let m = inspect(bytes)?;
    (m == entry.manifest).then_some(()).ok_or_else(|| format!("{}: the package differs from its catalog entry", m.id))
}

/// Where a map came from in a lock: a module (selected external source) or the feature
/// package (embedded, legacy and 6c).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MapSource {
    Module,
    Embedded,
}

/// One map of a locked feature: the selected source and the exact file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedMap {
    pub id: String,
    pub source: MapSource,
    pub version: String,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedFeature {
    pub id: String,
    pub version: String,
    /// The feature package digest (package.sha256 of its version folder).
    pub sha256: String,
    pub maps: Vec<LockedMap>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedModule {
    pub id: String,
    pub version: String,
    pub sha256: String,
    /// The powers that the member approved for this identity (consent before activation).
    pub powers: Vec<Power>,
}

/// One immutable session lock (plan 6.2, P1 subset). Content-addressed: its file name is the
/// SHA-256 of its bytes. `catalog_revision` is not part of the selection: select compares the
/// features and modules, so a new catalog with the same selection makes no new lock, and a
/// rollback selects the kept file of that selection again (Sol r1 deviation note). Recovery
/// uses it as a replay floor that needs no clock (Sol r3 R3-1).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lock {
    pub schema: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_revision: Option<u64>,
    /// The view of that revision (None: legacy). v2 revisions count apart from legacy ones, so
    /// the replay floor compares only revisions of one view. Absent for legacy: same bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_view: Option<String>,
    pub features: Vec<LockedFeature>,
    pub modules: Vec<LockedModule>,
}

impl Lock {
    pub fn feature(&self, id: &str) -> Option<&LockedFeature> {
        self.features.iter().find(|f| f.id == id)
    }

    pub fn module(&self, id: &str) -> Option<&LockedModule> {
        self.modules.iter().find(|m| m.id == id)
    }

    fn bytes(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).unwrap_or_default()
    }

    /// The lock digest (its file name).
    pub fn digest(&self) -> String {
        memory::digest(&self.bytes())
    }

    /// True when `o` selects the same features, maps and modules (the provenance aside).
    /// Also the catalog view: the first v2 lock with the same bindings is a new lock, so the v2
    /// recovery floor has a lock of its view (Sol review r2).
    pub fn same_selection(&self, o: &Lock) -> bool {
        self.schema == o.schema && self.features == o.features && self.modules == o.modules && self.catalog_view == o.catalog_view
    }
}

/// The selected lock and the kept earlier ones (newest first), in one file, replaced atomically.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pointer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous: Vec<String>,
}

/// The identities and powers that the member approved (plan 7.2). A new id or a new power asks
/// again; a new version of the same id with no new power does not.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Consent {
    #[serde(default)]
    pub modules: BTreeMap<String, BTreeSet<Power>>,
}

impl Consent {
    /// True when the member approved this identity with at least these powers.
    pub fn covers(&self, m: &ModuleManifest) -> bool {
        self.modules.get(&m.id).is_some_and(|p| m.powers.iter().all(|x| p.contains(x)))
    }

    /// The powers of `m` that the member did not approve (all of them for a new id).
    pub fn new_powers(&self, m: &ModuleManifest) -> Vec<Power> {
        m.powers.iter().filter(|x| !self.modules.get(&m.id).is_some_and(|p| p.contains(x))).copied().collect()
    }

    pub fn is_new(&self, m: &ModuleManifest) -> bool {
        !self.modules.contains_key(&m.id)
    }

    pub fn approve(&mut self, m: &ModuleManifest) {
        self.modules.entry(m.id.clone()).or_default().extend(m.powers.iter().copied());
    }
}

/// The cached catalog: its bytes and signature in one file, so a stop between two writes can
/// never leave a signature of other bytes (the host would refuse every module feature).
#[derive(Serialize, Deserialize)]
struct Cached {
    catalog: String,
    signature: String,
}

/// The watermark of accepted catalogs (Sol r1 B2): the highest revision, the SHA-256 of its exact
/// bytes, and every revocation received. Two copies apart from the cache (Sol r2 R2-1): the
/// highest valid copy wins, so one damaged file recovers. A revocation stays when a later
/// catalog does not list it. It is final for its version: a fix ships as a new version.
/// `feed_time`: the highest authenticated feed time so far (Root::seen_time, Sol r4 R4-2).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Known {
    revision: u64,
    sha256: String,
    #[serde(default)]
    revoked: Vec<Revoked>,
    #[serde(default)]
    feed_time: u64,
    /// The view of `revision` (None: legacy). The first v2 catalog starts a new watermark and
    /// keeps the revocations (item 7); after it, a legacy catalog is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    view: Option<String>,
}

/// The trust state of a module folder (Sol r2 R2-1). New: nothing was accepted and no module
/// file is there (first use). Damaged: module files are there but no watermark copy reads, so
/// the revocations and the replay floor are unknown. Then no module activates, and only the
/// recovery rule can restore the state (Root::accept_catalog, plan 8.1).
enum Trust {
    New,
    Known(Known),
    Damaged(String),
}

/// Adds the revocations of `more` that `list` does not have (one entry per id and version).
fn add_revoked(list: &mut Vec<Revoked>, more: &[Revoked]) {
    for r in more {
        if !list.iter().any(|x| x.id == r.id && x.version == r.version) {
            list.push(r.clone());
        }
    }
}

/// What a fetched catalog did to the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accepted {
    /// A newer revision replaced the cache.
    Newer,
    /// The same bytes as the accepted revision (a damaged cache is written again).
    Same,
    /// The recovery rule restored a damaged watermark (Root::accept_catalog).
    Recovered,
}

/// The one writer of a module folder (Sol r1 B3): an exclusive OS lock on <modules>/.writer, so
/// threads and processes change the trust files, the store, the consent, the leases and the
/// lock pointer one at a time. Each change takes it and the drop releases it. The host reads
/// under a shared lock on the same file (Session::load, Sol r2 R2-5).
pub struct Writer {
    _file: std::fs::File,
    root: PathBuf,
}

/// How long the host waits for a writer to end before it refuses module maps (Session::load).
pub const READ_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Module versions that one pending change reserves (Sol r2 R2-3), on disk so that the prune of
/// every process sees them: leases/<name>.json, and leases/<name>.lock that the owner keeps
/// locked. The OS frees that lock when the owner process ends, so a prune removes the lease of
/// a dead owner (no process id or start time to compare). The drop removes the lease: the
/// change ended, was cancelled, or its thread panicked.
#[derive(Debug)]
pub struct Lease {
    dir: PathBuf,
    name: String,
    lock: Option<std::fs::File>,
}

impl Lease {
    /// Tests: the owner process ends with no clean-up. The lock goes (as the OS frees it), the
    /// files stay.
    #[cfg(test)]
    fn abandon(mut self) {
        self.lock = None;
    }
}

/// The module versions (id, version) that the live leases keep.
type Reserved = BTreeSet<(String, String)>;

/// An I/O error with its full path: the card shows it, so the member or support can act on
/// the right file (Sol r8 R8-3).
fn at(p: &Path, e: impl std::fmt::Display) -> String {
    format!("{}: {e}", p.display())
}

// Tests: work to run in Root::leased between the list of the lease files and their open
// (Sol r3 R3-2 interleave: an owner drops its lease there).
#[cfg(test)]
thread_local! {
    static AFTER_LIST: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
}

impl Drop for Lease {
    fn drop(&mut self) {
        // No lock: the test of a dead owner (abandon) keeps the files.
        if self.lock.is_none() { return; }
        let _ = std::fs::remove_file(self.dir.join(format!("{}.json", self.name)));
        self.lock = None;
        let _ = std::fs::remove_file(self.dir.join(format!("{}.lock", self.name)));
    }
}

/// The module folder of the app data: <data>/modules, next to <data>/features.
#[derive(Clone, Debug)]
pub struct Root(PathBuf);

impl Root {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self(dir.into())
    }

    /// <features dir>/../modules: the app and the hosts get the features dir from fm_paths.
    pub fn for_features(features_dir: &Path) -> Self {
        Self(features_dir.parent().unwrap_or(features_dir).join("modules"))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Waits for the writer of this folder and takes it (Writer). Do not keep it across an await
    /// and do not take it twice in one thread: the second take then waits for ever.
    pub fn writer(&self) -> Result<Writer, String> {
        std::fs::create_dir_all(&self.0).map_err(|e| at(&self.0, e))?;
        let path = self.0.join(".writer");
        let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path).map_err(|e| at(&path, e))?;
        file.lock().map_err(|e| at(&path, e))?;
        Ok(Writer { _file: file, root: self.0.clone() })
    }

    /// Each change checks that its writer is the one of this folder.
    fn owns(&self, w: &Writer) -> Result<(), String> {
        (w.root == self.0).then_some(()).ok_or_else(|| "the module writer is for another folder".into())
    }

    fn store(&self, id: &str) -> Install {
        Install::module(self.0.join("store").join(id))
    }

    /// The folder of a complete module version, if it is in the store.
    pub fn version_dir(&self, id: &str, version: &str) -> Option<PathBuf> {
        check_module_id(id).ok()?;
        let s = self.store(id);
        s.is_complete(version).then(|| s.version_dir(version))
    }

    /// The checked manifest and package digest of a stored module version.
    pub fn stored(&self, id: &str, version: &str) -> Result<(ModuleManifest, String), String> {
        self.stored_read(id, version)?.ok_or_else(|| format!("{id} {version} is not in the module store, or its files are damaged"))
    }

    /// stored() that keeps the two cases apart (Sol r7 sweep). Ok(None): the version is not
    /// complete, or its manifest or stamp is missing or damaged (add_package repairs it). Err: a
    /// file does not read, so a caller must not act as if the version were not there.
    fn stored_read(&self, id: &str, version: &str) -> Result<Option<(ModuleManifest, String)>, String> {
        let s = self.store(id);
        if check_module_id(id).is_err() || !s.complete(version).map_err(|e| e.to_string())? { return Ok(None); }
        let read = |f: &str| match std::fs::read(s.version_dir(version).join(f)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(at(&s.version_dir(version).join(f), e)),
        };
        let (Some(m), Some(sha)) = (read(MANIFEST)?, read(crate::PACKAGE_STAMP)?) else { return Ok(None) };
        let Ok(m) = serde_json::from_slice::<ModuleManifest>(&m) else { return Ok(None) };
        if m.check().is_err() || m.id != id || m.version != version { return Ok(None); }
        Ok(Some((m, String::from_utf8_lossy(&sha).trim().to_string())))
    }

    /// Every complete version in the store, with its manifest and digest. A damaged version is
    /// not in it. Err: a folder, an entry or a file does not read (Sol r7 sweep), so the lock
    /// build and the plans do not act on a part of the store.
    pub fn all_stored(&self) -> Result<Vec<(ModuleManifest, String)>, String> {
        let dir = self.0.join("store");
        let mut out = vec![];
        for s in crate::entries(&dir).map_err(|e| format!("module store {}: {e}", dir.display()))? {
            // Only add_package makes folders here, each with a module id name.
            let Some(id) = s.file_name().and_then(|n| n.to_str()).filter(|id| check_module_id(id).is_ok()) else { continue };
            // stale(&[]): every entry of versions/; stored_read skips the names that are no version.
            for v in self.store(id).stale(&[])? {
                let Some(version) = v.file_name().and_then(|n| n.to_str()) else { continue };
                out.extend(self.stored_read(id, version)?);
            }
        }
        Ok(out)
    }

    /// Verifies a downloaded package (verify_package) and unpacks it into the store. A version
    /// that is there with other bytes is refused: one version, one content.
    pub fn add_package(&self, w: &Writer, entry: &CatalogEntry, bytes: &[u8], key: &str) -> Result<(), String> {
        self.owns(w)?;
        verify_package(bytes, entry, key)?;
        let (id, version) = (&entry.manifest.id, &entry.manifest.version);
        // A stamp of other bytes is refused; the same stamp with another manifest is damage
        // (Sol r1 B2), so the verified package replaces the folder.
        match self.stored_read(id, version)? {
            Some((_, sha)) if sha != entry.sha256 => return Err(format!("{id} {version} is in the store with other bytes")),
            Some((m, _)) if m == entry.manifest => return Ok(()),
            _ => {}
        }
        let store = self.store(id);
        store.unpack(version, std::io::Cursor::new(bytes), true).map_err(|e| format!("{id} {version}: unpack failed: {}", at(&store.version_dir(version), e)))?;
        let stamp = store.version_dir(version).join(crate::PACKAGE_STAMP);
        fm_platform::files::write_replace(&stamp, entry.sha256.as_bytes()).map_err(|e| at(&stamp, e))
    }

    /// The cache of the view in use (v2_started).
    fn cache_file(&self) -> PathBuf {
        self.cache_file_of(self.v2_started())
    }

    /// The two watermark copies of the view in use (tests; the code names the view).
    #[cfg(test)]
    fn known_paths(&self) -> [PathBuf; 2] {
        self.known_paths_of(self.v2_started())
    }

    /// The v2 view has its own trust files (client contract item 7), so an app from before the
    /// release rules keeps its legacy cache and watermark after a switch back to it, and its
    /// legacy refresh never changes the v2 state (Sol review r5).
    fn cache_file_of(&self, v2: bool) -> PathBuf {
        self.0.join(if v2 { "catalog-v2.json" } else { "catalog.json" })
    }

    /// The two watermark copies of one view, written in this order.
    fn known_paths_of(&self, v2: bool) -> [PathBuf; 2] {
        if v2 { [self.0.join("known-v2-a.json"), self.0.join("known-v2-b.json")] } else { [self.0.join("known-a.json"), self.0.join("known-b.json")] }
    }

    /// True when a v2 trust file is there (or does not stat): from then on the v2 files decide,
    /// and a lost v2 watermark is damage, not a new start from the legacy one.
    fn v2_started(&self) -> bool {
        self.known_paths_of(true).iter().chain([&self.cache_file_of(true)]).any(|p| p.try_exists().unwrap_or(true))
    }

    /// The cache file checked with `key`, and the SHA-256 of its exact catalog bytes.
    fn read_cache(&self, key: &str) -> Result<Option<(Catalog, String)>, String> {
        let Some(bytes) = self.cache_bytes()? else { return Ok(None) };
        Self::check_cache(&bytes, key).map(Some)
    }

    /// The bytes of the cache file. None: there is none. Err: it does not read.
    fn cache_bytes(&self) -> Result<Option<Vec<u8>>, String> {
        match std::fs::read(self.cache_file()) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("module catalog cache {}", at(&self.cache_file(), e))),
        }
    }

    /// Cache bytes checked with `key`, and the SHA-256 of the exact catalog bytes in them.
    fn check_cache(bytes: &[u8], key: &str) -> Result<(Catalog, String), String> {
        let c: Cached = serde_json::from_slice(bytes).map_err(|e| format!("module catalog cache: {e}"))?;
        let catalog = Catalog::verify(c.catalog.as_bytes(), &c.signature, key)?;
        Ok((catalog, memory::digest(c.catalog.as_bytes())))
    }

    /// True when the folder has module state: a trust file, a lock or a stored module. A
    /// watermark that is missing then is lost, not first use. A folder that does not read
    /// counts as state: only a missing folder is empty (Sol r6 R6-1).
    fn established(&self) -> bool {
        let has = |p: PathBuf| std::fs::read_dir(p).map_or_else(|e| e.kind() != std::io::ErrorKind::NotFound, |mut d| d.next().is_some());
        // try_exists: a file that does not stat counts as state too (Sol r7 sweep).
        let trust = [false, true].map(|v2| self.known_paths_of(v2).into_iter().chain([self.cache_file_of(v2)])).into_iter().flatten();
        trust.chain([self.pointer_file()]).any(|p| p.try_exists().unwrap_or(true)) || has(self.0.join("locks")) || has(self.0.join("store"))
    }

    /// The trust state (Trust) of the view in use.
    fn trust(&self) -> Trust {
        self.trust_of(self.v2_started())
    }

    /// The trust state of one view. A copy that does not read or parse is damaged, never missing:
    /// with no valid copy, established state is Damaged (Sol r2 R2-1). Valid copies merge: the
    /// highest revision with its digest, and the revocations of both.
    fn trust_of(&self, v2: bool) -> Trust {
        let (mut best, mut bad): (Option<Known>, Vec<String>) = (None, vec![]);
        for p in self.known_paths_of(v2) {
            let k = match std::fs::read(&p) {
                Ok(b) => serde_json::from_slice::<Known>(&b).map_err(|e| e.to_string()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => Err(e.to_string()),
            };
            match (k, best.as_mut()) {
                (Err(e), _) => bad.push(format!("{}: {e}", p.display())),
                (Ok(k), None) => best = Some(k),
                (Ok(k), Some(b)) => {
                    add_revoked(&mut b.revoked, &k.revoked);
                    b.feed_time = b.feed_time.max(k.feed_time);
                    if k.revision > b.revision { (b.revision, b.sha256, b.view) = (k.revision, k.sha256, k.view); }
                }
            }
        }
        match best {
            Some(k) => Trust::Known(k),
            None if !bad.is_empty() => Trust::Damaged(format!("the module watermark is damaged ({})", bad.join("; "))),
            None if self.established() => Trust::Damaged("the module watermark is missing".into()),
            None => Trust::New,
        }
    }

    /// True before the first accepted catalog, with no module file (first use). The app writes
    /// no lock then: a lock file would make a later missing watermark read as lost trust (R4-1).
    pub fn first_use(&self) -> bool {
        matches!(self.trust(), Trust::New)
    }

    /// Ok only when a catalog was accepted and the watermark reads (Trust::Known). select and
    /// prune need it (Sol r4 R4-1, r5 R5-1): with damaged trust, the lock files and packages are
    /// the replay floor and the last good state, so nothing changes until a recovery succeeds.
    pub fn trusted(&self) -> Result<(), String> {
        match self.trust() {
            Trust::Known(_) => Ok(()),
            Trust::New => Err("no module catalog was accepted yet, so no module lock is written".into()),
            Trust::Damaged(e) => Err(format!("{e}; no module lock or package changes until the module list is restored")),
        }
    }

    /// The highest authenticated feed time so far (0: none or unknown). The app takes the
    /// effective time as the higher of it and the PC clock, so a PC clock that is behind it
    /// cannot keep an expired catalog fresh (Sol r4 R4-2, the time model of plan 8.1).
    pub fn seen_time(&self) -> u64 {
        match self.trust() {
            Trust::Known(k) => k.feed_time,
            Trust::New | Trust::Damaged(_) => 0,
        }
    }

    /// Every revocation that the app received (the watermark): the lock build and the host
    /// refuse them also when the cache is damaged or a later catalog does not list them.
    /// Err: the watermark is damaged, so the revocations are unknown (no module activates).
    pub fn known_revoked(&self) -> Result<Vec<Revoked>, String> {
        match self.trust() {
            Trust::New => Ok(vec![]),
            Trust::Known(k) => Ok(k.revoked),
            Trust::Damaged(e) => Err(e),
        }
    }

    /// The cached signed catalog. None: no catalog was ever accepted. Err: the cache is damaged,
    /// not signed with `key`, not the accepted revision, or the watermark is damaged.
    pub fn cached_catalog(&self, key: &str) -> Result<Option<Catalog>, String> {
        let k = match self.trust() {
            Trust::Damaged(e) => return Err(e),
            Trust::Known(k) => Some(k),
            Trust::New => None,
        };
        let Some((c, sha)) = self.read_cache(key)? else { return Ok(None) };
        match k {
            Some(k) if c.view != k.view || c.revision < k.revision || (c.revision == k.revision && sha != k.sha256) => {
                Err(format!("module catalog cache: revision {} is not the accepted revision {}", c.revision, k.revision))
            }
            _ => Ok(Some(c)),
        }
    }

    /// Writes each watermark copy of the view of `k` that differs from `k`, A before B. Each
    /// write is durable (write_replace), so the watermark is on disk before the cache changes.
    fn write_known(&self, w: &Writer, k: &Known) -> Result<(), String> {
        self.owns(w)?;
        let bytes = serde_json::to_vec_pretty(k).map_err(|e| e.to_string())?;
        for p in self.known_paths_of(k.view.is_some()) {
            if std::fs::read(&p).ok().and_then(|b| serde_json::from_slice::<Known>(&b).ok()).as_ref() != Some(k) {
                fm_platform::files::write_replace(&p, &bytes).map_err(|e| format!("module catalog watermark {}", at(&p, e)))?;
            }
        }
        Ok(())
    }

    /// Takes a fetched catalog: a valid signature and a revision above the watermark (the same
    /// revision only with the same exact bytes). An older one is a replay (plan 7.1). A damaged
    /// watermark takes the recovery rule (recover). The watermark is on disk before the cache,
    /// and a stop between them reads as an error until the next fetch. `feed_time`: the time
    /// that TLS authenticates for this answer (HTTPS Date plus Age): recovery and seen_time use it.
    pub fn accept_catalog(&self, w: &Writer, bytes: &[u8], signature: &str, key: &str, feed_time: Option<u64>) -> Result<(Catalog, Accepted), String> {
        self.owns(w)?;
        let new = Catalog::verify(bytes, signature, key)?;
        let text = String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string())?;
        let sha256 = memory::digest(bytes);
        let (v2, started) = (new.is_v2(), self.v2_started());
        if !v2 && started {
            return Err("a legacy module catalog after the v2 catalog is refused".into());
        }
        // The first v2 catalog starts the v2 files from the legacy trust (item 7).
        let first_v2 = v2 && !started;
        let (mut revoked, accepted, seen) = match self.trust_of(v2 && started) {
            Trust::New => (vec![], Accepted::Newer, 0),
            // Its revisions count apart, so it starts a new watermark with the revocations
            // received so far. A v2 lock file is still a floor: deleted v2 files are no new start.
            Trust::Known(k) if first_v2 => {
                if let Some(f) = self.lock_revisions(Some(V2_VIEW))?.into_iter().max().filter(|f| new.revision < *f) {
                    return Err(format!("module catalog revision {} is older than revision {f} that a kept session lock records (refused as a replay)", new.revision));
                }
                (k.revoked, Accepted::Newer, k.feed_time)
            }
            Trust::Known(k) => {
                if new.revision < k.revision {
                    return Err(format!("module catalog revision {} is older than {} (refused as a replay)", new.revision, k.revision));
                }
                if new.revision == k.revision && sha256 != k.sha256 {
                    return Err(format!("module catalog revision {} has other content (refused)", new.revision));
                }
                (k.revoked, if new.revision == k.revision { Accepted::Same } else { Accepted::Newer }, k.feed_time)
            }
            Trust::Damaged(e) => (self.recover(&new, key, feed_time).map_err(|why| format!("{e}; {why}"))?, Accepted::Recovered, 0),
        };
        add_revoked(&mut revoked, &new.revoked);
        // Never lowered. A feed time after the expiry is kept as expiry + 1 s: it proves that this
        // catalog expired, but a bad Date cannot make a later catalog look expired (R4-2).
        let feed_time = seen.max(feed_time.map_or(0, |t| t.min(new.expires.saturating_add(1))));
        std::fs::create_dir_all(&self.0).map_err(|e| at(&self.0, e))?;
        self.write_known(w, &Known { revision: new.revision, sha256, revoked, feed_time, view: new.view.clone() })?;
        if accepted != Accepted::Same || !matches!(self.cached_catalog(key), Ok(Some(_))) {
            let cached = serde_json::to_vec(&Cached { catalog: text, signature: signature.trim().to_string() }).map_err(|e| e.to_string())?;
            let file = self.cache_file_of(v2);
            fm_platform::files::write_replace(&file, &cached).map_err(|e| format!("module catalog cache {}", at(&file, e)))?;
        }
        Ok((new, accepted))
    }

    /// The recovery rule of a damaged watermark (Sol r2 R2-1, r3 R3-1). The PC clock can be
    /// wrong, so expiry is checked only at `feed_time` (authenticated). No such time: refused.
    /// A floor that needs no clock: the revision must not be older than a readable cache or a
    /// lock file (each lock records the revision that built it). Returns the cache revocations.
    fn recover(&self, new: &Catalog, key: &str, feed_time: Option<u64>) -> Result<Vec<Revoked>, String> {
        let Some(t) = feed_time else {
            return Err("Feature Machine restores it only with the time of a secure (HTTPS) answer of the module feed, and this answer did not give one".into());
        };
        if !new.fresh(t) {
            return Err(format!("only a module catalog that is not expired can restore it, and revision {} is expired", new.revision));
        }
        // A cache that does not read refuses: its revision is not known (Sol r7 R7-2). Damaged
        // content gives no floor: the recovery is for lost trust files (Sol r2 R2-1), and the
        // lock files stay the floor.
        let bytes = self.cache_bytes().map_err(|e| format!("{e}; the replay floor is not known"))?;
        let cache = bytes.and_then(|b| Self::check_cache(&b, key).ok()).map(|(c, _)| c);
        // A floor counts only in the view of `new`; a v2 cache still gives its revocations.
        let floor = cache.iter().filter(|c| c.view == new.view).map(|c| c.revision).chain(self.lock_revisions(new.view.as_deref())?).max();
        if let Some(f) = floor.filter(|f| new.revision < *f) {
            return Err(format!("module catalog revision {} is older than revision {f} that the cache or a kept session lock records (refused as a replay)", new.revision));
        }
        Ok(cache.map(|c| c.revoked).unwrap_or_default())
    }

    /// The catalog revision of each lock file (the recovery floor), in the whole folder: a lost
    /// pointer must not lower it. Err: the folder, an entry or a lock does not read, or a lock is
    /// damaged. Its revision is not known, so recovery is refused (Sol r6 R6-1, r7 R7-2: fail
    /// closed). A name that is not <digest>.json is no lock: select writes only those names.
    fn lock_revisions(&self, view: Option<&str>) -> Result<Vec<u64>, String> {
        let dir = self.0.join("locks");
        let list = crate::entries(&dir).map_err(|e| format!("module locks {}: {e}", dir.display()))?;
        let mut out = vec![];
        for d in list.iter().filter_map(|p| p.file_name()?.to_str()?.strip_suffix(".json")).filter(|d| memory::digest_valid(d)) {
            let read = self.read_lock_file(d).map_err(|e| format!("module lock {}; the replay floor is not known", at(&self.lock_file(d), e)))?;
            let lock = read.map_err(|e| format!("{e}; the replay floor is not known"))?;
            out.extend(lock.catalog_revision.filter(|_| lock.catalog_view.as_deref() == view));
        }
        Ok(out)
    }

    /// A shared lock on .writer for one consistent read (Sol r2 R2-5), waiting at most `wait`
    /// for a writer to end. None: no writer ever ran here, so there is nothing to wait for.
    fn read_lock(&self, wait: std::time::Duration) -> Result<Option<std::fs::File>, String> {
        let path = self.0.join(".writer");
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("module files {}", at(&path, e))),
        };
        let end = std::time::Instant::now() + wait;
        loop {
            match file.try_lock_shared() {
                Ok(()) => return Ok(Some(file)),
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < end => std::thread::sleep(std::time::Duration::from_millis(20)),
                Err(std::fs::TryLockError::WouldBlock) => return Err("the module files stay busy (Feature Machine changes them); start the game again".into()),
                Err(std::fs::TryLockError::Error(e)) => return Err(format!("module files {}", at(&path, e))),
            }
        }
    }

    /// Reserves `modules` (id, version) for one pending change (Lease). Under the writer, so no
    /// prune runs between the plan of the change and its lease (Sol r2 R2-2).
    pub fn lease(&self, w: &Writer, modules: &BTreeSet<(String, String)>) -> Result<Lease, String> {
        self.owns(w)?;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = self.0.join("leases");
        std::fs::create_dir_all(&dir).map_err(|e| format!("module lease {}", at(&dir, e)))?;
        let name = format!("{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(format!("{name}.lock"))).map_err(|e| format!("module lease {}", at(&dir.join(format!("{name}.lock")), e)))?;
        lock.try_lock().map_err(|e| format!("module lease {}", at(&dir.join(format!("{name}.lock")), e)))?;
        let lease = Lease { dir, name, lock: Some(lock) };
        let bytes = serde_json::to_vec(modules).map_err(|e| e.to_string())?;
        let json = lease.dir.join(format!("{}.json", lease.name));
        fm_platform::files::write_replace(&json, &bytes).map_err(|e| format!("module lease {}", at(&json, e)))?;
        Ok(lease)
    }

    /// The module versions of the leases whose owner lives (its .lock is locked), in all
    /// processes, and the removal errors of dead leases (they go). A lease that goes between the
    /// list and the open was dropped by its owner, so it keeps nothing (Sol r3 R3-2). Err: the
    /// folder, an entry or a live lease does not read, so a prune must not guess (Sol r6 R6-1).
    fn leased(&self, w: &Writer) -> Result<(Reserved, Vec<String>), String> {
        self.owns(w)?;
        let dir = self.0.join("leases");
        let list = crate::entries(&dir).map_err(|e| format!("module leases {}: {e}", dir.display()))?;
        #[cfg(test)]
        if let Some(f) = AFTER_LIST.with(|h| h.borrow_mut().take()) { f() }
        let (mut out, mut failed, mut done) = (BTreeSet::new(), vec![], BTreeSet::new());
        let remove = |p: &Path, failed: &mut Vec<String>, done: &mut BTreeSet<PathBuf>| {
            done.insert(p.to_path_buf());
            if let Err(e) = crate::remove_file(p) { failed.push(format!("{}: {e}", p.display())); }
        };
        for path in &list {
            let Some(name) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".lock")).map(String::from) else { continue };
            let json = dir.join(format!("{name}.json"));
            let f = match std::fs::OpenOptions::new().write(true).open(path) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("module lease {}", at(path, e))),
            };
            match f.try_lock() {
                Ok(()) => {
                    drop(f);
                    remove(&json, &mut failed, &mut done);
                    remove(path, &mut failed, &mut done);
                }
                Err(std::fs::TryLockError::WouldBlock) => match std::fs::read(&json) {
                    Ok(b) => out.extend(serde_json::from_slice::<Reserved>(&b).map_err(|e| format!("module lease {}", at(&json, e)))?),
                    // The owner removes it now (Lease::drop): it keeps nothing.
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(format!("module lease {}", at(&json, e))),
                },
                Err(std::fs::TryLockError::Error(e)) => return Err(format!("module lease {}", at(path, e))),
            }
        }
        // A .json with no .lock: the owner stopped before it made the lock, or after the drop.
        for p in list.iter().filter(|p| p.extension().is_some_and(|x| x == "json")) {
            let lock = p.with_extension("lock");
            if lock.try_exists().map_err(|e| format!("module lease {}: {e}", lock.display()))? || done.contains(p) { continue; }
            remove(p, &mut failed, &mut done);
        }
        Ok((out, failed))
    }

    fn consent_file(&self) -> PathBuf {
        self.0.join("consent.json")
    }

    /// The member's approvals. A missing or damaged file approves nothing (the next approval
    /// writes a valid one). Err: the file does not read, so a caller must not save over
    /// approvals that it could not see (Sol r7 sweep).
    pub fn consent(&self) -> Result<Consent, String> {
        match std::fs::read(self.consent_file()) {
            Ok(b) => Ok(serde_json::from_slice(&b).unwrap_or_default()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Consent::default()),
            Err(e) => Err(format!("module consent {}", at(&self.consent_file(), e))),
        }
    }

    /// Read the consent after the writer is taken, so two approvals cannot lose one.
    pub fn save_consent(&self, w: &Writer, c: &Consent) -> Result<(), String> {
        self.owns(w)?;
        std::fs::create_dir_all(&self.0).map_err(|e| at(&self.0, e))?;
        fm_platform::files::write_replace(&self.consent_file(), &serde_json::to_vec_pretty(c).map_err(|e| e.to_string())?).map_err(|e| format!("module consent {}", at(&self.consent_file(), e)))
    }

    fn pointer_file(&self) -> PathBuf {
        self.0.join("lock.json")
    }

    fn lock_file(&self, digest: &str) -> PathBuf {
        self.0.join("locks").join(format!("{digest}.json"))
    }

    pub fn pointer(&self) -> Result<Pointer, String> {
        match std::fs::read(self.pointer_file()) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("module lock pointer {}", at(&self.pointer_file(), e))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Pointer::default()),
            Err(e) => Err(format!("module lock pointer {}", at(&self.pointer_file(), e))),
        }
    }

    /// A lock by digest; its bytes must have that digest (an edited lock is refused).
    pub fn lock(&self, digest: &str) -> Result<Lock, String> {
        self.read_lock_file(digest).map_err(|e| format!("module lock {}", at(&self.lock_file(digest), e)))?
    }

    /// A lock file by digest. Err: the file does not read (an I/O error, NotFound too). Ok(Err):
    /// it reads, but its content is damaged (a bad name, other bytes than the digest, no
    /// parse). Each caller states what the two cases mean (Sol r7 R7-1, R7-2).
    fn read_lock_file(&self, digest: &str) -> std::io::Result<Result<Lock, String>> {
        if !memory::digest_valid(digest) { return Ok(Err("bad lock name".into())); }
        let bytes = std::fs::read(self.lock_file(digest))?;
        if memory::digest(&bytes) != digest { return Ok(Err(format!("module lock {} changed on disk", self.lock_file(digest).display()))); }
        Ok(serde_json::from_slice(&bytes).map_err(|e| format!("module lock {}", at(&self.lock_file(digest), e))))
    }

    /// A lock that the pointer names. None: it is missing or damaged. It then never loads (the
    /// host checks the digest) and is never selected again, so nothing can use what only it
    /// names. Lock files never change (write_replace, then no write), so this is not a moment.
    /// Err: it does not read, so the caller stops (Sol r7 R7-1).
    fn kept_lock(&self, digest: &str) -> Result<Option<Lock>, String> {
        match self.read_lock_file(digest) {
            Ok(read) => Ok(read.ok()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("module lock {}", at(&self.lock_file(digest), e))),
        }
    }

    /// The selected lock. None before the first lock.
    pub fn current(&self) -> Result<Option<Lock>, String> {
        self.pointer()?.current.map(|d| self.lock(&d)).transpose()
    }

    /// The selected lock, then the kept earlier ones (kept_lock). Err: the pointer or a kept lock
    /// does not read (Sol r7 R7-1).
    pub fn kept(&self) -> Result<Vec<Lock>, String> {
        let p = self.pointer()?;
        let mut out = vec![];
        for d in p.current.iter().chain(&p.previous) { out.extend(self.kept_lock(d)?); }
        Ok(out)
    }

    /// Selects `lock` (plan 6.2: one atomic step): the immutable file first, then one replace of
    /// the pointer. The same selection again changes nothing (provenance aside), and a kept lock
    /// with the same selection is selected again by its own file, so the history holds each
    /// selection once (Sol r1 deviation note). Returns true when the selection changed.
    pub fn select(&self, w: &Writer, lock: &Lock) -> Result<bool, String> {
        self.owns(w)?;
        self.trusted()?;
        let mut p = self.pointer()?;
        // A kept lock that does not read stops the selection (kept_lock, Sol r7 sweep).
        let same = |d: &String| Ok::<_, String>(self.kept_lock(d)?.is_some_and(|l| l.same_selection(lock)));
        if let Some(c) = &p.current { if same(c)? { return Ok(false); } }
        let mut digest = lock.digest();
        for d in &p.previous { if same(d)? { digest = d.clone(); break; } }
        let file = self.lock_file(&digest);
        std::fs::create_dir_all(file.parent().unwrap_or(&self.0)).map_err(|e| at(&file, e))?;
        // An existing file is used only when it reads and its digest checks (Sol r9 R9-2). Damaged
        // content of this build is written again from its bytes; a read error stops with the path.
        let valid = match self.read_lock_file(&digest) {
            Ok(read) => read.is_ok(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(format!("module lock {}", at(&file, e))),
        };
        if !valid {
            if digest != lock.digest() { return Err(format!("module lock {} changed on disk", file.display())); }
            fm_platform::files::write_replace(&file, &lock.bytes()).map_err(|e| format!("module lock {}", at(&file, e)))?;
        }
        if let Some(old) = p.current.take() { p.previous.insert(0, old); }
        p.previous.retain(|d| *d != digest);
        p.previous.dedup();
        p.previous.truncate(KEPT_LOCKS);
        p.current = Some(digest);
        fm_platform::files::write_replace(&self.pointer_file(), &serde_json::to_vec_pretty(&p).map_err(|e| e.to_string())?).map_err(|e| format!("module lock pointer {}", at(&self.pointer_file(), e)))?;
        Ok(true)
    }

    /// Removes module versions and lock files that no kept lock names and no live lease of any
    /// process reserves (Sol r1 B4, r2 R2-3: a module downloaded for an install that is not
    /// active yet). The leases are read under the same writer. Call it only when no game runs:
    /// a running game can use any kept lock. Only with trusted state (Sol r5 R5-1). A removal
    /// that fails is an error after the other removals ran (R5-2); a path already gone is not.
    pub fn prune(&self, w: &Writer) -> Result<(), String> {
        self.trusted()?;
        let (reserved, mut failed) = self.leased(w)?;
        let p = self.pointer()?;
        let names: Vec<String> = p.current.iter().chain(&p.previous).cloned().collect();
        // Every kept lock reads before any removal (Sol r7 R7-1); kept_lock gives the rule.
        let mut kept: Vec<Lock> = vec![];
        for d in &names { kept.extend(self.kept_lock(d)?); }
        // Every list first: a folder or an entry that does not read stops the prune before any
        // removal (Sol r6 R6-1). Then each removal runs, and the failed ones are returned.
        let read = |d: PathBuf| crate::entries(&d).map_err(|e| format!("{}: {e}", d.display()));
        let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let locks: Vec<PathBuf> = read(self.0.join("locks"))?.into_iter().filter(|p| !names.contains(&name(p).trim_end_matches(".json").to_string())).collect();
        let mut dirs: Vec<PathBuf> = vec![];
        for s in read(self.0.join("store"))? {
            let id = name(&s);
            let mut keep: Vec<String> = kept.iter().filter_map(|l| l.module(&id)).map(|m| m.version.clone()).collect();
            keep.extend(reserved.iter().filter(|(r, _)| *r == id).map(|(_, v)| v.clone()));
            if keep.is_empty() { dirs.push(s); } else { dirs.extend(self.store(&id).stale(&keep)?); }
        }
        failed.extend(locks.iter().filter_map(|p| crate::remove_file(p).err().map(|e| format!("{}: {e}", p.display()))));
        failed.extend(dirs.iter().filter_map(|p| crate::remove_dir(p).err().map(|e| format!("{}: {e}", p.display()))));
        if failed.is_empty() { Ok(()) } else { Err(format!("some files could not be removed: {}", failed.join("; "))) }
    }
}

/// One installed feature version that the next lock must cover. `pinned`: the member pinned
/// it (Versions list), so it keeps the module versions of its newest kept lock (rollback).
#[derive(Clone, Debug)]
pub struct Want {
    pub manifest: Manifest,
    pub sha256: String,
    pub pinned: bool,
}

/// Why a feature is not in a lock (the card shows it; the host then refuses the feature).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub feature: String,
    pub text: String,
}

/// What a selection may take: one rule for consent, downloads and the lock (Sol r1 S1, S2). The
/// catalog and the time for its freshness, the revocations that the app received
/// (Root::known_revoked; None: unknown, a damaged watermark, so no module is allowed), and the
/// kept locks, newest first (bindings, pins, the yank rule).
#[derive(Clone, Copy, Debug)]
pub struct Rules<'a> {
    pub catalog: Option<&'a Catalog>,
    pub now: u64,
    pub revoked: Option<&'a [Revoked]>,
    pub kept: &'a [Lock],
    /// The app's version and channel (client contract item 6). None: no release filter (the host
    /// and the tests); the app always gives it, so its locks hold only versions that it takes.
    pub release: Option<&'a crate::release::Release>,
}

impl Rules<'_> {
    /// Why no version is allowed when none is: unknown revocations, or old metadata.
    fn why(&self) -> &'static str {
        if self.revoked.is_none() {
            "; the module files are damaged, so the revocations are unknown (Feature Machine restores them at the next module list load)"
        } else if !self.catalog.is_some_and(|c| c.fresh(self.now)) {
            "; the module list is out of date or not loaded (connect to the internet)"
        } else {
            ""
        }
    }

    /// True when `m` may give maps to `w`. Never when revoked. With a fresh catalog: a version
    /// that is not yanked, or a yanked one that the current lock gives to this feature. With no
    /// fresh catalog: only a version that a kept lock binds to this exact feature version and
    /// package (Sol r1 S1: stale metadata keeps the bindings and makes no new selection).
    fn allows(&self, w: &Want, m: &ModuleManifest) -> bool {
        let same = |x: &Revoked| x.id == m.id && x.version == m.version;
        let Some(revoked) = self.revoked else { return false };
        // Before the lock exceptions: after a switch to Stable or to an older app, a kept lock
        // does not keep a Preview version or one that needs a newer app (items 6 and 9). The
        // channel is on the catalog entry (also a stale catalog); the store keeps listed ones only.
        if let Some(r) = self.release {
            let channel = self.catalog.and_then(|c| c.entry(&m.id, &m.version)).map(|e| e.channel);
            if r.refuses(channel.unwrap_or_default(), m.min_app.as_deref()).is_some() { return false; }
        }
        if revoked.iter().any(same) || self.catalog.is_some_and(|c| c.revoked.iter().any(same)) { return false; }
        let gives = |f: &LockedFeature| f.maps.iter().any(|x| x.module.as_deref() == Some(m.id.as_str()) && x.module_version.as_deref() == Some(m.version.as_str()));
        match self.catalog.filter(|c| c.fresh(self.now)) {
            Some(c) => !c.entry(&m.id, &m.version).is_some_and(|e| e.yanked) || self.kept.first().and_then(|l| l.feature(&w.manifest.id)).is_some_and(gives),
            None => self.kept.iter().filter_map(|l| l.feature(&w.manifest.id)).filter(|f| f.version == w.manifest.version && f.sha256 == w.sha256).any(gives),
        }
    }
}

/// The module version of `m` that gives requirement `r` for a game of the feature: the
/// interface and map in range.
fn fits(m: &ModuleManifest, r: &Requirement, games: &[Game]) -> Option<Provided> {
    let range = semver::VersionReq::parse(&r.versions).ok()?;
    let game = m.games.iter().any(|g| games.contains(g));
    m.provides.iter().find(|p| p.interface == r.interface && p.map.id == r.map && semver::Version::parse(&p.version).is_ok_and(|v| range.matches(&v)) && game).cloned()
}

fn newest(list: &mut [ModuleManifest]) {
    list.sort_by(|a, b| semver::Version::parse(&b.version).ok().cmp(&semver::Version::parse(&a.version).ok()));
}

/// Per module id: the versions that every requirement of one feature on it accepts, newest first.
type Candidates = BTreeMap<String, Vec<ModuleManifest>>;

/// The module versions that a feature's requirements select from `pool`: for each module id,
/// the versions that every requirement on it accepts, newest first. Err: a requirement with no
/// candidate (`why`: Rules::why, so the text says the cause), or two providers.
fn candidates(w: &Want, pool: &[ModuleManifest], ok: impl Fn(&ModuleManifest) -> bool, why: &str) -> Result<Candidates, String> {
    let mut by_module = Candidates::new();
    for r in &w.manifest.requires {
        let mut found: Vec<ModuleManifest> = pool.iter().filter(|m| fits(m, r, &w.manifest.games).is_some() && ok(m)).cloned().collect();
        let ids: BTreeSet<&str> = found.iter().map(|m| m.id.as_str()).collect();
        if ids.len() > 1 {
            return Err(format!("{} {}: two modules provide map {} ({}); a choice between providers is not supported yet", w.manifest.id, w.manifest.version, r.map, ids.into_iter().collect::<Vec<_>>().join(", ")));
        }
        let Some(id) = ids.into_iter().next().map(String::from) else {
            return Err(format!("{} {}: no current version of a required module gives map {} ({} {}){why}", w.manifest.id, w.manifest.version, r.map, r.interface, r.versions));
        };
        newest(&mut found);
        let slot = by_module.entry(id).or_insert_with(|| found.clone());
        slot.retain(|m| found.iter().any(|f| f.version == m.version));
    }
    Ok(by_module)
}

/// The one resolver (plan 8.1, Sol r1 S2). Per feature: the versions in `pool` that its
/// requirements accept and that `rules` and `ok` allow. Then one version per module id for all
/// features (M3, strict): the newest, or the version of a pinned feature's newest kept lock
/// with the same feature version and bytes (rollback). A feature that cannot resolve gets a
/// Problem. build_lock and plan both use it, so consent shows what the lock selects.
fn choose(wants: &[Want], pool: &[ModuleManifest], rules: &Rules, ok: impl Fn(&ModuleManifest) -> bool) -> (BTreeMap<String, ModuleManifest>, Vec<Problem>) {
    let mut problems = vec![];
    let mut per: Vec<(&Want, Candidates)> = vec![];
    for w in wants {
        match candidates(w, pool, |m| rules.allows(w, m) && ok(m), rules.why()) {
            Ok(c) => per.push((w, c)),
            Err(text) => problems.push(Problem { feature: w.manifest.id.clone(), text }),
        }
    }
    let mut chosen: BTreeMap<String, ModuleManifest> = BTreeMap::new();
    let ids: BTreeSet<String> = per.iter().flat_map(|(_, c)| c.keys().cloned()).collect();
    for id in ids {
        let users: Vec<&(&Want, Candidates)> = per.iter().filter(|(_, c)| c.contains_key(&id)).collect();
        let mut common: Vec<ModuleManifest> = users[0].1[&id].clone();
        for u in &users[1..] { common.retain(|m| u.1[&id].iter().any(|x| x.version == m.version)); }
        let pins: BTreeSet<String> = users.iter().filter(|(w, _)| w.pinned).filter_map(|(w, _)| {
            let old = rules.kept.iter().filter_map(|l| l.feature(&w.manifest.id)).find(|f| f.version == w.manifest.version && f.sha256 == w.sha256)?;
            old.maps.iter().find(|m| m.module.as_deref() == Some(id.as_str()))?.module_version.clone()
        }).collect();
        let pick = match pins.len() {
            0 => common.first().cloned(),
            1 => common.iter().find(|m| pins.contains(&m.version)).or(common.first()).cloned(),
            _ => None,
        };
        match pick {
            Some(m) => { chosen.insert(id, m); }
            None => {
                let names: Vec<String> = users.iter().map(|(w, _)| format!("{} {}", w.manifest.id, w.manifest.version)).collect();
                for (w, _) in &users {
                    problems.push(Problem { feature: w.manifest.id.clone(), text: format!("{} cannot use one version of module {id} with {}", w.manifest.id, names.join(" and ")) });
                }
            }
        }
    }
    (chosen, problems)
}

/// The module files as one read under one writer hold gives them (Root::snapshot, Sol r11):
/// the only input of a decision (build_lock, plan). It cannot outlive the hold (`'w`) and has
/// no copy, so a decision without the writer, or from an old read, does not compile. The UI
/// takes a View copy (view), which no decision takes.
pub struct Snapshot<'w> {
    catalog: Option<Catalog>,
    catalog_error: Option<String>,
    revoked: Option<Vec<Revoked>>,
    seen: u64,
    consent: Consent,
    kept: Vec<Lock>,
    stored: Vec<(ModuleManifest, String)>,
    release: Option<crate::release::Release>,
    _hold: std::marker::PhantomData<&'w Writer>,
}

impl Root {
    /// Reads the module files as the writer `w` (Snapshot). `key`: the feed key of the cached
    /// catalog. Err: consent, a kept lock or the store does not read. A damaged cache or
    /// watermark is no error: the snapshot has no catalog and no revocations (so no new
    /// selection is made), and catalog_error says why.
    pub fn snapshot<'w>(&self, w: &'w Writer, key: &str) -> Result<Snapshot<'w>, String> {
        self.owns(w)?;
        let catalog = self.cached_catalog(key);
        let (consent, kept, mut stored) = (self.consent()?, self.kept()?, self.all_stored()?);
        // Sol r1 B2: a stored version counts only with the manifest and digest of the signed
        // catalog. Else it is damaged: plan downloads it again and add_package repairs it. With
        // no catalog the host refuses module maps anyway, so the store and the kept bindings
        // stay as they are (Sol r2 N1: no empty lock while the metadata is missing).
        if let Some(c) = catalog.as_ref().ok().and_then(Option::as_ref) {
            stored.retain(|(m, sha)| c.entry(&m.id, &m.version).is_some_and(|e| e.manifest == *m && e.sha256 == *sha));
        }
        Ok(Snapshot { catalog_error: catalog.as_ref().err().cloned(), catalog: catalog.ok().flatten(), revoked: self.known_revoked().ok(), seen: self.seen_time(), consent, kept, stored, release: None, _hold: std::marker::PhantomData })
    }
}

impl Snapshot<'_> {
    pub fn consent(&self) -> &Consent {
        &self.consent
    }

    /// Why the cached catalog did not read (a damaged cache or watermark), if it did not.
    pub fn catalog_error(&self) -> Option<&str> {
        self.catalog_error.as_deref()
    }

    /// A copy for the UI view only (View::view_plan).
    pub fn view(&self) -> View {
        View { catalog: self.catalog.clone(), revoked: self.revoked.clone(), seen: self.seen, consent: self.consent.clone(), kept: self.kept.clone(), stored: self.stored.clone(), release: self.release.clone() }
    }

    /// The rules at the effective time: the higher of the PC clock `now` and the highest
    /// authenticated feed time (Sol r4 R4-2, plan 8.1 time model).
    fn rules(&self, now: u64) -> Rules<'_> {
        Rules { catalog: self.catalog.as_ref(), now: now.max(self.seen), revoked: self.revoked.as_deref(), kept: &self.kept, release: self.release.as_ref() }
    }

    /// The app's release for the rules of this snapshot (item 6).
    pub fn set_release(&mut self, release: crate::release::Release) {
        self.release = Some(release);
    }
}

/// A copy of a Snapshot for the UI (cards, the consent dialog). No decision takes it.
#[derive(Clone, Debug, Default)]
pub struct View {
    pub catalog: Option<Catalog>,
    pub revoked: Option<Vec<Revoked>>,
    pub seen: u64,
    pub consent: Consent,
    pub kept: Vec<Lock>,
    pub stored: Vec<(ModuleManifest, String)>,
    pub release: Option<crate::release::Release>,
}

impl View {
    /// `plan` from the view, for what the UI shows only. A decision uses plan (Snapshot).
    pub fn view_plan(&self, others: &[Want], target: &Want, now: u64) -> Result<Plan, String> {
        let rules = Rules { catalog: self.catalog.as_ref(), now: now.max(self.seen), revoked: self.revoked.as_deref(), kept: &self.kept, release: self.release.as_ref() };
        plan_from(&self.stored, others, target, &rules)
    }
}

/// Builds the next lock from snapshot `snap` (build_from) at the PC time `now`.
pub fn build_lock(snap: &Snapshot, wants: &[Want], now: u64) -> Result<(Lock, Vec<Problem>), String> {
    build_from(&snap.stored, wants, &snap.rules(now), &snap.consent)
}

/// Builds the next lock from the store `stored` (plan 8.1 resolver, choose). Every feature in
/// `wants` gets its maps: required ones from the chosen module versions, the others embedded.
/// Only versions with member consent count, so a refusal selects nothing. A feature that cannot
/// resolve is left out with its Problem. `rules.catalog` gives the provenance revision.
fn build_from(stored: &[(ModuleManifest, String)], wants: &[Want], rules: &Rules, consent: &Consent) -> Result<(Lock, Vec<Problem>), String> {
    let pool: Vec<ModuleManifest> = stored.iter().map(|(m, _)| m.clone()).collect();
    let (chosen, problems) = choose(wants, &pool, rules, |m| consent.covers(m));
    let failed: BTreeSet<String> = problems.iter().map(|p| p.feature.clone()).collect();
    let mut lock = Lock { schema: SCHEMA, catalog_revision: rules.catalog.map(|c| c.revision), catalog_view: rules.catalog.and_then(|c| c.view.clone()), features: vec![], modules: vec![] };
    for w in wants.iter().filter(|w| !failed.contains(&w.manifest.id)) {
        let required: BTreeSet<&str> = w.manifest.requires.iter().map(|r| r.map.as_str()).collect();
        let mut maps: Vec<LockedMap> = w.manifest.memory_maps.iter().filter(|p| !required.contains(p.id.as_str())).map(|p| LockedMap {
            id: p.id.clone(), source: MapSource::Embedded, version: p.version.clone(), sha256: p.sha256.clone(), interface: w.manifest.map_contracts.get(&p.id).cloned(), interface_version: None, module: None, module_version: None,
        }).collect();
        for r in &w.manifest.requires {
            let Some(m) = chosen.values().find(|m| fits(m, r, &w.manifest.games).is_some()) else { continue };
            let p = fits(m, r, &w.manifest.games).expect("chosen fits");
            maps.push(LockedMap { id: p.map.id, source: MapSource::Module, version: p.map.version, sha256: p.map.sha256, interface: Some(p.interface), interface_version: Some(p.version), module: Some(m.id.clone()), module_version: Some(m.version.clone()) });
        }
        maps.sort_by(|a, b| a.id.cmp(&b.id));
        lock.features.push(LockedFeature { id: w.manifest.id.clone(), version: w.manifest.version.clone(), sha256: w.sha256.clone(), maps });
    }
    for m in chosen.values().filter(|m| lock.features.iter().any(|f| f.maps.iter().any(|x| x.module.as_deref() == Some(m.id.as_str())))) {
        // Each chosen module comes from `stored` (the pool), so the digest is always found.
        let sha = stored.iter().find(|(s, _)| s.id == m.id && s.version == m.version).map(|(_, d)| d.clone()).unwrap_or_default();
        lock.modules.push(LockedModule { id: m.id.clone(), version: m.version.clone(), sha256: sha, powers: m.powers.clone() });
    }
    lock.features.sort_by(|a, b| a.id.cmp(&b.id));
    Ok((lock, problems))
}

/// The module versions that one feature version gets, and the ones to download first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub modules: Vec<ModuleManifest>,
    pub download: Vec<CatalogEntry>,
}

/// What `target` gets (Sol r1 S2: the same choose as build_lock, so consent, downloads and the
/// lock agree, pins included): choose over the store (`stored`, Root::all_stored) and the
/// catalog, with the other installed features (`others`; the target's id is replaced) and no
/// consent filter (consent asks for the result). Err: why the target cannot resolve.
pub fn plan(snap: &Snapshot, others: &[Want], target: &Want, now: u64) -> Result<Plan, String> {
    plan_from(&snap.stored, others, target, &snap.rules(now))
}

/// plan over the store `stored` with `rules` (plan, View::view_plan).
fn plan_from(stored: &[(ModuleManifest, String)], others: &[Want], target: &Want, rules: &Rules) -> Result<Plan, String> {
    if target.manifest.requires.is_empty() { return Ok(Plan::default()); }
    let in_store = |m: &ModuleManifest| stored.iter().any(|(s, _)| s.id == m.id && s.version == m.version);
    let mut pool: Vec<ModuleManifest> = stored.iter().map(|(m, _)| m.clone()).collect();
    pool.extend(rules.catalog.map_or(&[][..], |c| &c.modules).iter().map(|e| e.manifest.clone()).filter(|m| !in_store(m)));
    let wants: Vec<Want> = others.iter().filter(|w| w.manifest.id != target.manifest.id).cloned().chain([target.clone()]).collect();
    let (chosen, problems) = choose(&wants, &pool, rules, |_| true);
    if let Some(p) = problems.into_iter().find(|p| p.feature == target.manifest.id) { return Err(p.text); }
    let modules: Vec<ModuleManifest> = chosen.into_values().filter(|m| target.manifest.requires.iter().any(|r| fits(m, r, &target.manifest.games).is_some())).collect();
    let download = modules.iter().filter(|m| !in_store(m)).filter_map(|m| rules.catalog.and_then(|c| c.entry(&m.id, &m.version)).cloned()).collect();
    Ok(Plan { modules, download })
}

/// The host error when no signed catalog was ever received (Sol r1 B1).
const NO_CATALOG: &str = "no signed module catalog is on this computer, so revocations are unknown; open Feature Machine with a network connection";

/// What the host knows at its start: the selected lock, and the revocations that the app
/// received (or why it could not read them). Read once per game process (SessionCell).
#[derive(Clone, Debug)]
pub struct Session {
    root: Root,
    lock: Result<Option<Lock>, String>,
    revoked: Result<Vec<Revoked>, String>,
    /// The module entries of the signed catalog: the only trusted manifests (Sol r1 B2).
    signed: Vec<CatalogEntry>,
    /// The feed key, for the feed package of a module writer (Sol r1 B1).
    key: String,
}

impl Session {
    /// Session::load_with with the host wait (READ_WAIT).
    pub fn load(root: &Root, key: &str) -> Session {
        Session::load_with(root, key, READ_WAIT)
    }

    /// Reads the cached catalog (checked with `key`, the feed key), the watermark, the pointer
    /// and the lock as one snapshot under the shared lock (Sol r2 R2-5; at most `wait` for a
    /// writer). No cache, a damaged one, one below the watermark or a damaged watermark: the
    /// revocations are unknown, so module maps refuse (Sol r1 B1, r2 R2-1).
    pub fn load_with(root: &Root, key: &str, wait: std::time::Duration) -> Session {
        let guard = match root.read_lock(wait) {
            Ok(g) => g,
            Err(e) => return Session { root: root.clone(), lock: Err(e.clone()), revoked: Err(e), signed: vec![], key: key.to_string() },
        };
        let mut signed = vec![];
        let revoked = match (root.cached_catalog(key), root.known_revoked()) {
            (Ok(Some(c)), Ok(known)) => {
                signed = c.modules;
                let mut list = c.revoked;
                add_revoked(&mut list, &known);
                Ok(list)
            }
            (Err(e), _) | (_, Err(e)) => Err(e),
            (Ok(None), Ok(_)) => Err(NO_CATALOG.to_string()),
        };
        let lock = root.current();
        drop(guard);
        Session { root: root.clone(), lock, revoked, signed, key: key.to_string() }
    }

    /// No data folder (or a test host): no lock and no known revocations, so every feature
    /// that requires a module is refused and the others keep the 6c pin rule.
    pub fn unavailable(reason: &str) -> Session {
        Session { root: Root::new(PathBuf::new()), lock: Err(reason.to_string()), revoked: Err(reason.to_string()), signed: vec![], key: String::new() }
    }

    pub fn lock(&self) -> Option<&Lock> {
        self.lock.as_ref().ok().and_then(Option::as_ref)
    }

    /// The host log lines of this session (P1 live gaps, 2026-10-27): the lock digest or why
    /// there is no lock, and each locked map with its digest and source (the module folder, or
    /// embedded), so the log shows where each map comes from and why a module map refuses.
    pub fn describe(&self) -> Vec<String> {
        let l = match &self.lock {
            Err(e) => return vec![format!("module lock: none ({e})")],
            Ok(None) => return vec!["module lock: none (no lock selected yet)".into()],
            Ok(Some(l)) => l,
        };
        let mut out = vec![format!("module lock {} ({} features, {} modules)", l.digest(), l.features.len(), l.modules.len())];
        for f in &l.features {
            for x in &f.maps {
                let from = match (&x.source, x.module.as_deref(), x.module_version.as_deref()) {
                    (MapSource::Module, Some(id), Some(v)) => match self.root.version_dir(id, v) {
                        Some(dir) => format!("module {id} {v} at {}", dir.display()),
                        None => format!("module {id} {v} (not complete in the store)"),
                    },
                    (MapSource::Module, _, _) => "module (no module named)".into(),
                    (MapSource::Embedded, _, _) => "embedded".into(),
                };
                out.push(format!("module lock map {} {}: {} {} sha256 {} from {from}", f.id, f.version, x.id, x.version, x.sha256));
            }
        }
        if let Err(e) = &self.revoked { out.push(format!("module revocations: unknown ({e}); required maps refuse")); }
        out
    }

    /// The verified maps of one installed feature version (`dir`, manifest `m`) with the pin of
    /// each, for this game session. A required map comes only from its locked module: any
    /// failure refuses the feature, and its embedded copy is never read (plan 6.2, no silent
    /// fallback). The other maps keep the 6c pin check; a lock that names the feature must
    /// give them the same pins.
    pub fn feature_maps(&self, dir: &Path, m: &Manifest) -> Result<Vec<(MapDependency, Signed)>, String> {
        let lock = match &self.lock {
            Ok(l) => l.as_ref(),
            Err(e) if !m.requires.is_empty() => return Err(format!("{}: {e}", m.id)),
            Err(_) => None,
        };
        // No requirement and no map: nothing to check. Else the stamp must read; a missing one
        // (an older install) matches no lock entry, so required maps refuse (Sol r7 sweep).
        if m.requires.is_empty() && m.memory_maps.is_empty() { return Ok(vec![]); }
        let stamp = match std::fs::read_to_string(dir.join(crate::PACKAGE_STAMP)) {
            Ok(s) => s.trim().to_string(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("{}: {}", m.id, at(&dir.join(crate::PACKAGE_STAMP), e))),
        };
        let locked = lock.and_then(|l| l.feature(&m.id)).filter(|f| f.version == m.version && f.sha256 == stamp);
        let (Some(lock), Some(locked)) = (lock, locked) else {
            if !m.requires.is_empty() {
                return Err(format!("{} {}: not in the session lock; it starts at the next game start after Feature Machine prepares its modules", m.id, m.version));
            }
            if m.memory_maps.is_empty() { return Ok(vec![]); }
            return memory::verify_directory(dir, m).map(|maps| m.memory_maps.iter().cloned().zip(maps).collect());
        };
        let required: BTreeSet<&str> = m.requires.iter().map(|r| r.map.as_str()).collect();
        let embedded = Manifest { memory_maps: m.memory_maps.iter().filter(|p| !required.contains(p.id.as_str())).cloned().collect(), map_contracts: m.map_contracts.iter().filter(|(k, _)| !required.contains(k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect(), ..m.clone() };
        let mut pins: Vec<(String, String, String)> = embedded.memory_maps.iter().map(|p| (p.id.clone(), p.version.clone(), p.sha256.clone())).collect();
        let lock_pins: Vec<(String, String, String)> = locked.maps.iter().filter(|x| x.source == MapSource::Embedded).map(|x| (x.id.clone(), x.version.clone(), x.sha256.clone())).collect();
        pins.sort();
        if pins != lock_pins { return Err(format!("{} {}: its embedded maps differ from the session lock", m.id, m.version)); }
        if memory::module_writer(m) { memory::verify_feed_package(dir, m, &stamp, &self.key)?; }
        let mut out = vec![];
        if !embedded.memory_maps.is_empty() {
            let maps = memory::verify_directory(dir, &embedded)?;
            out.extend(embedded.memory_maps.iter().cloned().zip(maps));
        }
        for r in &m.requires {
            let x = locked.maps.iter().find(|x| x.id == r.map && x.source == MapSource::Module).ok_or_else(|| format!("{}: map {} has no module in the session lock", m.id, r.map))?;
            out.push(self.module_map(lock, m, r, x)?);
        }
        Ok(out)
    }

    /// One required map from its locked module version: not revoked (unknown revocations
    /// refuse), complete in the store, and equal to the lock (Sol r1 S3): the module package
    /// digest and powers of the lock's module entry, the map pin, the interface and its
    /// version, the requirement range, the game, and the camera rules.
    fn module_map(&self, lock: &Lock, m: &Manifest, r: &Requirement, x: &LockedMap) -> Result<(MapDependency, Signed), String> {
        let (id, version) = (x.module.as_deref().unwrap_or(""), x.module_version.as_deref().unwrap_or(""));
        match &self.revoked {
            Err(e) => return Err(format!("{}: the module catalog cannot be read ({e}); open Feature Machine", m.id)),
            Ok(list) => if let Some(v) = list.iter().find(|v| v.id == id && v.version == version) {
                return Err(format!("{}: module {id} {version} was revoked ({}); it does not start", m.id, v.reason));
            },
        }
        let entry = lock.module(id).filter(|e| e.version == version).ok_or_else(|| format!("{}: module {id} {version} is not a module of the session lock", m.id))?;
        let (manifest, sha256) = self.root.stored(id, version)?;
        if entry.sha256 != sha256 {
            return Err(format!("{}: module {id} {version} in the store is not the package of the session lock", m.id));
        }
        // Sol r1 B2: the stored manifest is a file on disk; only the signed catalog entry is
        // trusted, so the manifest and digest must be the ones that the feed signed.
        if !self.signed.iter().any(|e| e.manifest == manifest && e.sha256 == sha256) {
            return Err(format!("{}: module {id} {version} in the store differs from the signed module catalog", m.id));
        }
        if entry.powers.iter().collect::<BTreeSet<_>>() != manifest.powers.iter().collect::<BTreeSet<_>>() {
            return Err(format!("{}: module {id} {version} has other powers than the session lock approved", m.id));
        }
        let provided = fits(&manifest, r, &m.games)
            .filter(|p| p.map.version == x.version && p.map.sha256 == x.sha256 && x.interface.as_deref() == Some(p.interface.as_str()) && x.interface_version.as_deref() == Some(p.version.as_str()))
            .ok_or_else(|| format!("{}: module {id} {version} does not give map {} as the session lock says", m.id, r.map))?;
        let dir = self.root.version_dir(id, version).ok_or("module version is not complete")?;
        let path = dir.join(crate::feature::safe_rel(&memory::map_path(&provided.map)).map_err(|e| e.to_string())?);
        if std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?.len() > LIMIT { return Err("module map exceeds the size limit".into()); }
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let map = check_map(&manifest, &provided, &bytes)?;
        if !m.games.iter().any(|g| game_name(g) == map.game()) { return Err(format!("{}: map {} is for another game", m.id, r.map)); }
        Ok((provided.map, map))
    }
}

/// The host's session state, read once per game process (plan 6.2: a change applies at the next
/// game start). The first `get` loads it; every later one returns the same value.
#[derive(Default)]
pub struct SessionCell(OnceLock<Session>);

impl SessionCell {
    pub const fn new() -> Self {
        Self(OnceLock::new())
    }

    pub fn get(&self, load: impl FnOnce() -> Session) -> &Session {
        self.0.get_or_init(load)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_signing::TestKey;

    const NOW: u64 = 1_800_000_000;

    use crate::test_signing::camera_locator as locator;

    fn manifest(version: &str, map: &[u8], map_version: &str) -> ModuleManifest {
        let mut m = TestKey::default().module_entry("p42.test-camera", version, 0.0).0.manifest;
        m.provides[0].map = MapDependency { id: "cam-locator".into(), version: map_version.into(), sha256: memory::digest(map) };
        m
    }

    fn package(m: &ModuleManifest, map: &[u8]) -> Vec<u8> {
        crate::tests::zip_of(&[(MANIFEST, &serde_json::to_vec(m).unwrap()), (&memory::map_path(&m.provides[0].map), map)])
    }

    /// A module version with its package and signed entry (map version = module version).
    fn published(key: &TestKey, version: &str, near: f32) -> (CatalogEntry, Vec<u8>) {
        key.module_entry("p42.test-camera", version, near)
    }

    fn catalog(revision: u64, modules: Vec<CatalogEntry>, revoked: Vec<Revoked>) -> Catalog {
        Catalog { schema: 1, revision, issued: NOW - 10, expires: NOW + 86_400, modules, revoked, view: None }
    }

    fn signed(key: &TestKey, c: &Catalog) -> (Vec<u8>, String) {
        key.sign_catalog(c)
    }

    // Release-admin item 6 on the real module types: a stable entry writes no channel, a
    // preview entry keeps its channel through a signed catalog, an unknown channel reads as
    // Unknown, and a bad min_app fails check().
    #[test]
    fn release_fields_on_the_module_catalog_types() {
        let key = TestKey::default();
        let stable = published(&key, "1.0.0", 0.005).0;
        let json = serde_json::to_string(&stable).unwrap();
        assert!(!json.contains("channel") && !json.contains("min_app"), "{json}");
        let preview = CatalogEntry { channel: crate::release::Channel::Preview, ..published(&key, "1.1.0", 0.005).0 };
        let c = catalog(3, vec![stable.clone(), preview.clone()], vec![]);
        let (bytes, sig) = signed(&key, &c);
        let read = Catalog::verify(&bytes, &sig, &key.public()).unwrap();
        assert_eq!((read.modules[0].channel, read.modules[1].channel), (crate::release::Channel::Stable, crate::release::Channel::Preview));
        let nightly = serde_json::to_string(&preview).unwrap().replace(r#""channel":"preview""#, r#""channel":"nightly""#);
        assert_eq!(serde_json::from_str::<CatalogEntry>(&nightly).unwrap().channel, crate::release::Channel::Unknown);
        let m = ModuleManifest { min_app: Some("0.2.0-preview.1".into()), ..stable.manifest.clone() };
        m.check().unwrap();
        assert!(ModuleManifest { min_app: Some("next".into()), ..m }.check().is_err());
    }

    /// The rules of a lock build or plan at NOW: `c`, the received revocations and kept locks.
    fn rules<'a>(c: Option<&'a Catalog>, revoked: &'a [Revoked], kept: &'a [Lock]) -> Rules<'a> {
        Rules { catalog: c, now: NOW, revoked: Some(revoked), kept, release: None }
    }

    fn writer(root: &Root) -> Writer {
        root.writer().unwrap()
    }

    /// A camera-only depth feature that requires the locator map, with an embedded copy (`embedded`).
    fn feature(version: &str, embedded: Option<&[u8]>) -> Manifest {
        let mut m: Manifest = serde_json::from_value(serde_json::json!({ "id": "mirage", "name": "Heat mirage", "version": version, "description": "d", "api": 5, "games": ["msfs2024"],
            "capabilities": ["render_pass", "render_depth"], "requires": [{ "interface": memory::LOCATOR_CONTRACT, "versions": "^1.0", "map": "cam-locator" }] })).unwrap();
        if let Some(b) = embedded {
            m.memory_maps = vec![MapDependency { id: "cam-locator".into(), version: "1.0.0".into(), sha256: memory::digest(b) }];
        }
        m
    }

    /// An installed feature version folder: feature.json, the stamp and the embedded map.
    fn installed(dir: &Path, m: &Manifest, embedded: Option<&[u8]>) -> Want {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(crate::FEATURE_MANIFEST), serde_json::to_vec(m).unwrap()).unwrap();
        std::fs::write(dir.join(crate::PACKAGE_STAMP), "f".repeat(64)).unwrap();
        if let (Some(b), Some(p)) = (embedded, m.memory_maps.first()) {
            let path = dir.join(memory::map_path(p));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b).unwrap();
        }
        Want { manifest: m.clone(), sha256: "f".repeat(64), pinned: false }
    }

    fn approved(m: &ModuleManifest) -> Consent {
        let mut c = Consent::default();
        c.approve(m);
        c
    }

    fn session(root: &Root, key: &TestKey) -> Session {
        Session::load(root, &key.public())
    }

    fn near(maps: &[(MapDependency, Signed)]) -> f32 {
        match &maps[0].1 { Signed::Locator(l) => l.near[0], Signed::Map(_) => panic!("a locator") }
    }

    #[test]
    fn manifest_rules() {
        let map = locator("1.0.0", 0.005);
        let m = manifest("1.0.0", &map, "1.0.0");
        m.check().unwrap();
        assert_eq!((m.provider(), m.provider_name().as_str()), ("p42", "Parallel 42"));
        for bad in ["p42", "p42.", ".x", "p42.A", "p42.x.y", "p42/x"] {
            assert!(ModuleManifest { id: bad.into(), ..m.clone() }.check().is_err(), "{bad}");
        }
        assert!(ModuleManifest { license: "MIT".into(), ..m.clone() }.check().is_err(), "M5 license");
        assert!(ModuleManifest { powers: vec![], ..m.clone() }.check().is_err(), "a camera map needs the power");
        assert!(ModuleManifest { kind: Kind::Unknown, ..m.clone() }.check().is_err());
        assert!(ModuleManifest { source: Source { commit: "x".into(), ..m.source.clone() }, ..m.clone() }.check().is_err());
        let mut scalar = m.clone();
        scalar.provides[0].interface = "anchored-scalar-v1".into();
        assert!(scalar.check().is_err(), "camera contracts only");
        let mut value = serde_json::to_value(&m).unwrap();
        value["powers"] = serde_json::json!(["game_memory_read", "game_files_write"]);
        let unknown: ModuleManifest = serde_json::from_value(value).unwrap();
        assert!(unknown.check().unwrap_err().contains("does not know"));
        let extra = ModuleManifest { powers: vec![Power::GameMemoryRead, Power::GameMemoryWrite], ..m.clone() };
        assert!(extra.check().unwrap_err().contains("powers must be"), "consent never shows a power that no map uses");
        // The feature side: requires only for a camera-only depth feature.
        feature("0.3.0", None).check().unwrap();
        let mut writer = feature("0.3.0", None);
        writer.capabilities.extend([fm_feature_sdk::Capability::VarsRead, fm_feature_sdk::Capability::VarsMemory]);
        assert!(writer.check().is_err(), "no module map for a memory feature");
        let mut range = feature("0.3.0", None);
        range.requires[0].versions = "one".into();
        assert!(range.check().is_err());
        let mut twice = feature("0.3.0", None);
        twice.requires.push(twice.requires[0].clone());
        assert!(twice.check().is_err());
    }

    /// The setter module of test_signing: its manifest and map.
    fn setter_module() -> (ModuleManifest, Vec<u8>) {
        (TestKey::default().setter_module_entry("p42.test-wetness", "test-wetness").0.manifest, crate::test_signing::setter_map("test-wetness"))
    }

    // Ground wetness plan slice 1: a setter map comes only with the write power and the setter
    // contract, and a camera map never with the write power alone.
    #[test]
    fn a_setter_module_needs_the_write_power_and_its_contract() {
        let (m, map) = setter_module();
        m.check().unwrap();
        assert_eq!(inspect(&package(&m, &map)).unwrap(), m);
        assert!(ModuleManifest { powers: vec![Power::GameMemoryRead], ..m.clone() }.check().is_err(), "a setter map needs the write power");
        assert!(ModuleManifest { powers: vec![Power::GameMemoryRead, Power::GameMemoryWrite], ..m.clone() }.check().is_err(), "no extra power");
        let mut v1 = m.clone();
        v1.provides[0].interface = "mirrored-setter-v1".into();
        assert!(v1.check().is_err(), "exact-build contracts stay embedded");
        let mut as_camera = m.clone();
        as_camera.provides[0].interface = memory::CAMERA_CONTRACT.into();
        as_camera.powers = vec![Power::GameMemoryRead];
        assert!(inspect(&package(&as_camera, &map)).unwrap_err().contains("contract"), "a setter map is not a camera map");
        let camera = locator("1.0.0", 0.005);
        let mut write_camera = manifest("1.0.0", &camera, "1.0.0");
        write_camera.powers = vec![Power::GameMemoryWrite];
        assert!(write_camera.check().is_err(), "a camera map needs the read power");
        let mut setter_as_locator = write_camera.clone();
        setter_as_locator.provides[0].interface = memory::SETTER_V2_CONTRACT.into();
        assert!(inspect(&package(&setter_as_locator, &camera)).unwrap_err().contains("contract"), "a locator is not a setter map");
    }

    /// A memory writer whose map comes from a setter module (Wet and dry speed after the plan).
    fn writer_feature() -> Manifest {
        serde_json::from_value(serde_json::json!({ "id": "wet-dry", "name": "Wet and dry speed", "version": "0.3.0", "description": "d", "api": 5, "games": ["msfs2024"],
            "capabilities": ["vars_read", "vars_memory", "vars_write", "ui_app"], "ui": "index.html",
            "requires": [{ "interface": memory::SETTER_V2_CONTRACT, "versions": "^1.0", "map": "test-wetness" }] })).unwrap()
    }

    // Ground wetness plan slice 2: a writer requires setter maps only, with no embedded map, and
    // its package has no session record; the host gets the setter map from the locked module.
    #[test]
    fn a_memory_writer_takes_its_setter_map_from_a_module() {
        let w = writer_feature();
        w.check().unwrap();
        assert!(memory::module_writer(&w));
        let mut no_write = w.clone();
        no_write.capabilities.retain(|c| *c != fm_feature_sdk::Capability::VarsWrite);
        assert!(no_write.check().is_err(), "a setter requirement needs vars_write");
        let mut camera_req = w.clone();
        camera_req.requires[0].interface = memory::LOCATOR_CONTRACT.into();
        assert!(camera_req.check().is_err(), "a writer requires setter contracts only");
        let mut embedded = w.clone();
        embedded.memory_maps = vec![MapDependency { id: "other".into(), version: "1.0.0".into(), sha256: "a".repeat(64) }];
        assert!(embedded.check().is_err() || !memory::module_writer(&embedded), "embedded maps keep the session record path");
        let mut reader = feature("0.3.0", None);
        reader.requires[0].interface = memory::SETTER_V2_CONTRACT.into();
        assert!(reader.check().is_err(), "a camera feature cannot require a setter map");
        // The package: no session record, and one present is refused.
        let manifest = serde_json::to_vec(&w).unwrap();
        let plain = crate::tests::zip_of(&[(crate::FEATURE_MANIFEST, &manifest)]);
        memory::verify_install(&mut zip::ZipArchive::new(std::io::Cursor::new(plain)).unwrap(), "0.3.0").unwrap();
        let with_record = crate::tests::zip_of(&[(crate::FEATURE_MANIFEST, &manifest), (memory::SESSION, b"{}"), (memory::SIGNATURE, b"x")]);
        assert!(memory::verify_install(&mut zip::ZipArchive::new(std::io::Cursor::new(with_record)).unwrap(), "0.3.0").unwrap_err().contains("no session record"));
        // The lock and the host read.
        let key = TestKey::default();
        let (entry, bytes) = key.setter_module_entry("p42.test-wetness", "test-wetness");
        let (m, map) = (entry.manifest.clone(), crate::test_signing::setter_map("test-wetness"));
        let t = tempfile::tempdir().unwrap();
        let root = Root::new(t.path().join("modules"));
        let c = catalog(1, vec![entry.clone()], vec![]);
        root.add_package(&writer(&root), &entry, &bytes, &key.public()).unwrap();
        root.accept_catalog(&writer(&root), &signed(&key, &c).0, &signed(&key, &c).1, &key.public(), Some(NOW)).unwrap();
        let dir = t.path().join("feature");
        let want = Want { manifest: w.clone(), sha256: key.feed_install(&dir, &w), pinned: false };
        let (none, problems) = build_from(&root.all_stored().unwrap(), std::slice::from_ref(&want), &rules(Some(&c), &[], &[]), &Consent::default()).unwrap();
        assert!(none.features.is_empty() && none.modules.is_empty() && problems.len() == 1, "no consent for the write power: nothing selected");
        root.save_consent(&writer(&root), &approved(&m)).unwrap();
        let (lock, problems) = build_lock(&root.snapshot(&writer(&root), &key.public()).unwrap(), std::slice::from_ref(&want), NOW).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(lock.modules[0].powers, [Power::GameMemoryWrite]);
        root.select(&writer(&root), &lock).unwrap();
        let maps = session(&root, &key).feature_maps(&dir, &w).unwrap();
        assert_eq!(maps.len(), 1);
        assert_eq!((maps[0].0.id.as_str(), maps[0].0.sha256.as_str()), ("test-wetness", memory::digest(&map).as_str()));
        match &maps[0].1 { Signed::Map(x) => assert!(x.symbols[0].setter.is_some()), Signed::Locator(_) => panic!("a setter map") }
        // Sol r1 B1: the installed files must be the feed-signed package.
        let refused = |why: &str| { let e = session(&root, &key).feature_maps(&dir, &w).unwrap_err(); assert!(e.contains("install it again from the Feature Machine catalog"), "{why}: {e}"); };
        let lib = dir.join(crate::feature::parts::library());
        std::fs::write(&lib, b"changed").unwrap();
        refused("a changed library");
        std::fs::write(&lib, b"library").unwrap();
        session(&root, &key).feature_maps(&dir, &w).unwrap();
        let (kept, sig) = (std::fs::read(dir.join(memory::FEED_PACKAGE)).unwrap(), std::fs::read(dir.join(memory::FEED_SIGNATURE)).unwrap());
        std::fs::remove_file(dir.join(memory::FEED_PACKAGE)).unwrap();
        refused("no feed package (an unpack of a file)");
        std::fs::write(dir.join(memory::FEED_PACKAGE), &kept).unwrap();
        std::fs::write(dir.join(memory::FEED_SIGNATURE), TestKey::default().sign(&kept, "0.3.0")).unwrap();
        refused("a signature of another key");
        std::fs::write(dir.join(memory::FEED_SIGNATURE), &sig).unwrap();
        let stamp = std::fs::read(dir.join(crate::PACKAGE_STAMP)).unwrap();
        std::fs::remove_file(dir.join(crate::PACKAGE_STAMP)).unwrap();
        assert!(session(&root, &key).feature_maps(&dir, &w).unwrap_err().contains("not in the session lock"), "a missing stamp");
        std::fs::write(dir.join(crate::PACKAGE_STAMP), stamp).unwrap();
        session(&root, &key).feature_maps(&dir, &w).unwrap();
        // Sol r1 B2: a changed map with its pin changed in the stored manifest. The host refuses
        // it with the old lock, a new lock does not select it, and add_package repairs it.
        let store = root.version_dir("p42.test-wetness", "1.0.0").unwrap();
        let mut forged = m.clone();
        let other = crate::test_signing::setter_map("test-wetness").into_iter().chain(*b" ").collect::<Vec<u8>>();
        forged.provides[0].map.sha256 = memory::digest(&other);
        std::fs::write(store.join(memory::map_path(&forged.provides[0].map)), &other).unwrap();
        std::fs::write(store.join(MANIFEST), serde_json::to_vec(&forged).unwrap()).unwrap();
        assert!(session(&root, &key).feature_maps(&dir, &w).unwrap_err().contains("differs from the signed module catalog"));
        let (relock, problems) = build_lock(&root.snapshot(&writer(&root), &key.public()).unwrap(), std::slice::from_ref(&want), NOW).unwrap();
        assert!(relock.features.is_empty() && problems.len() == 1, "the forged version is not selected");
        root.add_package(&writer(&root), &entry, &bytes, &key.public()).unwrap();
        assert_eq!(root.stored("p42.test-wetness", "1.0.0").unwrap().0, m, "repaired");
        session(&root, &key).feature_maps(&dir, &w).unwrap();
        // Sol r2 N1: with the catalog cache gone, a new lock keeps the bindings of the kept lock.
        std::fs::remove_file(root.path().join("catalog.json")).unwrap();
        let (kept, problems) = build_lock(&root.snapshot(&writer(&root), &key.public()).unwrap(), std::slice::from_ref(&want), NOW).unwrap();
        assert!(problems.is_empty() && kept.features.len() == 1 && kept.modules.len() == 1, "{problems:?}");
    }

    #[test]
    fn a_package_holds_only_reviewed_files_and_matching_maps() {
        let key = TestKey::default();
        let (entry, bytes) = published(&key, "1.0.0", 0.005);
        assert_eq!(inspect(&bytes).unwrap(), entry.manifest);
        let map = locator("1.0.0", 0.005);
        let extra = crate::tests::zip_of(&[(MANIFEST, &serde_json::to_vec(&entry.manifest).unwrap()), (&memory::map_path(&entry.manifest.provides[0].map), &map), ("maps/other/1.0.0/map.json", &map)]);
        assert!(inspect(&extra).unwrap_err().contains("unreviewed file"));
        let top = crate::tests::zip_of(&[(MANIFEST, &serde_json::to_vec(&entry.manifest).unwrap()), (&memory::map_path(&entry.manifest.provides[0].map), &map), ("run.exe", b"x")]);
        assert!(inspect(&top).is_err(), "only module.json and maps/ at the top");
        let dir = tempfile::tempdir().unwrap();
        assert!(Install::module(dir.path()).unpack("1.0.0", std::io::Cursor::new(extra), false).is_err(), "the store applies the same rule");
        Install::module(dir.path()).unpack("1.0.0", std::io::Cursor::new(bytes), false).unwrap();
        assert!(Install::module(dir.path()).is_complete("1.0.0"));
    }

    // Done check 4: a bad digest or a wrong contract is refused, at the package, at the
    // download and in the host read of a stored module.
    #[test]
    fn a_bad_digest_or_contract_is_refused() {
        let key = TestKey::default();
        let (entry, bytes) = published(&key, "1.0.0", 0.005);
        let mut wrong_pin = entry.manifest.clone();
        wrong_pin.provides[0].map.sha256 = "b".repeat(64);
        assert!(inspect(&package(&wrong_pin, &locator("1.0.0", 0.005))).unwrap_err().contains("pinned digest"));
        let mut camera = entry.manifest.clone();
        camera.provides[0].interface = memory::CAMERA_CONTRACT.into();
        assert!(inspect(&package(&camera, &locator("1.0.0", 0.005))).unwrap_err().contains("contract"));
        let mut changed = bytes.clone();
        let last = changed.len() - 30;
        changed[last] ^= 1;
        assert!(verify_package(&changed, &entry, &key.public()).unwrap_err().contains("does not match"));
        assert!(verify_package(&bytes, &CatalogEntry { sha256: memory::digest(&changed), size: changed.len() as u64, ..entry.clone() }, &key.public()).is_err());
        assert!(verify_package(&bytes, &entry, &TestKey::default().public()).is_err(), "another key");
        verify_package(&bytes, &entry, &key.public()).unwrap();
        // The host read: a stored map file changed after the install fails its locked digest.
        let t = tempfile::tempdir().unwrap();
        let root = Root::new(t.path().join("modules"));
        let c = catalog(1, vec![entry.clone()], vec![]);
        root.add_package(&writer(&root), &entry, &bytes, &key.public()).unwrap();
        root.accept_catalog(&writer(&root), &signed(&key, &c).0, &signed(&key, &c).1, &key.public(), Some(NOW)).unwrap();
        let w = installed(&t.path().join("feature"), &feature("0.3.0", None), None);
        let (lock, problems) = build_from(&root.all_stored().unwrap(), std::slice::from_ref(&w), &rules(Some(&c), &[], &[]), &approved(&entry.manifest)).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        root.select(&writer(&root), &lock).unwrap();
        session(&root, &key).feature_maps(&t.path().join("feature"), &w.manifest).unwrap();
        let file = root.version_dir("p42.test-camera", "1.0.0").unwrap().join(memory::map_path(&entry.manifest.provides[0].map));
        std::fs::write(&file, locator("1.0.0", 0.006)).unwrap();
        assert!(session(&root, &key).feature_maps(&t.path().join("feature"), &w.manifest).unwrap_err().contains("pinned digest"));
        // A lock with a wrong contract for the map is refused too.
        let mut bad = lock.clone();
        bad.features[0].maps[0].interface = Some(memory::CAMERA_CONTRACT.into());
        std::fs::write(&file, locator("1.0.0", 0.005)).unwrap();
        root.select(&writer(&root), &bad).unwrap();
        assert!(session(&root, &key).feature_maps(&t.path().join("feature"), &w.manifest).unwrap_err().contains("as the session lock says"));
    }

    #[test]
    fn the_catalog_signature_binds_bytes_and_revision() {
        let key = TestKey::default();
        let c = catalog(7, vec![published(&key, "1.0.0", 0.005).0], vec![]);
        let (bytes, sig) = signed(&key, &c);
        assert_eq!(Catalog::verify(&bytes, &sig, &key.public()).unwrap(), c);
        let mut other = bytes.clone();
        other.extend(b" ");
        assert!(Catalog::verify(&other, &sig, &key.public()).is_err(), "changed bytes");
        assert!(Catalog::verify(&bytes, &key.sign(&bytes, "6"), &key.public()).unwrap_err().contains("signed for version 6"), "another revision");
        assert!(Catalog::verify(&bytes, &sig, &TestKey::default().public()).is_err(), "another key");
        assert!(Catalog::verify(&bytes, &sig, crate::feature::feed_key()).is_err(), "the shipped feed key parses and did not sign it");
        assert!(c.fresh(NOW) && !c.fresh(NOW + 86_401));
    }

    // Also Sol r1 N1: the same revision is the same only with the same exact bytes; other bytes
    // that parse to the same catalog (a space more) are other content.
    #[test]
    fn an_older_catalog_is_a_replay() {
        let key = TestKey::default();
        let t = tempfile::tempdir().unwrap();
        let root = Root::new(t.path());
        let accept_bytes = |b: &[u8], s: &str| root.accept_catalog(&writer(&root), b, s, &key.public(), Some(NOW)).map(|(_, a)| a);
        let accept = |c: &Catalog| { let (b, s) = signed(&key, c); accept_bytes(&b, &s) };
        let (one, two) = (published(&key, "1.0.0", 0.005).0, published(&key, "1.1.0", 0.005).0);
        assert_eq!(accept(&catalog(5, vec![one.clone()], vec![])), Ok(Accepted::Newer));
        assert_eq!(accept(&catalog(5, vec![one.clone()], vec![])), Ok(Accepted::Same));
        let (mut spaced, _) = signed(&key, &catalog(5, vec![one.clone()], vec![]));
        spaced.push(b' ');
        assert!(accept_bytes(&spaced, &key.sign(&spaced, "5")).unwrap_err().contains("other content"), "the exact bytes, not the parsed value");
        assert!(accept(&catalog(5, vec![one.clone(), two.clone()], vec![])).unwrap_err().contains("other content"));
        assert!(accept(&catalog(4, vec![one.clone(), two.clone()], vec![])).unwrap_err().contains("replay"));
        assert_eq!(accept(&catalog(6, vec![one, two], vec![])), Ok(Accepted::Newer));
        assert_eq!(root.cached_catalog(&key.public()).unwrap().unwrap().revision, 6);
        // A damaged cache gives an error, never an empty revocation list.
        std::fs::write(t.path().join("catalog.json"), b"{").unwrap();
        assert!(root.cached_catalog(&key.public()).is_err());
    }

    fn v2(c: Catalog) -> Catalog {
        Catalog { schema: V2_SCHEMA, view: Some(V2_VIEW.into()), ..c }
    }

    // Client contract item 7: the first v2 catalog starts its own trust files (its revisions
    // count apart) with the legacy revocations; after it, a legacy catalog is refused and the
    // legacy files stay. A schema and view pair other than (1, none) or (2, v2) never verifies.
    #[test]
    fn the_first_v2_catalog_starts_its_own_watermark_and_keeps_the_revocations() {
        let key = TestKey::default();
        let t = tempfile::tempdir().unwrap();
        let root = Root::new(t.path());
        let accept = |c: &Catalog| { let (b, s) = signed(&key, c); root.accept_catalog(&writer(&root), &b, &s, &key.public(), Some(NOW)).map(|(c, a)| (c.view, a)) };
        let one = published(&key, "1.0.0", 0.005).0;
        let gone = Revoked { id: "p42.test-camera".into(), version: "0.9.0".into(), reason: "bad".into() };
        assert_eq!(accept(&catalog(7, vec![one.clone()], vec![gone.clone()])), Ok((None, Accepted::Newer)));
        assert_eq!(accept(&v2(catalog(1, vec![one.clone()], vec![]))), Ok((Some("v2".into()), Accepted::Newer)), "revision 1 of v2 after legacy 7");
        assert_eq!(root.known_revoked().unwrap(), std::slice::from_ref(&gone), "the legacy revocations stay");
        let cached = root.cached_catalog(&key.public()).unwrap().unwrap();
        assert!(cached.is_v2() && cached.revision == 1);
        assert!(accept(&catalog(8, vec![one.clone()], vec![])).unwrap_err().contains("legacy module catalog after the v2"));
        assert!(accept(&v2(catalog(0, vec![one.clone()], vec![]))).unwrap_err().contains("replay"), "v2 has its own replay floor");
        assert_eq!(accept(&v2(catalog(2, vec![one.clone()], vec![]))), Ok((Some("v2".into()), Accepted::Newer)));
        for bad in [Catalog { schema: V2_SCHEMA, ..catalog(3, vec![one.clone()], vec![]) }, Catalog { view: Some(V2_VIEW.into()), ..catalog(3, vec![one.clone()], vec![]) }, Catalog { view: Some("v3".into()), ..v2(catalog(3, vec![one.clone()], vec![])) }] {
            assert!(accept(&bad).unwrap_err().contains("unknown schema"), "{:?} {:?}", bad.schema, bad.view);
        }
        // Sol review r5: the legacy files keep the legacy state for an older app after a switch.
        let legacy = Root::check_cache(&std::fs::read(t.path().join("catalog.json")).unwrap(), &key.public()).unwrap().0;
        assert_eq!((legacy.view, legacy.revision), (None, 7));
        assert!(matches!(root.trust_of(false), Trust::Known(Known { revision: 7, view: None, .. })));
        // A lost v2 watermark is damage, never a new start from the legacy one.
        for p in root.known_paths_of(true) { std::fs::remove_file(p).unwrap(); }
        assert!(root.known_revoked().unwrap_err().contains("missing"));
        assert!(accept(&v2(catalog(1, vec![one], vec![]))).unwrap_err().contains("replay"), "the v2 cache is the floor");
    }

    // Item 6 for modules: with the app's release, the lock build never takes a version of another
    // channel or one that needs a newer app (the signed manifest decides). No release: no filter.
    #[test]
    fn the_lock_build_takes_only_versions_that_this_app_takes() {
        use crate::release::{Channel, Release};
        let w = World::new(&[("1.0.0", 0.005), ("1.1.0", 0.004)]);
        w.store(0);
        w.store(1);
        let mut c = w.catalog(2, vec![]);
        c.modules[1].channel = Channel::Preview;
        let want = [w.want("0.3.0", None)];
        let (revoked, kept, stored) = (w.root.known_revoked().ok(), w.root.kept().unwrap(), w.root.all_stored().unwrap());
        let build = |r: Option<&Release>, stored: &[(ModuleManifest, String)]| {
            let lock = build_from(stored, &want, &Rules { catalog: Some(&c), now: NOW, revoked: revoked.as_deref(), kept: &kept, release: r }, &w.consent).unwrap().0;
            lock.modules.iter().map(|m| m.version.clone()).collect::<Vec<_>>()
        };
        let (stable, preview) = (Release { app: "0.2.0".into(), channel: Channel::Stable }, Release { app: "0.2.0".into(), channel: Channel::Preview });
        assert_eq!(build(None, &stored), ["1.1.0"]);
        assert_eq!(build(Some(&preview), &stored), ["1.1.0"]);
        assert_eq!(build(Some(&stable), &stored), ["1.0.0"], "a Stable app never locks a Preview version");
        let mut needs = stored.clone();
        needs.iter_mut().filter(|(m, _)| m.version == "1.0.0").for_each(|(m, _)| m.min_app = Some("0.3.0".into()));
        assert!(build(Some(&stable), &needs).is_empty(), "nor one that needs a newer app");
        // Item 8, a pinned feature: its kept lock names the Preview version; after the switch to
        // Stable the release rule comes before the lock exceptions, so the lock moves to Stable.
        w.commit(&want, Some(&c));
        let kept = w.root.kept().unwrap();
        let pinned = [Want { pinned: true, ..w.want("0.3.0", None) }];
        let lock = build_from(&stored, &pinned, &Rules { catalog: Some(&c), now: NOW, revoked: revoked.as_deref(), kept: &kept, release: Some(&stable) }, &w.consent).unwrap().0;
        assert_eq!(lock.modules.iter().map(|m| m.version.as_str()).collect::<Vec<_>>(), ["1.0.0"]);
    }

    // Item 7: the recovery floor compares only revisions of one view, so legacy locks with higher
    // revisions do not refuse a v2 recovery, and a v2 lock records its view.
    #[test]
    fn the_recovery_floor_counts_only_locks_of_the_same_view() {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.store(0);
        w.commit(&[w.want("0.3.0", None)], Some(&w.catalog(5, vec![])));
        let v2cat = v2(w.catalog(2, vec![]));
        w.publish(&v2cat);
        let (lock, _) = w.commit(&[w.want("0.3.1", None)], Some(&v2cat));
        assert_eq!((lock.catalog_revision, lock.catalog_view.as_deref()), (Some(2), Some("v2")));
        assert_eq!(w.root.lock_revisions(None).unwrap(), [5]);
        assert_eq!(w.root.lock_revisions(Some("v2")).unwrap(), [2]);
        // The same bindings under the v2 view are a new lock (its floor), not the legacy one.
        let (same, _) = w.commit(&[w.want("0.3.0", None)], Some(&v2cat));
        assert_eq!(same.catalog_view.as_deref(), Some("v2"));
        assert_eq!(w.root.lock_revisions(Some("v2")).unwrap().len(), 2);
        for p in w.root.known_paths() { std::fs::remove_file(p).unwrap(); }
        assert!(w.accept(&v2(w.catalog(1, vec![]))).unwrap_err().contains("replay"), "below the v2 floor");
        assert_eq!(w.accept(&v2(w.catalog(3, vec![]))), Ok(Accepted::Recovered), "legacy revision 5 is no floor for v2");
        // Every v2 file gone: v2 starts again from the legacy trust, and its locks stay the floor.
        for p in w.root.known_paths_of(true).into_iter().chain([w.root.cache_file_of(true)]) { std::fs::remove_file(p).unwrap(); }
        assert!(w.accept(&v2(w.catalog(1, vec![]))).unwrap_err().contains("replay"), "below the v2 locks");
        assert_eq!(w.accept(&v2(w.catalog(4, vec![]))), Ok(Accepted::Newer));
    }

    /// A store with module versions `versions`, a catalog that lists them (revision 1 accepted
    /// at the start, so the host knows the revocations), a feature that requires the map, and
    /// its folder. Returns what the tests change.
    struct World { t: tempfile::TempDir, key: TestKey, root: Root, entries: Vec<(CatalogEntry, Vec<u8>)>, consent: Consent }

    impl World {
        fn new(versions: &[(&str, f32)]) -> Self {
            let key = TestKey::default();
            let t = tempfile::tempdir().unwrap();
            let root = Root::new(t.path().join("modules"));
            let entries: Vec<(CatalogEntry, Vec<u8>)> = versions.iter().map(|(v, n)| published(&key, v, *n)).collect();
            let consent = approved(&entries[0].0.manifest);
            let w = World { t, key, root, entries, consent };
            w.publish(&w.fresh());
            w
        }
        fn store(&self, i: usize) { self.root.add_package(&writer(&self.root), &self.entries[i].0, &self.entries[i].1, &self.key.public()).unwrap(); }
        fn catalog(&self, revision: u64, revoked: Vec<Revoked>) -> Catalog { catalog(revision, self.entries.iter().map(|(e, _)| e.clone()).collect(), revoked) }
        /// Revision 1 with every version: what World::new accepts.
        fn fresh(&self) -> Catalog { self.catalog(1, vec![]) }
        fn publish(&self, c: &Catalog) { self.accept(c).unwrap(); }
        fn accept(&self, c: &Catalog) -> Result<Accepted, String> { let (b, s) = signed(&self.key, c); self.root.accept_catalog(&writer(&self.root), &b, &s, &self.key.public(), Some(NOW)).map(|(_, a)| a) }
        fn dir(&self, version: &str) -> PathBuf { self.t.path().join(format!("mirage-{version}")) }
        fn want(&self, version: &str, embedded: Option<&[u8]>) -> Want { installed(&self.dir(version), &feature(version, embedded), embedded) }
        /// build_lock with catalog `c`, the received revocations and the kept locks.
        fn build(&self, wants: &[Want], c: Option<&Catalog>) -> (Lock, Vec<Problem>) {
            let (revoked, kept) = (self.root.known_revoked().ok(), self.root.kept().unwrap());
            build_from(&self.root.all_stored().unwrap(), wants, &Rules { catalog: c, now: NOW, revoked: revoked.as_deref(), kept: &kept, release: None }, &self.consent).unwrap()
        }
        fn commit(&self, wants: &[Want], c: Option<&Catalog>) -> (Lock, Vec<Problem>) {
            let (lock, problems) = self.build(wants, c);
            self.root.select(&writer(&self.root), &lock).unwrap();
            (lock, problems)
        }
        fn plan(&self, others: &[Want], target: &Want, c: Option<&Catalog>) -> Result<Plan, String> {
            let (revoked, kept) = (self.root.known_revoked().ok(), self.root.kept().unwrap());
            plan_from(&self.root.all_stored().unwrap(), others, target, &Rules { catalog: c, now: NOW, revoked: revoked.as_deref(), kept: &kept, release: None })
        }
        fn maps(&self, w: &Want) -> Result<Vec<(MapDependency, Signed)>, String> { session(&self.root, &self.key).feature_maps(&self.dir(&w.manifest.version), &w.manifest) }
    }

    // Done check 2: a new module version changes the map of the same feature package.
    #[test]
    fn a_new_module_version_updates_the_map_with_the_same_feature() {
        let w = World::new(&[("1.0.0", 0.005), ("1.1.0", 0.004)]);
        let mirage = w.want("0.3.0", None);
        w.store(0);
        let c = w.fresh();
        let p = w.plan(&[], &mirage, Some(&c)).unwrap();
        assert_eq!(p.download.iter().map(|e| e.manifest.version.as_str()).collect::<Vec<_>>(), ["1.1.0"], "the newest is downloaded");
        let (one, _) = w.commit(std::slice::from_ref(&mirage), Some(&c));
        assert_eq!(one.modules[0].version, "1.0.0");
        assert_eq!(near(&w.maps(&mirage).unwrap()), 0.005);
        w.store(1);
        let (two, _) = w.commit(std::slice::from_ref(&mirage), Some(&c));
        assert_eq!((two.modules[0].version.as_str(), two.features[0].version.as_str()), ("1.1.0", "0.3.0"), "same feature, new map");
        assert_eq!(near(&w.maps(&mirage).unwrap()), 0.004);
        assert_eq!(w.root.pointer().unwrap().previous, [one.digest()]);
        // A newer catalog revision with the same selection makes no new lock (provenance only).
        assert!(!w.root.select(&writer(&w.root), &w.build(std::slice::from_ref(&mirage), Some(&w.catalog(2, vec![]))).0).unwrap());
    }

    // Done check 6: a pinned feature version restores the module version of its newest kept
    // lock, and the earlier lock file itself (dedupe by selection). Sol r1 S2: the plan that
    // consent shows and downloads is the lock that the build then selects, pin included. A
    // revoked old version is not restored.
    #[test]
    fn a_pinned_feature_restores_its_earlier_lock() {
        let w = World::new(&[("1.0.0", 0.005), ("1.1.0", 0.004)]);
        w.store(0);
        let old = w.want("0.3.0", None);
        let (a, _) = w.commit(std::slice::from_ref(&old), Some(&w.fresh()));
        w.store(1);
        let new = w.want("0.3.1", None);
        let (b, _) = w.commit(std::slice::from_ref(&new), Some(&w.fresh()));
        assert_eq!(b.modules[0].version, "1.1.0");
        let pinned = Want { pinned: true, ..old.clone() };
        let later = w.catalog(2, vec![]);
        let p = w.plan(std::slice::from_ref(&new), &pinned, Some(&later)).unwrap();
        let (back, problems) = w.commit(std::slice::from_ref(&pinned), Some(&later));
        assert!(problems.is_empty());
        assert_eq!(back.modules[0].version, "1.0.0");
        assert_eq!(p.modules.iter().map(|m| (m.id.as_str(), m.version.as_str())).collect::<Vec<_>>(), back.modules.iter().map(|m| (m.id.as_str(), m.version.as_str())).collect::<Vec<_>>(), "consent shows what activates");
        assert!(p.download.is_empty(), "no download for a restored lock");
        assert_ne!(back.digest(), a.digest(), "another catalog revision (provenance)");
        let pointer = w.root.pointer().unwrap();
        assert_eq!((pointer.current.as_deref(), pointer.previous.as_slice()), (Some(a.digest().as_str()), [b.digest()].as_slice()), "the earlier lock file, once in the history");
        // Latest again: the newest version.
        assert_eq!(w.commit(std::slice::from_ref(&old), Some(&w.fresh())).0.modules[0].version, "1.1.0");
        // Current revocations win over a rollback.
        let c = w.catalog(3, vec![Revoked { id: "p42.test-camera".into(), version: "1.0.0".into(), reason: "bad".into() }]);
        let (r, _) = w.commit(std::slice::from_ref(&pinned), Some(&c));
        assert_eq!(r.modules[0].version, "1.1.0");
        assert_eq!(w.plan(&[], &pinned, Some(&c)).unwrap().modules[0].version, "1.1.0");
    }

    // Done checks 7 and 11: a yank keeps the lock that uses it, gives no new selection, and a
    // safe replacement wins at the next build; with no replacement a new install is refused.
    #[test]
    fn a_yank_keeps_the_lock_and_blocks_new_selection() {
        let mut w = World::new(&[("1.0.0", 0.005), ("1.1.0", 0.004)]);
        w.store(0);
        let mirage = w.want("0.3.0", None);
        let (_, p) = w.commit(std::slice::from_ref(&mirage), Some(&w.fresh()));
        assert!(p.is_empty());
        w.entries[0].0.yanked = true;
        let yanked_only = catalog(2, vec![w.entries[0].0.clone()], vec![]);
        let (kept, p) = w.commit(std::slice::from_ref(&mirage), Some(&yanked_only));
        assert!(p.is_empty() && kept.modules[0].version == "1.0.0", "the installed feature keeps its yanked version");
        assert!(w.maps(&mirage).is_ok());
        let other = Want { manifest: Manifest { id: "other-depth".into(), ..mirage.manifest.clone() }, ..mirage.clone() };
        assert!(w.plan(&[], &other, Some(&yanked_only)).unwrap_err().contains("no current version"), "a new install cannot select it");
        let (both, p) = w.build(&[mirage.clone(), other.clone()], Some(&yanked_only));
        assert_eq!((both.features.len(), p[0].feature.as_str()), (1, "other-depth"));
        // A replacement in the catalog and the store: the next build takes it.
        w.store(1);
        let c = catalog(3, w.entries.iter().map(|(e, _)| e.clone()).collect(), vec![]);
        assert_eq!(w.commit(std::slice::from_ref(&mirage), Some(&c)).0.modules[0].version, "1.1.0");
    }

    // Done checks 7 and 10: a revocation that the app received (the cached signed catalog)
    // refuses the start, with no network; a damaged cache refuses too (unknown is not safe).
    #[test]
    fn a_known_revocation_refuses_the_start() {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.store(0);
        let mirage = w.want("0.3.0", None);
        w.commit(std::slice::from_ref(&mirage), Some(&w.fresh()));
        assert!(w.maps(&mirage).is_ok());
        w.publish(&w.catalog(2, vec![Revoked { id: "p42.test-camera".into(), version: "1.0.0".into(), reason: "reads the wrong memory".into() }]));
        assert!(w.maps(&mirage).unwrap_err().contains("was revoked"), "the cached lock does not start");
        let (lock, problems) = w.commit(std::slice::from_ref(&mirage), w.root.cached_catalog(&w.key.public()).unwrap().as_ref());
        assert!(lock.features.is_empty() && problems[0].text.contains("no current version"), "the app drops it from the next lock");
        std::fs::write(w.root.path().join("catalog.json"), b"{").unwrap();
        let s = session(&w.root, &w.key);
        assert!(s.feature_maps(&w.dir("0.3.0"), &mirage.manifest).is_err());
    }

    // Sol r1 B1: with no signed catalog cache the revocations are unknown, so a module map is
    // refused (it was an empty revocation list). The next accepted catalog starts it again.
    #[test]
    fn a_missing_catalog_cache_refuses_module_maps() {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.store(0);
        let mirage = w.want("0.3.0", None);
        w.commit(std::slice::from_ref(&mirage), Some(&w.fresh()));
        assert!(w.maps(&mirage).is_ok());
        std::fs::remove_file(w.root.path().join("catalog.json")).unwrap();
        assert!(w.maps(&mirage).unwrap_err().contains("no signed module catalog"));
        for k in w.root.known_paths() { std::fs::remove_file(k).unwrap(); }
        assert!(w.maps(&mirage).unwrap_err().contains("watermark is missing"), "with module files, a missing watermark is lost state");
        assert_eq!(w.accept(&w.fresh()), Ok(Accepted::Recovered));
        assert!(w.maps(&mirage).is_ok());
    }

    // Sol r1 B2: after cache damage, an older signed revision is still a replay, and the
    // revocations that the app received stay (lock build and host). The same revision again
    // repairs the cache.
    #[test]
    fn cache_damage_keeps_the_revision_and_the_revocations() {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.store(0);
        let mirage = w.want("0.3.0", None);
        w.commit(std::slice::from_ref(&mirage), Some(&w.fresh()));
        let revoked = w.catalog(2, vec![Revoked { id: "p42.test-camera".into(), version: "1.0.0".into(), reason: "bad".into() }]);
        w.publish(&revoked);
        std::fs::write(w.root.path().join("catalog.json"), b"{").unwrap();
        let (b, s) = signed(&w.key, &w.fresh());
        assert!(w.root.accept_catalog(&writer(&w.root), &b, &s, &w.key.public(), Some(NOW)).unwrap_err().contains("replay"), "revision 1 after 2");
        assert!(w.maps(&mirage).is_err(), "damaged: unknown revocations refuse");
        let (lock, problems) = w.build(std::slice::from_ref(&mirage), Some(&w.fresh()));
        assert!(lock.features.is_empty() && problems[0].text.contains("no current version"), "the revocation stays known: {problems:?}");
        // A deleted cache is the same: the watermark stays.
        std::fs::remove_file(w.root.path().join("catalog.json")).unwrap();
        assert!(w.root.accept_catalog(&writer(&w.root), &b, &s, &w.key.public(), Some(NOW)).unwrap_err().contains("replay"));
        let (b2, s2) = signed(&w.key, &revoked);
        assert_eq!(w.root.accept_catalog(&writer(&w.root), &b2, &s2, &w.key.public(), Some(NOW)).unwrap().1, Accepted::Same);
        assert_eq!(w.root.cached_catalog(&w.key.public()).unwrap().unwrap().revision, 2, "the cache is written again");
        assert!(w.maps(&mirage).unwrap_err().contains("was revoked"));
    }

    // Done checks 8 and 9: the host start needs only files on disk; an expired catalog stops
    // new selections and downloads but never a cached lock.
    #[test]
    fn a_cached_lock_starts_with_no_network() {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.store(0);
        let mirage = w.want("0.3.0", None);
        w.commit(std::slice::from_ref(&mirage), Some(&w.fresh()));
        // A new process with only the data folder: no feed URL, no client.
        let s = Session::load(&Root::new(w.root.path()), &w.key.public());
        assert_eq!(near(&s.feature_maps(&w.dir("0.3.0"), &mirage.manifest).unwrap()), 0.005);
    }

    // P1 live gaps: the host log names the lock, each map with its digest and module folder,
    // and the reason when there is no lock (a read wait that ends, no data folder).
    #[test]
    fn the_session_describes_its_lock_and_map_sources() {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.store(0);
        let mirage = w.want("0.3.0", None);
        let (lock, _) = w.commit(std::slice::from_ref(&mirage), Some(&w.fresh()));
        let lines = Session::load(&w.root, &w.key.public()).describe();
        assert_eq!(lines[0], format!("module lock {} (1 features, 1 modules)", lock.digest()), "{lines:?}");
        let dir = w.root.version_dir("p42.test-camera", "1.0.0").unwrap();
        let maps: Vec<&String> = lines.iter().filter(|l| l.starts_with("module lock map ")).collect();
        assert!(!maps.is_empty() && maps.len() == lock.features[0].maps.len(), "{lines:?}");
        for (line, x) in maps.iter().zip(&lock.features[0].maps) {
            assert!(line.contains(&x.sha256) && line.ends_with(&format!("from module p42.test-camera 1.0.0 at {}", dir.display())), "{line}");
        }
        assert_eq!(Session::unavailable("no data folder").describe(), vec!["module lock: none (no data folder)".to_string()]);
        let _held = w.root.writer().unwrap();
        let busy = Session::load_with(&w.root, &w.key.public(), std::time::Duration::from_millis(50)).describe();
        assert!(busy.len() == 1 && busy[0].starts_with("module lock: none (") && busy[0].len() > "module lock: none ()".len(), "{busy:?}");
    }

    // Sol r1 S1: with expired metadata the lock keeps the existing bindings (1.0.0 stays, also
    // with 1.1.0 in the store), a new feature gets no selection, and the plan says why. The
    // host still starts the cached lock.
    #[test]
    fn an_expired_catalog_keeps_bindings_and_makes_no_new_selection() {
        let w = World::new(&[("1.0.0", 0.005), ("1.1.0", 0.004)]);
        w.store(0);
        let mirage = w.want("0.3.0", None);
        w.commit(std::slice::from_ref(&mirage), Some(&w.fresh()));
        w.store(1);
        let mut c = w.catalog(2, vec![]);
        c.expires = NOW - 1;
        w.publish(&c);
        assert!(!c.fresh(NOW));
        let other = Want { manifest: Manifest { id: "other-depth".into(), ..mirage.manifest.clone() }, ..mirage.clone() };
        let (lock, problems) = w.commit(&[mirage.clone(), other.clone()], Some(&c));
        assert_eq!(lock.modules.iter().map(|m| m.version.as_str()).collect::<Vec<_>>(), ["1.0.0"], "the binding stays, no new version");
        assert_eq!(lock.features.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), ["mirage"]);
        assert!(problems.len() == 1 && problems[0].feature == "other-depth" && problems[0].text.contains("out of date"), "{problems:?}");
        assert!(w.plan(std::slice::from_ref(&mirage), &other, Some(&c)).unwrap_err().contains("out of date"));
        assert!(w.plan(&[], &other, None).unwrap_err().contains("out of date"), "no catalog: the same");
        assert_eq!(w.plan(&[], &mirage, Some(&c)).unwrap(), Plan { modules: vec![w.entries[0].0.manifest.clone()], download: vec![] });
        assert_eq!(near(&w.maps(&mirage).unwrap()), 0.005, "stale metadata does not stop installed features");
    }

    // Done check 12: no consent, no selection: a refusal activates nothing, and the lock of
    // before stays selected. A new power of an approved module asks again; a new version with
    // the same powers does not (the 7.2 exception with its conditions).
    #[test]
    fn no_consent_activates_nothing() {
        let mut w = World::new(&[("1.0.0", 0.005), ("1.1.0", 0.004)]);
        w.consent = Consent::default();
        w.store(0);
        let mirage = w.want("0.3.0", None);
        let before = w.root.pointer().unwrap();
        let (lock, problems) = w.build(std::slice::from_ref(&mirage), Some(&w.fresh()));
        assert!(lock.features.is_empty() && problems.len() == 1, "nothing selected without consent");
        assert_eq!(w.root.pointer().unwrap(), before);
        assert!(w.maps(&mirage).is_err(), "and the host does not start it");
        let m = &w.entries[0].0.manifest;
        assert!(w.consent.is_new(m) && w.consent.new_powers(m) == [Power::GameMemoryRead]);
        w.consent.approve(m);
        assert!(w.consent.covers(&w.entries[1].0.manifest), "a new version with the same powers: no question");
        let wider = ModuleManifest { powers: vec![Power::GameMemoryRead, Power::Unknown], ..m.clone() };
        assert!(!w.consent.covers(&wider) && !w.consent.is_new(&wider), "a new power asks again");
        let other = ModuleManifest { id: "someone.test-camera".into(), ..m.clone() };
        assert!(w.consent.is_new(&other), "another provider is a new identity");
        // With consent for 1.0.0 only by identity, 1.1.0 (same powers) is selected too.
        w.store(1);
        assert_eq!(w.commit(std::slice::from_ref(&mirage), Some(&w.fresh())).0.modules[0].version, "1.1.0");
    }

    // Done check 13: when the selected module map fails, the feature is refused; the embedded
    // copy of the same map id (valid, in the package) is never used. A legacy feature with
    // only embedded maps keeps working, with the embedded source in the lock.
    #[test]
    fn a_failed_module_map_never_falls_back_to_the_embedded_copy() {
        let w = World::new(&[("1.0.0", 0.005)]);
        let embedded = locator("1.0.0", 0.009);
        w.store(0);
        let mirage = w.want("0.3.0", Some(&embedded));
        let (lock, _) = w.commit(std::slice::from_ref(&mirage), Some(&w.fresh()));
        assert_eq!(lock.features[0].maps.iter().map(|m| m.source).collect::<Vec<_>>(), [MapSource::Module], "the required map comes from the module");
        assert_eq!(near(&w.maps(&mirage).unwrap()), 0.005, "the module map, not the embedded one (0.009)");
        // Break the module copy in three ways; each refuses, none gives 0.009.
        let file = w.root.version_dir("p42.test-camera", "1.0.0").unwrap().join(memory::map_path(&w.entries[0].0.manifest.provides[0].map));
        std::fs::write(&file, locator("1.0.0", 0.007)).unwrap();
        assert!(w.maps(&mirage).is_err());
        std::fs::remove_file(&file).unwrap();
        assert!(w.maps(&mirage).is_err());
        std::fs::remove_dir_all(w.root.path().join("store")).unwrap();
        assert!(w.maps(&mirage).is_err());
        // A legacy feature: no requires, the embedded map, its source in the lock.
        let legacy = Manifest { id: "legacy".into(), requires: vec![], ..feature("0.2.9", Some(&embedded)) };
        let lw = installed(&w.t.path().join("legacy"), &legacy, Some(&embedded));
        let (lock, p) = w.build(std::slice::from_ref(&lw), None);
        assert!(p.is_empty() && lock.features[0].maps[0].source == MapSource::Embedded);
        w.root.select(&writer(&w.root), &lock).unwrap();
        let s = session(&w.root, &w.key);
        assert_eq!(near(&s.feature_maps(&w.t.path().join("legacy"), &legacy).unwrap()), 0.009);
    }

    // Sol r1 S3: the host compares the module entry of the lock with the store: the package
    // digest, the approved powers, and the interface version of the map. Each change refuses.
    #[test]
    fn the_host_checks_the_locked_module_entry() {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.store(0);
        let mirage = w.want("0.3.0", None);
        let (lock, _) = w.commit(std::slice::from_ref(&mirage), Some(&w.fresh()));
        assert!(w.maps(&mirage).is_ok());
        let mut digest = lock.clone();
        digest.modules[0].sha256 = "0".repeat(64);
        let mut powers = lock.clone();
        powers.modules[0].powers.clear();
        let mut interface = lock.clone();
        interface.features[0].maps[0].interface_version = Some("1.9.0".into());
        let mut missing = lock.clone();
        missing.modules.clear();
        for (bad, text) in [(digest, "not the package"), (powers, "other powers"), (interface, "as the session lock says"), (missing, "not a module of the session lock")] {
            w.root.select(&writer(&w.root), &bad).unwrap();
            assert!(w.maps(&mirage).unwrap_err().contains(text), "{text}");
        }
    }

    // Done check 5 (fm_install part): the cell loads once; a lock selected later is not seen
    // by that process, and a new process sees it.
    #[test]
    fn the_session_cell_reads_once() {
        let w = World::new(&[("1.0.0", 0.005), ("1.1.0", 0.004)]);
        w.store(0);
        w.store(1);
        let mirage = w.want("0.3.0", None);
        let mut consent = w.consent.clone();
        consent.approve(&w.entries[0].0.manifest);
        let only_old = catalog(1, vec![], vec![Revoked { id: "p42.test-camera".into(), version: "1.1.0".into(), reason: "x".into() }]);
        let only_old = build_from(&w.root.all_stored().unwrap(), std::slice::from_ref(&mirage), &rules(Some(&only_old), &[], &[]), &consent).unwrap().0;
        w.root.select(&writer(&w.root), &only_old).unwrap();
        let cell = SessionCell::new();
        let load = || Session::load(&w.root, &w.key.public());
        assert_eq!(near(&cell.get(load).feature_maps(&w.dir("0.3.0"), &mirage.manifest).unwrap()), 0.005);
        w.commit(std::slice::from_ref(&mirage), Some(&w.fresh()));
        assert_eq!(near(&cell.get(load).feature_maps(&w.dir("0.3.0"), &mirage.manifest).unwrap()), 0.005, "the running process keeps its lock");
        assert_eq!(near(&load().feature_maps(&w.dir("0.3.0"), &mirage.manifest).unwrap()), 0.004, "the next start takes the new one");
    }

    // M3 (strict in P1): two features that need one module get one version that fits both; no
    // common version is a conflict that names both. A pinned feature without a kept lock gets
    // the newest.
    #[test]
    fn one_version_per_module_for_all_features() {
        let w = World::new(&[("1.0.0", 0.005), ("1.1.0", 0.004)]);
        w.store(0);
        w.store(1);
        let a = w.want("0.3.0", None);
        let mut bm = feature("0.3.0", None);
        bm.id = "other-depth".into();
        let b = installed(&w.t.path().join("b"), &bm, None);
        let c = w.fresh();
        let (lock, p) = build_from(&w.root.all_stored().unwrap(), &[a.clone(), b.clone()], &rules(Some(&c), &[], &[]), &w.consent).unwrap();
        assert!(p.is_empty() && lock.modules.len() == 1 && lock.modules[0].version == "1.1.0");
        // Interface ranges cannot differ here (both versions give 1.0.0), so a conflict comes
        // from two pins to different kept versions.
        let pa = Want { pinned: true, ..a.clone() };
        let pb = Want { pinned: true, ..b.clone() };
        let kept = vec![
            Lock { schema: 1, catalog_revision: None, catalog_view: None, features: vec![LockedFeature { id: "mirage".into(), version: "0.3.0".into(), sha256: a.sha256.clone(), maps: vec![LockedMap { id: "cam-locator".into(), source: MapSource::Module, version: "1.0.0".into(), sha256: "0".repeat(64), interface: None, interface_version: None, module: Some("p42.test-camera".into()), module_version: Some("1.0.0".into()) }] },
                LockedFeature { id: "other-depth".into(), version: "0.3.0".into(), sha256: b.sha256.clone(), maps: vec![LockedMap { id: "cam-locator".into(), source: MapSource::Module, version: "1.1.0".into(), sha256: "0".repeat(64), interface: None, interface_version: None, module: Some("p42.test-camera".into()), module_version: Some("1.1.0".into()) }] }], modules: vec![] },
        ];
        let (lock, p) = build_from(&w.root.all_stored().unwrap(), &[pa, pb], &rules(Some(&c), &[], &kept), &w.consent).unwrap();
        assert!(lock.features.is_empty() && p.len() == 2 && p[0].text.contains("one version of module p42.test-camera"), "{p:?}");
    }

    // The pointer keeps a bounded history and prune keeps what a kept lock names, and what a
    // pending change reserves (Sol r1 B4).
    #[test]
    fn prune_keeps_what_kept_locks_name() {
        let w = World::new(&[("1.0.0", 0.005), ("1.1.0", 0.004)]);
        w.store(0);
        let mirage = w.want("0.3.0", None);
        w.commit(std::slice::from_ref(&mirage), Some(&w.fresh()));
        w.store(1);
        w.commit(std::slice::from_ref(&mirage), Some(&w.fresh()));
        w.root.prune(&writer(&w.root)).unwrap();
        assert!(w.root.version_dir("p42.test-camera", "1.0.0").is_some(), "the previous lock keeps 1.0.0 for rollback and running games");
        for i in 0..KEPT_LOCKS + 1 {
            let mut m = feature(&format!("0.4.{i}"), None);
            m.id = "mirage".into();
            let want = installed(&w.dir(&format!("0.4.{i}")), &m, None);
            w.commit(std::slice::from_ref(&want), Some(&w.fresh()));
        }
        let reserved: BTreeSet<(String, String)> = [("p42.test-camera".to_string(), "1.0.0".to_string())].into();
        let lease = w.root.lease(&writer(&w.root), &reserved).unwrap();
        w.root.prune(&writer(&w.root)).unwrap();
        assert!(w.root.version_dir("p42.test-camera", "1.0.0").is_some(), "reserved for a pending install");
        drop(lease);
        w.root.prune(&writer(&w.root)).unwrap();
        assert_eq!(w.root.pointer().unwrap().previous.len(), KEPT_LOCKS);
        assert!(w.root.version_dir("p42.test-camera", "1.0.0").is_none(), "no kept lock names 1.0.0 now");
        assert_eq!(std::fs::read_dir(w.root.path().join("locks")).unwrap().count(), KEPT_LOCKS + 1);
    }

    // An edited lock file is refused by its name (content-addressed).
    #[test]
    fn an_edited_lock_is_refused() {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.store(0);
        let mirage = w.want("0.3.0", None);
        let (lock, _) = w.commit(std::slice::from_ref(&mirage), Some(&w.fresh()));
        let file = w.root.path().join("locks").join(format!("{}.json", lock.digest()));
        let text = std::fs::read_to_string(&file).unwrap().replace("1.0.0", "1.0.1");
        std::fs::write(&file, text).unwrap();
        assert!(w.root.current().unwrap_err().contains("changed on disk"));
        assert!(w.maps(&mirage).is_err(), "the host refuses a feature that needs the lock");
    }

    // Sol r1 B3: writers on 8 threads (one Root each, as two app processes) change the consent
    // and the lock pointer 20 times each. With one writer at a time, no change fails or is
    // lost (each read, change and write is whole), and the pointer stays valid.
    #[test]
    fn concurrent_writers_are_serialized() {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.store(0);
        let mirage = w.want("0.3.0", None);
        let (lock, _) = w.build(std::slice::from_ref(&mirage), Some(&w.fresh()));
        let base = w.entries[0].0.manifest.clone();
        let results: Vec<Result<(), String>> = std::thread::scope(|s| {
            let runs: Vec<_> = (0..8).map(|t| {
                let (path, lock, base) = (w.root.path().to_path_buf(), lock.clone(), base.clone());
                s.spawn(move || -> Result<(), String> {
                    let root = Root::new(path);
                    for i in 0..20 {
                        let wr = root.writer()?;
                        let mut c = root.consent()?;
                        c.approve(&ModuleManifest { id: format!("p42.t{t}-{i}"), ..base.clone() });
                        root.save_consent(&wr, &c)?;
                        let mut l = lock.clone();
                        l.features[0].version = format!("0.{t}.{i}");
                        root.select(&wr, &l)?;
                    }
                    Ok(())
                })
            }).collect();
            runs.into_iter().map(|r| r.join().unwrap()).collect()
        });
        assert!(results.iter().all(|r| r.is_ok()), "{results:?}");
        assert_eq!(w.root.consent().unwrap().modules.len(), 8 * 20, "no lost approval");
        let p = w.root.pointer().unwrap();
        assert!(w.root.current().unwrap().is_some() && p.previous.len() == KEPT_LOCKS && p.previous.iter().all(|d| w.root.lock(d).is_ok()));
        let other = Root::new(w.t.path().join("other"));
        assert!(other.select(&writer(&w.root), &lock).unwrap_err().contains("another folder"), "a writer is for its own folder");
    }

    // Sol r2 R2-1: a watermark copy that does not read is damaged, never missing. One valid copy
    // recovers. With no valid copy, the host and the lock build refuse module maps (probe b),
    // and only a catalog that is not expired and not older than a readable cache restores it.
    // A revocation stays when a later catalog does not list it.
    #[test]
    fn a_damaged_watermark_refuses_and_one_copy_recovers() {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.store(0);
        let mirage = w.want("0.3.0", None);
        w.commit(std::slice::from_ref(&mirage), Some(&w.fresh()));
        let bad = Revoked { id: "p42.test-camera".into(), version: "1.0.0".into(), reason: "bad".into() };
        w.publish(&w.catalog(2, vec![bad.clone()]));
        let later = w.catalog(3, vec![]);
        w.publish(&later);
        assert!(w.maps(&mirage).unwrap_err().contains("was revoked"), "a later catalog that omits it does not lift it");
        let [a, b] = w.root.known_paths();
        std::fs::write(&a, b"{").unwrap();
        assert!(w.maps(&mirage).unwrap_err().contains("was revoked"), "copy B recovers");
        std::fs::write(&b, b"{").unwrap();
        assert!(w.maps(&mirage).unwrap_err().contains("watermark is damaged"));
        let (lock, problems) = w.build(std::slice::from_ref(&mirage), Some(&later));
        assert!(lock.features.is_empty() && problems[0].text.contains("revocations are unknown"), "{problems:?}");
        assert!(w.accept(&w.catalog(2, vec![])).unwrap_err().contains("older than revision 3"));
        let mut expired = w.catalog(4, vec![]);
        expired.expires = NOW - 1;
        assert!(w.accept(&expired).unwrap_err().contains("expired"));
        assert_eq!(w.accept(&w.catalog(4, vec![bad])), Ok(Accepted::Recovered));
        assert!(w.maps(&mirage).unwrap_err().contains("was revoked"));
        for k in [a, b] { std::fs::remove_file(k).unwrap(); }
        assert!(w.maps(&mirage).unwrap_err().contains("watermark is missing"));
    }

    // Sol r2 R2-1, probe a: all trust files are damaged after revision 2. An old signed catalog
    // that is expired is not accepted, and nothing activates meanwhile. A catalog that is not
    // expired at the feed time restores the state (the recovery rule).
    #[test]
    fn losing_every_trust_file_accepts_only_a_catalog_that_is_not_expired() {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.store(0);
        let mirage = w.want("0.3.0", None);
        w.commit(std::slice::from_ref(&mirage), Some(&w.fresh()));
        w.publish(&w.catalog(2, vec![Revoked { id: "p42.test-camera".into(), version: "1.0.0".into(), reason: "bad".into() }]));
        for f in w.root.known_paths().into_iter().chain([w.root.cache_file()]) { std::fs::write(f, b"{").unwrap(); }
        assert!(w.maps(&mirage).is_err());
        assert!(w.build(std::slice::from_ref(&mirage), Some(&w.fresh())).0.features.is_empty());
        let mut old = w.fresh();
        old.expires = NOW - 1;
        assert!(w.accept(&old).unwrap_err().contains("expired"), "the replay of an old signed catalog");
        assert_eq!(w.accept(&w.fresh()), Ok(Accepted::Recovered));
    }

    // Sol r2 R2-3: a lease is on disk, so the prune of another process (another Root on the
    // same folder) keeps its modules. When the owner ends with no clean-up, the next prune
    // removes the lease and the modules. The drop of a lease removes its files.
    #[test]
    fn a_lease_of_another_process_keeps_its_modules_until_the_owner_ends() {
        let w = World::new(&[("1.0.0", 0.005), ("1.1.0", 0.004)]);
        w.store(0);
        w.store(1);
        let (one, two) = (Root::new(w.root.path()), Root::new(w.root.path()));
        let lease = one.lease(&writer(&one), &[("p42.test-camera".to_string(), "1.0.0".to_string())].into()).unwrap();
        two.prune(&writer(&two)).unwrap();
        assert!(two.version_dir("p42.test-camera", "1.0.0").is_some() && two.version_dir("p42.test-camera", "1.1.0").is_none());
        let leases = || std::fs::read_dir(w.root.path().join("leases")).unwrap().count();
        assert_eq!(leases(), 2);
        lease.abandon();
        two.prune(&writer(&two)).unwrap();
        assert!(two.version_dir("p42.test-camera", "1.0.0").is_none(), "a dead owner keeps nothing");
        assert_eq!(leases(), 0, "its lease files go");
        drop(one.lease(&writer(&one), &BTreeSet::new()).unwrap());
        assert_eq!(leases(), 0, "the drop removes a lease");
    }

    // Sol r2 R2-5: the host reads the trust files, the pointer and the lock as one snapshot. A
    // writer between the watermark and the cache write makes it wait (before, it saw the
    // mismatch and kept the error until the game restarted). A writer that stays longer than
    // the wait gives a clear refusal.
    #[test]
    fn the_host_reads_one_snapshot_under_the_shared_lock() {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.store(0);
        let mirage = w.want("0.3.0", None);
        w.commit(std::slice::from_ref(&mirage), Some(&w.fresh()));
        let (b, s) = signed(&w.key, &w.catalog(2, vec![]));
        let wr = writer(&w.root);
        w.root.write_known(&wr, &Known { revision: 2, sha256: memory::digest(&b), revoked: vec![], feed_time: 0, view: None }).unwrap();
        let (root, key, dir, m) = (w.root.clone(), w.key.public(), w.dir("0.3.0"), mirage.manifest.clone());
        let reader = std::thread::spawn(move || Session::load(&root, &key).feature_maps(&dir, &m).map(|_| ()));
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(w.root.accept_catalog(&wr, &b, &s, &w.key.public(), Some(NOW)).unwrap().1, Accepted::Same);
        drop(wr);
        assert_eq!(reader.join().unwrap(), Ok(()), "the host waited for the whole change");
        let wr = writer(&w.root);
        let busy = Session::load_with(&w.root, &w.key.public(), std::time::Duration::from_millis(100));
        assert!(busy.feature_maps(&w.dir("0.3.0"), &mirage.manifest).unwrap_err().contains("busy"));
        drop(wr);
    }

    // Sol r3 R3-1: recovery trusts only an authenticated time (the app gives the HTTPS Date of
    // the feed), and a floor that needs no clock: the catalog revision that built a lock file.
    // All trust files and the pointer are lost after revision 3; its lock file stays. With no
    // feed time nothing recovers, and a revision 2 that is not expired is a replay.
    #[test]
    fn recovery_needs_the_feed_time_and_a_revision_at_the_lock_floor() {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.store(0);
        let mirage = w.want("0.3.0", None);
        let three = w.catalog(3, vec![]);
        w.publish(&three);
        assert_eq!(w.commit(std::slice::from_ref(&mirage), Some(&three)).0.catalog_revision, Some(3));
        for f in w.root.known_paths().into_iter().chain([w.root.cache_file(), w.root.pointer_file()]) { std::fs::remove_file(f).unwrap(); }
        let at = |c: &Catalog, t: Option<u64>| { let (b, s) = signed(&w.key, c); w.root.accept_catalog(&writer(&w.root), &b, &s, &w.key.public(), t).map(|(_, a)| a) };
        assert!(at(&three, None).unwrap_err().contains("secure (HTTPS) answer"), "no authenticated time");
        assert!(w.root.cached_catalog(&w.key.public()).is_err(), "a refusal writes nothing");
        assert!(at(&w.catalog(2, vec![]), Some(NOW)).unwrap_err().contains("older than revision 3"), "the floor of the lock file");
        assert_eq!(at(&three, Some(NOW)), Ok(Accepted::Recovered));
    }

    // Sol r3 R3-2: an owner drops its lease between the list of the lease files and their open
    // in a prune (the AFTER_LIST hook). The prune goes on, and that lease keeps nothing. A live
    // lease that does not read stays an error, so a prune never guesses what it keeps.
    #[test]
    fn a_lease_dropped_between_list_and_open_does_not_stop_the_prune() {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.store(0);
        let lease = w.root.lease(&writer(&w.root), &[("p42.test-camera".to_string(), "1.0.0".to_string())].into()).unwrap();
        AFTER_LIST.with(|h| *h.borrow_mut() = Some(Box::new(move || drop(lease))));
        w.root.prune(&writer(&w.root)).unwrap();
        assert!(w.root.version_dir("p42.test-camera", "1.0.0").is_none(), "the dropped lease kept nothing");
        let live = w.root.lease(&writer(&w.root), &BTreeSet::new()).unwrap();
        std::fs::write(live.dir.join(format!("{}.json", live.name)), b"{").unwrap();
        assert!(w.root.prune(&writer(&w.root)).unwrap_err().contains("module lease"));
        drop(live);
    }

    // Sol r4 R4-2: the watermark keeps the highest authenticated feed time. It is never lowered.
    // A time after the expiry is kept as expiry + 1 s, so a bad Date cannot make a later catalog
    // look expired. The highest copy wins when one copy is damaged.
    #[test]
    fn the_seen_feed_time_only_rises_and_stops_after_the_expiry() {
        let w = World::new(&[("1.0.0", 0.005)]);
        assert_eq!(w.root.seen_time(), NOW, "World::new accepted at NOW");
        let at = |c: &Catalog, t: Option<u64>| { let (b, s) = signed(&w.key, c); w.root.accept_catalog(&writer(&w.root), &b, &s, &w.key.public(), t).unwrap(); w.root.seen_time() };
        let two = w.catalog(2, vec![]);
        assert_eq!(at(&two, Some(NOW - 50)), NOW, "never lowered");
        assert_eq!(at(&two, None), NOW, "no time keeps it");
        assert_eq!(at(&two, Some(u64::MAX / 2)), two.expires + 1, "a far-future Date stops after the expiry");
        std::fs::write(&w.root.known_paths()[0], b"{").unwrap();
        assert_eq!(w.root.seen_time(), two.expires + 1, "copy B keeps it");
        let b: Known = serde_json::from_slice(&std::fs::read(&w.root.known_paths()[1]).unwrap()).unwrap();
        std::fs::write(&w.root.known_paths()[0], serde_json::to_vec(&Known { feed_time: 1, ..b }).unwrap()).unwrap();
        assert_eq!(w.root.seen_time(), two.expires + 1, "the copies merge: the higher feed time wins");
        let mut three = w.catalog(3, vec![]);
        three.expires = NOW + 10 * 86_400;
        assert_eq!(at(&three, Some(NOW + 2 * 86_400)), NOW + 2 * 86_400, "a later catalog is fresh again");
    }

    // Sol r5 R5-1: with damaged trust, select and prune change nothing. The lock files stay the
    // replay floor and the packages stay the last good state, so an older catalog is still a
    // replay. Here both watermark copies, the cache and the pointer are lost after revision 3.
    #[test]
    fn damaged_trust_selects_and_prunes_nothing() {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.store(0);
        let mirage = w.want("0.3.0", None);
        let three = w.catalog(3, vec![]);
        w.publish(&three);
        w.commit(std::slice::from_ref(&mirage), Some(&three));
        for f in w.root.known_paths().into_iter().chain([w.root.cache_file(), w.root.pointer_file()]) { std::fs::remove_file(f).unwrap(); }
        assert!(w.root.select(&writer(&w.root), &Lock::default()).unwrap_err().contains("until the module list is restored"));
        assert!(w.root.prune(&writer(&w.root)).unwrap_err().contains("until the module list is restored"));
        assert_eq!(w.root.lock_revisions(None).unwrap(), [3], "the floor stays");
        assert!(w.root.version_dir("p42.test-camera", "1.0.0").is_some(), "the package stays");
        let (b, s) = signed(&w.key, &w.catalog(2, vec![]));
        assert!(w.root.accept_catalog(&writer(&w.root), &b, &s, &w.key.public(), Some(NOW)).unwrap_err().contains("older than revision 3"));
    }

    // Sol r6 R6-1 (same cause in the recovery floor): a locks folder that does not read is not
    // "no locks". The state stays established (not first use), and the floor is not known, so
    // an older catalog cannot restore. Only the locks folder records state here.
    #[test]
    fn an_unreadable_locks_folder_refuses_the_recovery() {
        let w = World::new(&[("1.0.0", 0.005)]);
        let mirage = w.want("0.3.0", None);
        let three = w.catalog(3, vec![]);
        w.publish(&three);
        w.commit(std::slice::from_ref(&mirage), Some(&three));
        for f in w.root.known_paths().into_iter().chain([w.root.cache_file(), w.root.pointer_file()]) { std::fs::remove_file(f).unwrap(); }
        let _ = std::fs::remove_dir_all(w.root.0.join("store"));
        let locks = w.root.0.join("locks");
        std::fs::rename(&locks, w.root.0.join("locks-aside")).unwrap();
        std::fs::write(&locks, b"").unwrap();
        assert!(!w.root.first_use(), "established");
        let (b, s) = signed(&w.key, &w.catalog(2, vec![]));
        assert!(w.root.accept_catalog(&writer(&w.root), &b, &s, &w.key.public(), Some(NOW)).unwrap_err().contains("module locks"));
    }

    // Sol r5 R5-2: a removal that fails is a prune error, not a silent success. A file of an
    // old module version is held open with no delete share (Windows), so its folder cannot go.
    // When the handle closes, the next prune removes it.
    #[cfg(windows)]
    #[test]
    fn a_removal_that_fails_is_a_prune_error() {
        use std::os::windows::fs::OpenOptionsExt;
        let w = World::new(&[("1.0.0", 0.005), ("1.1.0", 0.004)]);
        w.store(0);
        w.store(1);
        let mirage = w.want("0.3.0", None);
        assert_eq!(w.commit(std::slice::from_ref(&mirage), Some(&w.fresh())).0.modules[0].version, "1.1.0");
        let file = w.root.version_dir("p42.test-camera", "1.0.0").unwrap().join(MANIFEST);
        // FILE_SHARE_READ only: no FILE_SHARE_DELETE.
        let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(&file).unwrap();
        assert!(w.root.prune(&writer(&w.root)).unwrap_err().contains("1.0.0"));
        drop(held);
        w.root.prune(&writer(&w.root)).unwrap();
        assert!(w.root.version_dir("p42.test-camera", "1.0.0").is_none());
    }

    // Sol r6 R6-1: a leases folder that does not read is not "no leases". The prune stops with
    // an error and removes nothing, so a reserved module cannot go. Here the folder is a file.
    #[test]
    fn an_unreadable_lease_folder_stops_the_prune() {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.store(0);
        let dir = w.root.0.join("leases");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::write(&dir, b"").unwrap();
        assert!(w.root.prune(&writer(&w.root)).unwrap_err().contains("module leases"));
        assert!(w.root.version_dir("p42.test-camera", "1.0.0").is_some(), "nothing is removed");
        std::fs::remove_file(&dir).unwrap();
        w.root.prune(&writer(&w.root)).unwrap();
        assert!(w.root.version_dir("p42.test-camera", "1.0.0").is_none());
    }

    // Sol r6 R6-1: one entry that does not read (the BAD_ENTRY hook) stops the prune before any
    // removal: first the .lock of a live lease that reserves 1.0.0, then a version folder.
    #[test]
    fn an_entry_that_does_not_read_stops_the_prune_before_any_removal() {
        let w = World::new(&[("1.0.0", 0.005), ("1.1.0", 0.004)]);
        w.store(0);
        w.store(1);
        let mirage = w.want("0.3.0", None);
        assert_eq!(w.commit(std::slice::from_ref(&mirage), Some(&w.fresh())).0.modules[0].version, "1.1.0");
        let bad = |p: Option<PathBuf>| crate::tests::BAD_ENTRY.with(|b| *b.borrow_mut() = p);
        let lease = w.root.lease(&writer(&w.root), &[("p42.test-camera".to_string(), "1.0.0".to_string())].into()).unwrap();
        bad(Some(lease.dir.join(format!("{}.lock", lease.name))));
        assert!(w.root.prune(&writer(&w.root)).unwrap_err().contains("module leases"));
        assert!(w.root.version_dir("p42.test-camera", "1.0.0").is_some(), "the reserved module stays");
        drop(lease);
        bad(w.root.version_dir("p42.test-camera", "1.1.0"));
        assert!(w.root.prune(&writer(&w.root)).unwrap_err().contains("versions"));
        assert!(w.root.version_dir("p42.test-camera", "1.0.0").is_some(), "nothing is removed");
        bad(None);
        w.root.prune(&writer(&w.root)).unwrap();
        assert!(w.root.version_dir("p42.test-camera", "1.0.0").is_none());
    }

    // Sol r6 R6-1: the lease files of a dead owner that do not go are a prune error. The .json is
    // held open with no delete share (Windows). When the handle closes, the next prune removes it.
    #[cfg(windows)]
    #[test]
    fn a_lease_that_does_not_go_is_a_prune_error() {
        use std::os::windows::fs::OpenOptionsExt;
        let w = World::new(&[("1.0.0", 0.005)]);
        let dir = w.root.0.join("leases");
        std::fs::create_dir_all(&dir).unwrap();
        let (json, lock) = (dir.join("999-1.json"), dir.join("999-1.lock"));
        std::fs::write(&json, b"[]").unwrap();
        std::fs::write(&lock, b"").unwrap();
        let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(&json).unwrap();
        assert!(w.root.prune(&writer(&w.root)).unwrap_err().contains("999-1.json"));
        drop(held);
        w.root.prune(&writer(&w.root)).unwrap();
        assert!(!json.exists() && !lock.exists(), "the dead lease is gone");
    }

    /// Windows: opens `p` with delete share only. Until the drop, no other open (a read too)
    /// succeeds, but a replace (rename over it) does: a read error for a moment, as a scanner.
    #[cfg(windows)]
    fn hold(p: &Path) -> std::fs::File {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new().read(true).share_mode(4).open(p).unwrap()
    }

    /// Mirage committed with catalog 3, then both watermark copies, the cache and the pointer
    /// lost: only the recovery rule restores. Returns the revision 3 lock file (the floor).
    fn lost_after_three() -> (World, PathBuf) {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.store(0);
        let mirage = w.want("0.3.0", None);
        let three = w.catalog(3, vec![]);
        w.publish(&three);
        w.commit(std::slice::from_ref(&mirage), Some(&three));
        let file = w.root.lock_file(&w.root.pointer().unwrap().current.unwrap());
        for f in w.root.known_paths().into_iter().chain([w.root.cache_file(), w.root.pointer_file()]) { std::fs::remove_file(f).unwrap(); }
        (w, file)
    }

    // Sol r7 R7-1: the selected lock does not read for a moment. The prune stops with an error
    // and keeps the package that the lock names; kept and select stop too. When the read works
    // again, the prune keeps the package because the lock names it.
    #[cfg(windows)]
    #[test]
    fn a_kept_lock_that_does_not_read_stops_the_prune() {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.store(0);
        let mirage = w.want("0.3.0", None);
        assert_eq!(w.commit(std::slice::from_ref(&mirage), Some(&w.fresh())).0.modules[0].version, "1.0.0");
        let file = w.root.lock_file(&w.root.pointer().unwrap().current.unwrap());
        let held = hold(&file);
        assert!(w.root.prune(&writer(&w.root)).unwrap_err().contains(&file.display().to_string()), "the path (R8-3)");
        assert!(w.root.version_dir("p42.test-camera", "1.0.0").is_some(), "the package stays");
        assert!(w.root.kept().is_err());
        assert!(w.root.select(&writer(&w.root), &Lock::default()).unwrap_err().contains("module lock"));
        drop(held);
        w.root.prune(&writer(&w.root)).unwrap();
        assert!(w.root.version_dir("p42.test-camera", "1.0.0").is_some(), "the lock names it");
    }

    // Sol r7 R7-2: a lock file that does not read refuses the recovery, so a signed catalog
    // below its revision cannot restore. When it reads again, revision 2 is a replay.
    #[cfg(windows)]
    #[test]
    fn an_unreadable_lock_refuses_the_recovery() {
        let (w, file) = lost_after_three();
        let held = hold(&file);
        let e = w.accept(&w.catalog(2, vec![])).unwrap_err();
        assert!(e.contains("the replay floor is not known") && e.contains(&file.display().to_string()), "{e}");
        drop(held);
        assert!(w.accept(&w.catalog(2, vec![])).unwrap_err().contains("older than revision 3"));
    }

    // Sol r7 R7-2, the decision: a damaged lock file has no known revision, so it refuses every
    // recovery (fail closed), also a revision above it. The error names the file. When the file
    // is good again (or removed), recovery works.
    #[test]
    fn a_damaged_lock_refuses_the_recovery() {
        let (w, file) = lost_after_three();
        let good = std::fs::read(&file).unwrap();
        std::fs::write(&file, b"{}").unwrap();
        let e = w.accept(&w.catalog(2, vec![])).unwrap_err();
        assert!(e.contains("changed on disk; the replay floor is not known") && e.contains(&file.display().to_string()), "the error names the file: {e}");
        assert!(w.accept(&w.catalog(4, vec![])).unwrap_err().contains("the replay floor is not known"), "fail closed");
        std::fs::write(&file, good).unwrap();
        assert_eq!(w.accept(&w.catalog(4, vec![])), Ok(Accepted::Recovered));
    }

    // Sol r9 R9-2: the selected lock is damaged and the watermark stays good. The app starts
    // again offline with a fresh cached catalog, and the same build has the same digest: the
    // selection writes the file again, so the prune reads it and keeps the module it names.
    #[test]
    fn a_damaged_current_lock_is_written_again_by_the_same_selection() {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.store(0);
        let mirage = w.want("0.3.0", None);
        let (lock, _) = w.commit(std::slice::from_ref(&mirage), Some(&w.fresh()));
        let file = w.root.lock_file(&lock.digest());
        std::fs::write(&file, b"{}").unwrap();
        let (again, _) = w.build(std::slice::from_ref(&mirage), Some(&w.fresh()));
        assert_eq!(again.digest(), lock.digest(), "the same build");
        w.root.select(&writer(&w.root), &again).unwrap();
        assert_eq!(w.root.kept_lock(&lock.digest()), Ok(Some(lock)), "the file is good again");
        w.root.prune(&writer(&w.root)).unwrap();
        assert!(w.root.version_dir("p42.test-camera", "1.0.0").is_some(), "the prune keeps the module");
        assert!(w.maps(&mirage).is_ok());
    }

    // Sol r7 R7-2 (same cause): a cache that does not read refuses the recovery. Here the cache
    // (revision 3) is the only floor: no lock was made.
    #[cfg(windows)]
    #[test]
    fn an_unreadable_cache_refuses_the_recovery() {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.publish(&w.catalog(3, vec![]));
        for f in w.root.known_paths() { std::fs::remove_file(f).unwrap(); }
        let held = hold(&w.root.cache_file());
        let e = w.accept(&w.catalog(2, vec![])).unwrap_err();
        assert!(e.contains("the replay floor is not known") && e.contains(&w.root.cache_file().display().to_string()), "{e}");
        drop(held);
        assert!(w.accept(&w.catalog(2, vec![])).unwrap_err().contains("older than revision 3"));
    }

    // Sol r7 sweep: a consent file that does not read is an error, not "no approvals", so the
    // app does not save over approvals that it could not see.
    #[cfg(windows)]
    #[test]
    fn a_consent_file_that_does_not_read_is_an_error() {
        let w = World::new(&[("1.0.0", 0.005)]);
        let mut c = Consent::default();
        c.approve(&w.entries[0].0.manifest);
        w.root.save_consent(&writer(&w.root), &c).unwrap();
        let held = hold(&w.root.consent_file());
        assert!(w.root.consent().unwrap_err().contains(&w.root.consent_file().display().to_string()), "the path (R8-3)");
        drop(held);
        assert!(w.root.consent().unwrap() == c);
    }

    // Sol r7 sweep: a stored manifest that does not read is an error of all_stored, not "not in
    // the store". A damaged one is not listed, and add_package repairs it.
    #[cfg(windows)]
    #[test]
    fn a_store_file_that_does_not_read_is_an_error() {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.store(0);
        let file = w.root.version_dir("p42.test-camera", "1.0.0").unwrap().join(MANIFEST);
        let held = hold(&file);
        assert!(w.root.all_stored().unwrap_err().contains(&file.display().to_string()), "the path (R8-3)");
        drop(held);
        std::fs::write(&file, b"{").unwrap();
        assert!(w.root.all_stored().unwrap().is_empty(), "damaged: not listed");
        w.store(0);
        assert_eq!(w.root.all_stored().unwrap().len(), 1, "repaired");
    }

    // Sol r7 sweep: a feature package stamp that does not read refuses its maps with that cause.
    #[cfg(windows)]
    #[test]
    fn a_package_stamp_that_does_not_read_is_an_error() {
        let w = World::new(&[("1.0.0", 0.005)]);
        w.store(0);
        let mirage = w.want("0.3.0", None);
        w.commit(std::slice::from_ref(&mirage), Some(&w.fresh()));
        let held = hold(&w.dir("0.3.0").join(crate::PACKAGE_STAMP));
        assert!(w.maps(&mirage).unwrap_err().contains(&w.dir("0.3.0").join(crate::PACKAGE_STAMP).display().to_string()), "the path (R8-3)");
        drop(held);
        assert!(w.maps(&mirage).is_ok());
    }
}
