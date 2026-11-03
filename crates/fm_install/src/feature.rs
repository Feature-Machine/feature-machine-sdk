// Feature packages (plan 2026-10-18-feature-packages.md). A package is a zip with feature.json and
// its parts: bin/<os>/ (the feature library that our in-game host loads), ui/ (pages for the app),
// ats-mod/ (files for the ATS mod folder) and msfs-community/<package>/ (one MSFS Community
// package). The manifest is checked before anything installs, so a bad id or path can never
// leave the store folder or reach a game folder outside the part it belongs to.
use fm_platform::Game;
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Seek};
use std::path::{Component, Path, PathBuf};

// One definition for the app, the hosts and the features (the SDK owns them).
pub use fm_feature_sdk::{valid_host, Capability, Multiplayer, API_VERSION as FEATURE_API, MAX_NETWORK_HOSTS, MIN_API_VERSION as MIN_FEATURE_API};
pub use fm_feature_sdk::active::MapDependency;
pub use fm_feature_sdk::{Access, UserFile};
pub use fm_input::ControlDecl;

/// The most controls that a feature can declare: a member binds each one by hand.
pub const MAX_CONTROLS: usize = 32;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    /// One sentence for the Library card and the consent dialog.
    pub description: String,
    pub api: u32,
    pub games: Vec<Game>,
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub multiplayer: Multiplayer,
    /// Why the review set "risk" or "blocked"; shown to the member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiplayer_note: Option<String>,
    /// UI page in ui/ (for example "index.html"), when the feature has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<String>,
    /// First words of the engine commands that the feature runs (API 2, capability
    /// game_command); consent shows them, and the host refuses any other command.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub game_commands: Vec<String>,
    /// Hosts that the feature reaches with HTTPS GET (API 3, capability network); consent shows
    /// each one, and the hosts and the app refuse any other host (parent plan Q14).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_hosts: Vec<String>,
    /// Map packages carried with this feature, pinned by digest. Consent lists them. Without
    /// vars_memory and vars_write they are camera maps with no signature of ours (plan 6c).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_maps: Vec<MapDependency>,
    /// The camera contract of pinned maps, by map id; consent asks again when it changes (plan
    /// 6b answer 2). A top-level field, because older apps and hosts ignore unknown fields here
    /// but refuse them in a pin (MapDependency) and so the whole catalog (review round 2).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub map_contracts: std::collections::BTreeMap<String, String>,
    /// Named controls that the member binds in the app (API 5, capability controls); consent
    /// lists them. Default bindings are keyboard and mouse only (control input plan).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controls: Vec<ControlDecl>,
    /// Game files that the feature reads or changes in the game's user folder (API 5, capability
    /// user_files, fm_feature_sdk::files); consent lists each one, and the hosts refuse others.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub user_files: Vec<UserFile>,
    /// Maps that a module provides (module layer plan 8.1): by interface and map id, never by
    /// module. The lock selects the provider; the host reads a required map only from it, also
    /// when the package carries an embedded copy for older apps (they ignore this field).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<Requirement>,
    /// The oldest app version that this build runs on (release-admin item 6, crate::release).
    /// In the signed package, so the feed cannot lower it. Apps before 0.2 ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_app: Option<String>,
    /// API 6: page, icon, banner, widgets and settings (crate::surfaces). Flattened, so the
    /// fields sit at the top of feature.json and an API 1 to 5 manifest has none of them.
    #[serde(flatten)]
    pub surfaces: crate::surfaces::Surfaces,
}

pub use fm_feature_sdk::active::Requirement;

/// The most module requirements of one feature.
pub const MAX_REQUIRES: usize = 8;

/// Feature ids are folder names, ATS mod names and URL parts: lower case, digits and "-".
pub fn check_id(id: &str) -> io::Result<()> {
    let ok = (3..=48).contains(&id.len())
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
        && !id.ends_with('-');
    ok.then_some(()).ok_or_else(|| bad(format!("not a feature id: {id:?}")))
}

fn bad(m: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m)
}

impl Manifest {
    /// Every rule a package must meet before the store or a game folder sees it.
    pub fn check(&self) -> io::Result<()> {
        check_id(&self.id)?;
        crate::check_version(&self.version)?;
        crate::release::check_min_app(&self.id, self.min_app.as_deref()).map_err(bad)?;
        if !(MIN_FEATURE_API..=FEATURE_API).contains(&self.api) {
            return Err(bad(format!("{} needs feature API {}, this app gives {MIN_FEATURE_API} to {FEATURE_API}", self.id, self.api)));
        }
        if self.capabilities.contains(&Capability::Unknown) {
            return Err(bad(format!("{} needs a capability that this version of Feature Machine does not know", self.id)));
        }
        if self.games.is_empty() {
            return Err(bad(format!("{} names no game", self.id)));
        }
        if self.name.trim().is_empty() || self.name.len() > 60 {
            return Err(bad(format!("{}: the name must have 1 to 60 characters", self.id)));
        }
        if let Some(ui) = &self.ui {
            safe_rel(ui)?;
            if !self.capabilities.contains(&Capability::UiApp) {
                return Err(bad(format!("{} has a UI page but no ui_app capability", self.id)));
            }
        }
        self.check_commands()?;
        self.check_memory()?;
        self.check_controls()?;
        if self.capabilities.contains(&Capability::RenderPass) && self.api < 5 {
            return Err(bad("render_pass needs API 5".into()));
        }
        if self.capabilities.contains(&Capability::RenderDepth) && !self.capabilities.contains(&Capability::RenderPass) {
            return Err(bad("render_depth needs render_pass".into()));
        }
        if self.capabilities.contains(&Capability::RenderShader) && !self.capabilities.contains(&Capability::RenderPass) {
            return Err(bad("render_shader needs render_pass".into()));
        }
        self.check_network()?;
        self.check_user_files()?;
        self.check_requires()?;
        crate::surfaces::check(self)
    }

    /// Module requirements (plan 8.1), API 5 and 1 to MAX_REQUIRES entries with a valid range and
    /// map id, one per map id. Two kinds: a camera-only depth feature requires camera contracts
    /// (the 6c trust path; an embedded copy's named contract must agree), and a memory writer
    /// (vars_read, vars_memory, vars_write, no embedded map) requires setter maps only (ground
    /// wetness plan; memory::module_writer, which the app takes only from the feed).
    fn check_requires(&self) -> io::Result<()> {
        if self.requires.is_empty() { return Ok(()); }
        let caps = &self.capabilities;
        let camera = caps.contains(&Capability::RenderDepth) && !caps.contains(&Capability::VarsMemory) && !caps.contains(&Capability::VarsWrite);
        let writer = [Capability::VarsRead, Capability::VarsMemory, Capability::VarsWrite].iter().all(|c| caps.contains(c)) && self.memory_maps.is_empty();
        if self.api < 5 || !(camera || writer) || self.requires.len() > MAX_REQUIRES {
            return Err(bad(format!("{}: requires needs API 5, 1 to {MAX_REQUIRES} entries, and render_depth with no vars_memory or vars_write (camera maps) or vars_read, vars_memory and vars_write with no embedded map (setter maps)", self.id)));
        }
        let allowed: &[&str] = if camera { &[crate::memory::CAMERA_CONTRACT, crate::memory::LOCATOR_CONTRACT] } else { &[crate::memory::SETTER_V2_CONTRACT] };
        let mut seen = std::collections::BTreeSet::new();
        for r in &self.requires {
            let range = semver::VersionReq::parse(&r.versions).is_ok();
            let contract = self.map_contracts.get(&r.map).is_none_or(|c| *c == r.interface);
            if !allowed.contains(&r.interface.as_str()) || !range || !fm_memory::valid_id(&r.map) || !seen.insert(&r.map) || !contract {
                return Err(bad(format!("{}: bad requirement {:?} (a contract of its kind, a semver range, a map id once, its map_contracts entry)", self.id, r.map)));
            }
        }
        Ok(())
    }

    /// User files: API 5 and the user_files capability with 1 to 16 valid entries, or none of
    /// them, so consent never shows the power with no file (the SDK rule: files::check_list).
    fn check_user_files(&self) -> io::Result<()> {
        let cap = self.capabilities.contains(&Capability::UserFiles);
        if self.user_files.is_empty() && !cap {
            return Ok(());
        }
        if self.api < 5 || !cap {
            return Err(bad(format!("{} needs API 5, the user_files capability and its user_files list together", self.id)));
        }
        // The host game ids ("ats") are the serde names of Game.
        let games: Vec<String> = self.games.iter().filter_map(|g| serde_json::to_value(g).ok()?.as_str().map(String::from)).collect();
        fm_feature_sdk::files::check_list(&self.user_files, &games).map_err(|e| bad(format!("{}: {e}", self.id)))
    }

    fn check_memory(&self) -> io::Result<()> {
        let memory = self.capabilities.contains(&Capability::VarsMemory);
        let write = self.capabilities.contains(&Capability::VarsWrite);
        // A named contract belongs to a pinned map and is a camera contract (verification checks
        // the map file against it).
        let cameras = [crate::memory::CAMERA_CONTRACT, crate::memory::LOCATOR_CONTRACT];
        if self.map_contracts.iter().any(|(id, c)| !self.memory_maps.iter().any(|m| &m.id == id) || !cameras.contains(&c.as_str())) {
            return Err(bad("map_contracts names a map that is not pinned, or a contract that is not a camera contract".into()));
        }
        if !memory && !write && self.memory_maps.is_empty() { return Ok(()); }
        // A render_depth feature may pin camera maps without vars_memory (owner decision, plan 7.5): the
        // host reads them for the shader host block only. Verification refuses other contracts.
        let camera = !memory && !write && self.capabilities.contains(&Capability::RenderDepth);
        // A memory writer whose maps all come from modules pins none (check_requires checks it).
        let from_modules = memory && self.memory_maps.is_empty() && !self.requires.is_empty();
        if self.api < 5 || !(camera || memory && self.capabilities.contains(&Capability::VarsRead)) || (self.memory_maps.is_empty() && !from_modules) || self.memory_maps.len() > 32 {
            return Err(bad("memory needs API 5, vars_read, vars_memory (or render_depth for camera maps) and 1 to 32 pinned maps (none when all maps come from modules)".into()));
        }
        let mut seen = std::collections::BTreeSet::new();
        for m in &self.memory_maps {
            if !fm_memory::valid_id(&m.id) || !seen.insert(&m.id) || crate::check_version(&m.version).is_err() || !crate::memory::digest_valid(&m.sha256) {
                return Err(bad("invalid or duplicate map dependency".into()));
            }
        }
        Ok(())
    }

    /// Controls: API 5 and the controls capability with 1 to MAX_CONTROLS declarations, or none
    /// of them, so consent never shows "controls" with no control. Each declaration passes its
    /// own check (id rule, label, a default that fits the kind), and ids are unique.
    fn check_controls(&self) -> io::Result<()> {
        let cap = self.capabilities.contains(&Capability::Controls);
        if self.controls.is_empty() && !cap {
            return Ok(());
        }
        if self.api < 5 || !cap || self.controls.is_empty() || self.controls.len() > MAX_CONTROLS {
            return Err(bad(format!("{} needs API 5, the controls capability and 1 to {MAX_CONTROLS} controls together", self.id)));
        }
        let mut seen = std::collections::BTreeSet::new();
        for c in &self.controls {
            c.check().map_err(|e| bad(format!("{}: {e}", self.id)))?;
            if !seen.insert(&c.id) {
                return Err(bad(format!("{} declares the control {:?} twice", self.id, c.id)));
            }
        }
        Ok(())
    }

    /// Network hosts: API 3 and the network capability, both or neither; 1 to MAX_NETWORK_HOSTS
    /// exact lower-case DNS names (valid_host), no duplicates. So consent lists every host the
    /// feature can reach, and "network" never shows with no host.
    fn check_network(&self) -> io::Result<()> {
        let cap = self.capabilities.contains(&Capability::Network);
        if self.network_hosts.is_empty() && !cap {
            return Ok(());
        }
        if self.api < 3 || !cap || self.network_hosts.is_empty() {
            return Err(bad(format!("{} needs API 3, the network capability and its network_hosts together", self.id)));
        }
        if self.network_hosts.len() > MAX_NETWORK_HOSTS {
            return Err(bad(format!("{} lists more than {MAX_NETWORK_HOSTS} network hosts", self.id)));
        }
        if let Some(h) = self.network_hosts.iter().find(|h| !valid_host(h)) {
            return Err(bad(format!("{}: {h:?} is not a lower-case host name (no scheme, port, path or wildcard)", self.id)));
        }
        let mut sorted = self.network_hosts.clone();
        sorted.sort();
        sorted.dedup();
        if sorted.len() != self.network_hosts.len() {
            return Err(bad(format!("{} lists a network host twice", self.id)));
        }
        Ok(())
    }

    /// Engine commands: API 2 and the game_command capability, and plain first words, so the
    /// consent dialog shows exactly what the host allows.
    fn check_commands(&self) -> io::Result<()> {
        if self.game_commands.is_empty() {
            return Ok(());
        }
        if self.api < 2 || !self.capabilities.contains(&Capability::GameCommand) {
            return Err(bad(format!("{} lists game commands but has not API 2 and the game_command capability", self.id)));
        }
        let word = |w: &&String| (1..=40).contains(&w.len()) && w.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if let Some(c) = self.game_commands.iter().find(|c| !word(c)) {
            return Err(bad(format!("{}: bad game command {c:?}", self.id)));
        }
        Ok(())
    }

    /// The page entry in ui/: `page.entry` (API 6), else the older `ui` field.
    pub fn page_entry(&self) -> Option<&str> {
        self.surfaces.page.as_ref().map(|p| p.entry.as_str()).or(self.ui.as_deref())
    }

    pub fn runs_on(&self, game: Game) -> bool {
        self.games.contains(&game)
    }
}

/// A package path ("/" separated) with normal parts only: no "..", no root, no drive, and no "\"
/// or ":" in a part. The same rule on every OS: Linux takes "C:\x" or "..\x" as one normal name,
/// so a package that passes there would fail or escape on Windows. Joined per part.
pub fn safe_rel(p: &str) -> io::Result<PathBuf> {
    let parts: Vec<&str> = p.split('/').collect();
    let normal = |s: &&str| !s.is_empty() && *s != "." && *s != ".." && !s.contains(['\\', ':']) && matches!(Path::new(s).components().next(), Some(Component::Normal(_)));
    let ok = !p.is_empty() && parts.iter().all(normal);
    ok.then(|| parts.iter().collect()).ok_or_else(|| bad(format!("not a safe relative path: {p:?}")))
}

/// Reads and checks feature.json from a package zip, without unpacking it.
pub fn read_manifest<R: Read + Seek>(zip: R) -> io::Result<Manifest> {
    inspect(zip).map(|(m, _)| m)
}

/// The manifest and the parts of a package (parts::ATS_MOD, parts::MSFS_COMMUNITY, parts::UI,
/// "bin"), checked: game files need the game_files capability, so consent always shows them. The
/// parts come from the same entry check as the unpack (crate::checked_entries).
pub fn inspect<R: Read + Seek>(zip: R) -> io::Result<(Manifest, Vec<String>)> {
    let mut a = zip::ZipArchive::new(zip).map_err(|e| bad(format!("not a feature package: {e}")))?;
    let entries = crate::checked_entries(&mut a, crate::Kind::Feature)?;
    let mut text = String::new();
    a.by_name(crate::FEATURE_MANIFEST).map_err(|_| bad(format!("the package has no {}", crate::FEATURE_MANIFEST)))?.read_to_string(&mut text)?;
    let m: Manifest = serde_json::from_str(&text).map_err(|e| bad(format!("{}: {e}", crate::FEATURE_MANIFEST)))?;
    m.check()?;
    if !m.memory_maps.is_empty() { crate::memory::verify_archive(&mut a, &m).map_err(bad)?; }
    let mut found: Vec<String> = entries.iter().filter(|(p, _)| p.components().count() > 1).filter_map(|(p, _)| p.components().next()?.as_os_str().to_str().map(String::from)).collect();
    found.sort();
    found.dedup();
    let game_files = found.iter().any(|p| p == parts::ATS_MOD || p == parts::MSFS_COMMUNITY);
    if game_files && !m.capabilities.contains(&Capability::GameFiles) {
        return Err(bad(format!("{} adds game files but does not declare game_files", m.id)));
    }
    if !entries.iter().any(|(p, dir)| !dir && *p == safe_rel(&parts::library()).unwrap_or_default()) {
        return Err(bad(format!("{} has no library for this system ({})", m.id, parts::library())));
    }
    // Sizes from the zip headers; a file is read only when its size is within its limit.
    let files: Vec<(String, u64)> = entries.iter().filter(|(_, dir)| !dir).filter_map(|(p, _)| {
        let name = p.components().filter_map(|c| c.as_os_str().to_str()).collect::<Vec<_>>().join("/");
        Some((name.clone(), a.by_name(&name).ok()?.size()))
    }).collect();
    crate::surfaces::check_files(&m, &files, &mut |name| {
        let mut out = vec![];
        a.by_name(name).map_err(io::Error::other)?.take(crate::surfaces::limits::BANNER + 1).read_to_end(&mut out)?;
        Ok(out)
    })?;
    for (p, dir) in entries.iter().filter(|(p, _)| p.starts_with(parts::SHADERS)) {
        let name = p.strip_prefix(parts::SHADERS).ok().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        if *dir && name.is_empty() { continue; }
        let size = a.by_name(&format!("{}/{name}", parts::SHADERS)).map(|e| e.size()).unwrap_or(u64::MAX);
        check_shader(&m, &name, size)?;
    }
    Ok((m, found))
}

/// A compiled shader in a package: shaders/<name>.cso directly in that folder, a name that the
/// SDK takes (fm_feature_sdk::shader_name_valid), at most SHADER_SIZE_MAX bytes, and the
/// render_shader capability, so consent always shows that the feature runs its own shaders.
fn check_shader(m: &Manifest, file: &str, size: u64) -> io::Result<()> {
    let name = file.strip_suffix(".cso").filter(|n| fm_feature_sdk::shader_name_valid(n));
    if name.is_none() { return Err(bad(format!("{}: shaders/{file} is not a compiled shader name (<name>.cso: a-z, 0-9, _ or -)", m.id))); }
    if size > fm_feature_sdk::abi::SHADER_SIZE_MAX as u64 { return Err(bad(format!("{}: shaders/{file} is larger than {} bytes", m.id, fm_feature_sdk::abi::SHADER_SIZE_MAX))); }
    if !m.capabilities.contains(&Capability::RenderShader) { return Err(bad(format!("{} has shaders but does not declare render_shader", m.id))); }
    Ok(())
}

/// Part folders inside a feature version folder.
pub mod parts {
    fn os() -> &'static str {
        if cfg!(windows) { "windows" } else { "linux" }
    }
    fn library_file() -> String {
        format!("{}feature{}", std::env::consts::DLL_PREFIX, std::env::consts::DLL_SUFFIX)
    }
    /// The archive entry of the feature library for this OS: bin/windows/feature.dll or
    /// bin/linux/libfeature.so. Zip names always use "/", so this is not a file system path.
    pub fn library() -> String {
        format!("bin/{}/{}", os(), library_file())
    }
    /// The feature library in an unpacked version folder, joined per component, so the path uses
    /// the separator of this OS only (active.json, logs, the host's LoadLibrary).
    pub fn library_path(version_dir: &std::path::Path) -> std::path::PathBuf {
        version_dir.join("bin").join(os()).join(library_file())
    }
    pub const UI: &str = "ui";
    pub const ATS_MOD: &str = "ats-mod";
    pub const MSFS_COMMUNITY: &str = "msfs-community";
    /// Compiled pixel shaders (<name>.cso, capability render_shader). In a feature's source, the
    /// same folder holds the HLSL that fm_pack compiles (pack_shaders).
    pub const SHADERS: &str = "shaders";
    /// The folders that a package can have at its top, besides feature.json. art and widgets:
    /// API 6 (crate::surfaces).
    pub const DIRS: [&str; 8] = ["bin", UI, ATS_MOD, MSFS_COMMUNITY, "maps", SHADERS, crate::surfaces::ART, crate::surfaces::WIDGETS];
}

/// Builds a feature package (fm_pack feature): feature.json and the ui/, ats-mod/ and
/// msfs-community/ folders from `src` (each when present), and each library at its bin/<os>/
/// path (.dll: Windows, .so: Linux). The manifest is checked first, so no bad package leaves.
pub fn pack(src: &Path, libraries: &[PathBuf], out: &mut (impl io::Write + Seek)) -> io::Result<Manifest> {
    pack_shaders(src, libraries, &[], out)
}

/// pack with the compiled shaders of the source's shaders/ folder (name, bytecode), which go in
/// as shaders/<name>.cso. fm_pack compiles them; a source with a shaders/ folder and no compiled
/// shaders is refused, so HLSL never ships uncompiled.
pub fn pack_shaders(src: &Path, libraries: &[PathBuf], shaders: &[(String, Vec<u8>)], out: &mut (impl io::Write + Seek)) -> io::Result<Manifest> {
    let m: Manifest = serde_json::from_slice(&std::fs::read(src.join(crate::FEATURE_MANIFEST))?).map_err(|e| bad(format!("{}: {e}", crate::FEATURE_MANIFEST)))?;
    m.check()?;
    if libraries.is_empty() {
        return Err(bad(format!("{}: a feature package needs its library", m.id)));
    }
    if !m.memory_maps.is_empty() { crate::memory::verify_source(src, libraries, shaders, &m).map_err(bad)?; }
    if let Some(ui) = &m.ui {
        if !src.join(parts::UI).join(safe_rel(ui)?).is_file() {
            return Err(bad(format!("{}: ui/{ui} is missing", m.id)));
        }
    }
    if src.join(parts::SHADERS).is_dir() && shaders.is_empty() {
        return Err(bad(format!("{}: shaders/ must be compiled first (fm_pack feature compiles it)", m.id)));
    }
    for (name, bytes) in shaders { check_shader(&m, &format!("{name}.cso"), bytes.len() as u64)?; }
    check_surface_files(src, &m)?;
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).unix_permissions(0o644);
    let mut z = zip::ZipWriter::new(out);
    let mut add = |name: &str, path: &Path| -> io::Result<()> {
        z.start_file(name, opts).map_err(io::Error::other)?;
        io::copy(&mut std::fs::File::open(path)?, &mut z)?;
        Ok(())
    };
    add(crate::FEATURE_MANIFEST, &src.join(crate::FEATURE_MANIFEST))?;
    // A camera-only feature has no signed session: its pins bind its maps (plan 6c).
    if !m.memory_maps.is_empty() && !crate::memory::camera_only(&m) {
        for name in [crate::memory::SESSION, crate::memory::SIGNATURE] { add(name, &src.join(name))?; }
    }
    for lib in libraries {
        let name = match lib.extension().and_then(|e| e.to_str()) {
            Some("dll") => "bin/windows/feature.dll",
            Some("so") => "bin/linux/libfeature.so",
            _ => return Err(bad(format!("{}: a library must be a .dll or a .so", lib.display()))),
        };
        add(name, lib)?;
    }
    for part in [parts::UI, parts::ATS_MOD, parts::MSFS_COMMUNITY, "maps", crate::surfaces::ART, crate::surfaces::WIDGETS] {
        let dir = src.join(part);
        if !dir.is_dir() {
            continue;
        }
        let mut files = vec![];
        crate::package::walk(&dir, &dir, &mut files)?;
        files.sort();
        for rel in files {
            add(&format!("{part}/{rel}"), &dir.join(&rel))?;
        }
    }
    for (name, bytes) in shaders {
        z.start_file(format!("{}/{name}.cso", parts::SHADERS), opts).map_err(io::Error::other)?;
        io::Write::write_all(&mut z, bytes)?;
    }
    z.finish().map_err(io::Error::other)?;
    Ok(m)
}

/// The same file rules as inspect (crate::surfaces::check_files) on the ui/, art/ and widgets/
/// folders of a feature source: pack runs it, and a test runs it on each first-party feature.
pub fn check_surface_files(src: &Path, m: &Manifest) -> io::Result<()> {
    let mut files = vec![];
    for part in [parts::UI, crate::surfaces::ART, crate::surfaces::WIDGETS] {
        let dir = src.join(part);
        if !dir.is_dir() { continue; }
        let mut rels = vec![];
        crate::package::walk(&dir, &dir, &mut rels)?;
        for rel in rels { files.push((format!("{part}/{rel}"), std::fs::metadata(dir.join(&rel))?.len())); }
    }
    crate::surfaces::check_files(m, &files, &mut |name| std::fs::read(src.join(safe_rel(name)?)))
}

/// One feature in the catalog of the feed (features/catalog.json).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    #[serde(flatten)]
    pub manifest: Manifest,
    /// Package URL, relative to the catalog or absolute.
    pub url: String,
    /// SHA-256 of the package (the download check). The signature below proves who made it.
    pub sha256: String,
    pub size: u64,
    /// Top folders of the package (inspect), so consent shows the game files before download.
    #[serde(default)]
    pub parts: Vec<String>,
    /// The feature's history file (every published version), relative to the catalog. Older
    /// apps ignore it and keep using this entry, the newest version (version history plan).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<String>,
    /// The feed signature of the package: minisign in the Tauri format, signed for its version
    /// with the feed key (publishing pipeline plan, L1). The app refuses an entry without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// The channel of this version on the feed (release-admin item 6). Not in the package: a
    /// promotion moves the same bytes from Preview to Stable. Missing: stable (legacy feed).
    #[serde(default, skip_serializing_if = "crate::release::Channel::is_stable")]
    pub channel: crate::release::Channel,
    /// The icon and banner of the package (API 6), copied next to it on the feed, so Discover
    /// shows them before an install. Not in the package: the catalog step writes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub art: Option<CatalogArt>,
}

/// Feed copies of a package's art (crate::surfaces): URLs relative to the catalog, as `url`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CatalogArt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<ArtFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banner: Option<ArtFile>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArtFile {
    pub url: String,
    /// SHA-256 of the file: the app shows it only when the download has this digest.
    pub sha256: String,
}

impl CatalogArt {
    /// The same art with each URL changed by `f` (the feed views put files at other paths).
    pub fn map_urls(&self, f: impl Fn(&str) -> String) -> CatalogArt {
        let map = |a: &Option<ArtFile>| a.as_ref().map(|a| ArtFile { url: f(&a.url), sha256: a.sha256.clone() });
        CatalogArt { icon: map(&self.icon), banner: map(&self.banner) }
    }
}

/// The public key of the feature feed. Its own key, not the app updater key: the publishing
/// pipeline holds only this one, so a leak there cannot sign an app update (owner decision 2026-10-24).
pub fn feed_key() -> &'static str {
    include_str!("../keys/feature-feed.pub").trim()
}

/// Checks that `bytes` is the package of `entry`, signed with `key` for the entry's version.
pub fn verify_package(bytes: &[u8], entry: &CatalogEntry, key: &str) -> Result<(), String> {
    let sig = entry.signature.as_deref().ok_or("the package has no Feature Machine signature")?;
    crate::package::verify(bytes, sig, key, &entry.manifest.version)
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub features: Vec<CatalogEntry>,
}

/// The path of a feature's history file, relative to the catalog.
pub fn history_path(id: &str) -> String {
    format!("{id}/versions.json")
}

/// One published version. Its package URL is relative to the catalog, as in CatalogEntry, so
/// both files point into the same files/ folder. `published`: Unix seconds (UTC). A withdrawn
/// version stays on the feed (members who run it keep it) but no member can install it again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    #[serde(flatten)]
    pub entry: CatalogEntry,
    pub published: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub withdrawn: bool,
}

/// Every published version of one feature, newest first (version history plan, slice 1). The
/// feed never deletes a version: the member can pin any one that is not withdrawn.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct History {
    pub versions: Vec<HistoryEntry>,
}

impl History {
    /// Adds a published version. The same version with the same bytes changes nothing (a
    /// retry: false), except that it gives a signature to an entry from before signing; with
    /// other bytes it is refused, because members who have that version must never get other
    /// content under its number. Returns true when the history changed.
    pub fn add(&mut self, entry: CatalogEntry, published: u64) -> Result<bool, String> {
        if let Some(old) = self.versions.iter_mut().find(|v| v.entry.manifest.version == entry.manifest.version) {
            if old.entry.sha256 != entry.sha256 {
                return Err(format!("{} {} is already on the feed with other content; give it a new version", entry.manifest.id, entry.manifest.version));
            }
            let signs = old.entry.signature.is_none() && entry.signature.is_some();
            if signs {
                old.entry.signature = entry.signature;
            }
            return Ok(signs);
        }
        self.versions.push(HistoryEntry { entry, published, withdrawn: false });
        // Newest first by semantic version; a version that does not parse goes last.
        self.versions.sort_by_key(|v| std::cmp::Reverse(version_key(&v.entry.manifest.version)));
        Ok(true)
    }

    /// Withdraws a version: hidden from the version list and from auto-update. The file stays.
    pub fn withdraw(&mut self, version: &str) -> Result<(), String> {
        let v = self.versions.iter_mut().find(|v| v.entry.manifest.version == version).ok_or_else(|| format!("version {version} is not on the feed"))?;
        v.withdrawn = true;
        Ok(())
    }

    /// The newest version that is not withdrawn: the catalog entry of the feature.
    pub fn latest(&self) -> Option<&CatalogEntry> {
        self.versions.iter().find(|v| !v.withdrawn).map(|v| &v.entry)
    }
}

fn version_key(v: &str) -> Option<semver::Version> {
    semver::Version::parse(v).ok()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn manifest() -> Manifest {
        Manifest {
            id: "speed-alert".into(),
            name: "Speed alert".into(),
            version: "1.0.0".into(),
            description: "Warns above a speed.".into(),
            api: FEATURE_API,
            games: vec![Game::Ats],
            capabilities: vec![Capability::VarsRead, Capability::UiApp],
            multiplayer: Multiplayer::Allowed,
            multiplayer_note: None,
            ui: Some("index.html".into()),
            game_commands: vec![],
            network_hosts: vec![], memory_maps: vec![], map_contracts: Default::default(), controls: vec![], user_files: vec![], requires: vec![], min_app: None, surfaces: Default::default(),
        }
    }

    /// user_files: API 5 and the capability with 1 to 16 entries, both or neither; each entry
    /// for a game of the feature with a user folder and a plain pattern (the SDK rule).
    #[test]
    fn user_file_rules() {
        let file = |path: &str, access| UserFile { game: "ats".into(), path: path.into(), access };
        let m = Manifest { capabilities: vec![Capability::UiApp, Capability::UserFiles], user_files: vec![file("game.log.txt", Access::Read), file("profiles/*/config.cfg", Access::Write)], ..manifest() };
        m.check().unwrap();
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains(r#""user_files":[{"game":"ats","path":"game.log.txt","access":"read"}"#) && json.contains(r#""ui_app","user_files"]"#), "{json}");
        assert_eq!(serde_json::from_str::<Manifest>(&json).unwrap(), m, "feature.json round trip");
        assert!(!serde_json::to_string(&manifest()).unwrap().contains("user_files"), "absent when empty");
        assert!(Manifest { user_files: vec![], ..m.clone() }.check().is_err(), "the capability needs files");
        assert!(Manifest { capabilities: vec![Capability::UiApp], ..m.clone() }.check().is_err(), "files need the capability");
        assert!(Manifest { api: 4, ..m.clone() }.check().is_err(), "user_files needs API 5");
        assert!(Manifest { user_files: vec![file("../Documents/x", Access::Read)], ..m.clone() }.check().is_err());
        assert!(Manifest { games: vec![Game::Msfs2024], ..m.clone() }.check().is_err(), "a game of the feature with a user folder");
    }

    #[test]
    fn ids_and_paths_must_be_safe() {
        for bad in ["", "ab", "Speed", "a b", "../x", "-x", "x-", "a/b", "é-x"] {
            assert!(check_id(bad).is_err(), "{bad}");
        }
        check_id("trip").unwrap();
        for bad in ["", "../x", "/x", "a/../../b", "C:\\x", "..\\x", "a\\b", "C:x", "a//b", "./x", "x/"] {
            assert!(safe_rel(bad).is_err(), "{bad}");
        }
        safe_rel("pages/index.html").unwrap();
    }

    #[test]
    fn manifest_rules() {
        manifest().check().unwrap();
        assert!(Manifest { api: FEATURE_API + 1, ..manifest() }.check().is_err(), "unknown API major");
        assert!(Manifest { api: 0, ..manifest() }.check().is_err());
        Manifest { api: 1, ..manifest() }.check().unwrap();
        let weather = Manifest { api: 2, capabilities: vec![Capability::UiApp, Capability::GameCommand], game_commands: vec!["g_set_weather".into()], ..manifest() };
        weather.check().unwrap();
        assert!(Manifest { api: 1, ..weather.clone() }.check().is_err(), "commands need API 2");
        assert!(Manifest { capabilities: vec![Capability::UiApp], ..weather.clone() }.check().is_err(), "commands need game_command");
        for bad in ["", "g_set weather", "G_SET", "g;quit", "g-set"] {
            assert!(Manifest { game_commands: vec![bad.into()], ..weather.clone() }.check().is_err(), "{bad:?}");
        }
        assert!(Manifest { games: vec![], ..manifest() }.check().is_err());
        assert!(Manifest { capabilities: vec![Capability::VarsRead], ..manifest() }.check().is_err(), "UI needs ui_app");
        assert!(Manifest { ui: Some("../app.html".into()), ..manifest() }.check().is_err());
        let pass = Manifest { capabilities: vec![Capability::UiApp, Capability::RenderPass], ..manifest() };
        pass.check().unwrap();
        assert!(serde_json::to_string(&pass).unwrap().contains("\"render_pass\""));
        assert!(Manifest { api: 4, ..pass.clone() }.check().is_err(), "render_pass needs API 5");
        let depth = Manifest { capabilities: vec![Capability::UiApp, Capability::RenderPass, Capability::RenderDepth], ..pass };
        depth.check().unwrap();
        assert!(serde_json::to_string(&depth).unwrap().contains("\"render_depth\""));
        assert!(Manifest { capabilities: vec![Capability::RenderDepth], ..depth.clone() }.check().is_err(), "render_depth needs render_pass");
        // Camera maps (plan 7.5): render_depth may pin them without vars_memory; render_pass may not.
        let pin = MapDependency { id: "msfs2024-camera".into(), version: "1.0.0".into(), sha256: "a".repeat(64) };
        Manifest { memory_maps: vec![pin.clone()], ..depth.clone() }.check().unwrap();
        let pass_only = Manifest { capabilities: vec![Capability::UiApp, Capability::RenderPass], memory_maps: vec![pin.clone()], ..depth.clone() };
        assert!(pass_only.check().is_err(), "maps without render_depth or vars_memory");
        assert!(Manifest { api: 4, memory_maps: vec![pin], ..depth.clone() }.check().is_err(), "camera maps need API 5");
        let shader = Manifest { capabilities: vec![Capability::UiApp, Capability::RenderPass, Capability::RenderShader], ..depth };
        shader.check().unwrap();
        assert!(serde_json::to_string(&shader).unwrap().contains("\"render_shader\""));
        assert!(Manifest { capabilities: vec![Capability::UiApp, Capability::RenderShader], ..shader }.check().is_err(), "render_shader needs render_pass");
        let json = serde_json::to_string(&manifest()).unwrap();
        assert!(json.contains("\"games\":[\"ats\"]") && json.contains("\"vars_read\""), "{json}");
        let old: Manifest = serde_json::from_str(&json.replace(",\"multiplayer\":\"allowed\"", "")).unwrap();
        assert_eq!(old.multiplayer, Multiplayer::Allowed, "default when absent");
    }

    /// network_hosts: exact lower-case DNS names, at most 8, only with API 3 and the network
    /// capability; and the capability never comes without a host.
    #[test]
    fn network_host_rules() {
        let weather = Manifest { api: 3, capabilities: vec![Capability::UiApp, Capability::Network], network_hosts: vec!["tgftp.nws.noaa.gov".into(), "aviationweather.gov".into()], ..manifest() };
        weather.check().unwrap();
        let json = serde_json::to_string(&weather).unwrap();
        assert!(json.contains(r#""network_hosts":["tgftp.nws.noaa.gov","aviationweather.gov"]"#) && json.contains(r#""network""#), "{json}");
        assert!(!serde_json::to_string(&manifest()).unwrap().contains("network_hosts"), "absent when empty");
        for bad in ["TGFTP.nws.noaa.gov", "*.noaa.gov", "https://tgftp.nws.noaa.gov", "tgftp.nws.noaa.gov:443", "tgftp.nws.noaa.gov/data", "localhost", "127.0.0.1", "noaa..gov", "-x.gov", "x-.gov", "tgftp.nws.noaa.gov.", "", "a b.gov", "\u{e9}t\u{e9}.gov"] {
            assert!(Manifest { network_hosts: vec![bad.into()], ..weather.clone() }.check().is_err(), "{bad:?}");
        }
        let many: Vec<String> = (0..=MAX_NETWORK_HOSTS).map(|i| format!("h{i}.example.com")).collect();
        assert!(Manifest { network_hosts: many[..MAX_NETWORK_HOSTS].to_vec(), ..weather.clone() }.check().is_ok(), "8 hosts");
        assert!(Manifest { network_hosts: many, ..weather.clone() }.check().is_err(), "9 hosts");
        assert!(Manifest { network_hosts: vec!["a.gov".into(), "a.gov".into()], ..weather.clone() }.check().is_err(), "twice");
        assert!(Manifest { capabilities: vec![Capability::UiApp], ..weather.clone() }.check().is_err(), "hosts without the capability");
        assert!(Manifest { network_hosts: vec![], memory_maps: vec![], ..weather.clone() }.check().is_err(), "the capability without a host");
        assert!(Manifest { api: 2, ..weather.clone() }.check().is_err(), "network needs API 3");
    }

    #[test]
    fn control_rules() {
        let decl = |id: &str, default: Option<&str>| ControlDecl { id: id.into(), kind: fm_input::Kind::Button, label: "Toggle".into(), default: default.map(Into::into) };
        let m = Manifest { capabilities: vec![Capability::UiApp, Capability::Controls], controls: vec![decl("toggle", Some("ctrl+r")), decl("next", None)], ..manifest() };
        m.check().unwrap();
        let json: Manifest = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(json, m, "feature.json round trip");
        assert!(Manifest { controls: vec![], ..m.clone() }.check().is_err(), "the capability needs controls");
        assert!(Manifest { capabilities: vec![Capability::UiApp], ..m.clone() }.check().is_err(), "controls need the capability");
        assert!(Manifest { api: 4, ..m.clone() }.check().is_err(), "controls need API 5");
        assert!(Manifest { controls: vec![decl("toggle", None), decl("toggle", None)], ..m.clone() }.check().is_err(), "unique ids");
        assert!(Manifest { controls: vec![decl("Toggle", None)], ..m.clone() }.check().is_err(), "id rule");
        assert!(Manifest { controls: vec![decl("toggle", Some("button:1"))], ..m.clone() }.check().is_err(), "no joystick default");
        assert!(Manifest { controls: (0..=MAX_CONTROLS).map(|i| decl(&format!("c{i}"), None)).collect(), ..m.clone() }.check().is_err(), "too many");
        let old: Manifest = serde_json::from_str(&serde_json::to_string(&manifest()).unwrap()).unwrap();
        assert!(old.network_hosts.is_empty(), "an API 1 or 2 manifest has no hosts");
    }

    /// A catalog from a feed with a capability that this build does not know (the removed
    /// "keys_read", or a newer one) and the old "keys" field: the catalog still loads, and only
    /// that entry fails its check, so the app skips it and lists the others.
    #[test]
    fn an_unknown_capability_fails_only_its_catalog_entry() {
        let entry = |id: &str, caps: &[&str]| serde_json::json!({ "id": id, "name": id, "version": "0.1.0", "description": "d", "api": 2, "games": ["ats"], "capabilities": caps, "keys": [{ "id": "rain", "key": "R" }], "url": "x.zip", "sha256": "0", "size": 1 });
        let c: Catalog = serde_json::from_value(serde_json::json!({ "features": [entry("rain-key", &["keys_read"]), entry("fine", &["vars_read"])] })).unwrap();
        assert_eq!(c.features[0].manifest.capabilities, [Capability::Unknown]);
        let err = c.features[0].manifest.check().unwrap_err().to_string();
        assert!(err.contains("does not know"), "{err}");
        c.features[1].manifest.check().unwrap();
    }

    /// The direct powers (submission review plan R2) are known names that pass the check. An
    /// app before them reads each one as Unknown, so it refuses the feature (the test above).
    #[test]
    fn direct_powers_are_known_capabilities() {
        let names = ["direct_files", "direct_processes", "direct_registry", "direct_network", "load_code"];
        let caps: Vec<Capability> = serde_json::from_value(serde_json::json!(names)).unwrap();
        assert_eq!(caps, [Capability::DirectFiles, Capability::DirectProcesses, Capability::DirectRegistry, Capability::DirectNetwork, Capability::LoadCode]);
        let m = manifest();
        Manifest { capabilities: [m.capabilities.clone(), caps].concat(), ..m }.check().unwrap();
    }

    #[test]
    fn read_manifest_checks_the_package() {
        let json = serde_json::to_vec(&manifest()).unwrap();
        let lib = parts::library();
        let zip = crate::tests::zip_of(&[(crate::FEATURE_MANIFEST, &json), (&lib, b"x"), ("ui/index.html", b"x")]);
        let (m, found) = inspect(io::Cursor::new(zip)).unwrap();
        assert_eq!((m, found), (manifest(), vec!["bin".to_string(), "ui".into()]));
        let path = parts::library_path(std::path::Path::new("v"));
        assert_eq!(path.components().count(), 4, "v, bin, os, file: {}", path.display());
        assert!(!path.to_string_lossy().contains(if cfg!(windows) { '/' } else { '\\' }), "one separator: {}", path.display());
        assert!(read_manifest(io::Cursor::new(crate::tests::zip_of(&[(crate::FEATURE_MANIFEST, &json)]))).is_err(), "no library");
        let mods = crate::tests::zip_of(&[(crate::FEATURE_MANIFEST, &json), (&lib, b"x"), ("ats-mod/a/manifest.sii", b"x")]);
        assert!(inspect(io::Cursor::new(mods)).is_err(), "game files need game_files");
        assert!(read_manifest(io::Cursor::new(crate::tests::zip_of(&[("x", b"x")]))).is_err());
        let evil = serde_json::to_vec(&Manifest { id: "../../x".into(), ..manifest() }).unwrap();
        assert!(read_manifest(io::Cursor::new(crate::tests::zip_of(&[(crate::FEATURE_MANIFEST, &evil), (&lib, b"x")]))).is_err());
    }

    // Compiled shaders: only shaders/<name>.cso, at most SHADER_SIZE_MAX bytes, with
    // render_shader; pack refuses a shaders/ source folder that was not compiled.
    #[test]
    fn shaders_need_render_shader_and_valid_compiled_names() {
        let shader = Manifest { capabilities: vec![Capability::UiApp, Capability::RenderPass, Capability::RenderShader], api: FEATURE_API, ..manifest() };
        let json = serde_json::to_vec(&shader).unwrap();
        let lib = parts::library();
        let ok = crate::tests::zip_of(&[(crate::FEATURE_MANIFEST, &json), (&lib, b"x"), ("shaders/tint.cso", b"x")]);
        assert!(inspect(io::Cursor::new(ok)).unwrap().1.contains(&"shaders".to_string()));
        for bad in ["shaders/tint.hlsl", "shaders/Tint.cso", "shaders/a/b.cso", "shaders/.cso"] {
            let zip = crate::tests::zip_of(&[(crate::FEATURE_MANIFEST, &json), (&lib, b"x"), (bad, b"x")]);
            assert!(inspect(io::Cursor::new(zip)).is_err(), "{bad}");
        }
        let big = vec![0u8; fm_feature_sdk::abi::SHADER_SIZE_MAX + 1];
        assert!(inspect(io::Cursor::new(crate::tests::zip_of(&[(crate::FEATURE_MANIFEST, &json), (&lib, b"x"), ("shaders/big.cso", &big)]))).is_err(), "too large");
        let plain = serde_json::to_vec(&Manifest { capabilities: vec![Capability::UiApp, Capability::RenderPass], ..shader.clone() }).unwrap();
        assert!(inspect(io::Cursor::new(crate::tests::zip_of(&[(crate::FEATURE_MANIFEST, &plain), (&lib, b"x"), ("shaders/tint.cso", b"x")]))).is_err(), "needs render_shader");

        let src = std::env::temp_dir().join(format!("fm-install-shaders-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&src);
        std::fs::create_dir_all(src.join("ui")).unwrap();
        std::fs::write(src.join("ui").join("index.html"), b"x").unwrap();
        std::fs::write(src.join(crate::FEATURE_MANIFEST), &json).unwrap();
        let built = src.join("feature.dll");
        std::fs::write(&built, b"x").unwrap();
        let mut zip = io::Cursor::new(Vec::new());
        pack_shaders(&src, std::slice::from_ref(&built), &[("tint".into(), b"bytes".to_vec())], &mut zip).unwrap();
        zip.set_position(0);
        let mut a = zip::ZipArchive::new(zip).unwrap();
        let mut text = String::new();
        a.by_name("shaders/tint.cso").unwrap().read_to_string(&mut text).unwrap();
        assert_eq!(text, "bytes");
        std::fs::create_dir_all(src.join("shaders")).unwrap();
        assert!(pack(&src, std::slice::from_ref(&built), &mut io::Cursor::new(Vec::new())).is_err(), "uncompiled shaders/ is refused");
        assert!(pack_shaders(&src, &[built], &[("Bad".into(), b"x".to_vec())], &mut io::Cursor::new(Vec::new())).is_err(), "bad name");
        let _ = std::fs::remove_dir_all(&src);
    }

    fn entry(version: &str, sha: &str) -> CatalogEntry {
        CatalogEntry { manifest: Manifest { version: version.into(), ..manifest() }, url: format!("files/speed-alert-{version}.zip"), sha256: sha.into(), size: 1, parts: vec![], art: None, history: None, signature: None, channel: Default::default() }
    }

    // Release-admin item 6 on the real feed types: a stable entry and a manifest without min_app
    // write no new field (legacy bytes), the channel survives both flatten levels (catalog entry
    // and history entry), an unknown channel reads as Unknown, and a bad min_app fails check().
    #[test]
    fn release_fields_on_the_feature_feed_types() {
        let stable = entry("1.0.0", "a");
        let json = serde_json::to_string(&stable).unwrap();
        assert!(!json.contains("channel") && !json.contains("min_app"), "{json}");
        assert_eq!(serde_json::from_str::<CatalogEntry>(&json).unwrap(), stable);
        let preview = CatalogEntry { channel: crate::release::Channel::Preview, manifest: Manifest { min_app: Some("0.2.0-preview.1".into()), ..stable.manifest.clone() }, ..stable.clone() };
        let json = serde_json::to_string(&preview).unwrap();
        assert!(json.contains(r#""channel":"preview""#) && json.contains(r#""min_app":"0.2.0-preview.1""#), "{json}");
        assert_eq!(serde_json::from_str::<CatalogEntry>(&json).unwrap(), preview);
        let history = HistoryEntry { entry: preview.clone(), published: 5, withdrawn: false };
        let json = serde_json::to_string(&history).unwrap();
        assert_eq!(serde_json::from_str::<HistoryEntry>(&json).unwrap(), history);
        let nightly = json.replace(r#""channel":"preview""#, r#""channel":"nightly""#);
        assert_eq!(serde_json::from_str::<HistoryEntry>(&nightly).unwrap().entry.channel, crate::release::Channel::Unknown);
        let nightly = serde_json::to_string(&preview).unwrap().replace("preview\"", "nightly\"");
        assert_eq!(serde_json::from_str::<CatalogEntry>(&nightly).unwrap().channel, crate::release::Channel::Unknown);
        preview.manifest.check().unwrap();
        assert!(Manifest { min_app: Some("0.2".into()), ..manifest() }.check().is_err());
    }

    fn versions(h: &History) -> Vec<&str> {
        h.versions.iter().map(|v| v.entry.manifest.version.as_str()).collect()
    }

    // Every version stays, newest first by semantic version (1.10.0 after 1.9.0), whatever the
    // publish order; a retry with the same bytes changes nothing; other bytes are refused.
    #[test]
    fn the_history_keeps_every_version_newest_first() {
        let mut h = History::default();
        assert_eq!(h.add(entry("1.9.0", "a"), 10), Ok(true));
        assert_eq!(h.add(entry("1.10.0", "b"), 20), Ok(true));
        assert_eq!(h.add(entry("1.2.0", "c"), 30), Ok(true), "an older hotfix published later");
        assert_eq!(versions(&h), ["1.10.0", "1.9.0", "1.2.0"]);
        assert_eq!(h.add(entry("1.9.0", "a"), 40), Ok(false), "retry");
        assert!(h.add(entry("1.9.0", "x"), 50).unwrap_err().contains("new version"));
        assert_eq!(h.versions.iter().find(|v| v.entry.manifest.version == "1.9.0").unwrap().published, 10);
        assert_eq!(h.latest().unwrap().manifest.version, "1.10.0");
    }

    // A withdrawn version stays in the file but is not the latest; with all withdrawn there is
    // no latest. An unknown version cannot be withdrawn.
    #[test]
    fn a_withdrawn_version_is_kept_but_never_the_latest() {
        let mut h = History::default();
        h.add(entry("1.0.0", "a"), 1).unwrap();
        h.add(entry("2.0.0", "b"), 2).unwrap();
        h.withdraw("2.0.0").unwrap();
        assert_eq!(versions(&h), ["2.0.0", "1.0.0"]);
        assert_eq!(h.latest().unwrap().manifest.version, "1.0.0");
        h.withdraw("1.0.0").unwrap();
        assert!(h.latest().is_none());
        assert!(h.withdraw("3.0.0").is_err());
    }

    // Old and new apps read each other's files: a catalog with no history field parses (None),
    // and an entry with one keeps the manifest fields; the history file round-trips, and a
    // version that is not withdrawn writes no "withdrawn" field.
    #[test]
    fn catalog_and_history_files_stay_compatible() {
        let old = serde_json::json!({ "features": [serde_json::to_value(entry("1.0.0", "a")).unwrap()] });
        assert!(old["features"][0].get("history").is_none());
        let c: Catalog = serde_json::from_value(old).unwrap();
        assert_eq!(c.features[0].history, None);
        let new = CatalogEntry { history: Some(history_path("speed-alert")), ..entry("1.0.0", "a") };
        let text = serde_json::to_string(&new).unwrap();
        assert!(text.contains(r#""history":"speed-alert/versions.json""#));
        let manifest_only: Manifest = serde_json::from_str(&text).unwrap();
        assert_eq!(manifest_only.version, "1.0.0", "an older app reads the entry as a manifest and ignores history");
        let mut h = History::default();
        h.add(entry("1.0.0", "a"), 7).unwrap();
        let file = serde_json::to_string(&h).unwrap();
        assert!(!file.contains("withdrawn") && file.contains(r#""published":7"#));
        assert_eq!(serde_json::from_str::<History>(&file).unwrap(), h);
        assert!(!file.contains("signature"), "an unsigned entry writes no signature field");
        let signed = serde_json::to_string(&CatalogEntry { signature: Some("s".into()), ..entry("1.0.0", "a") }).unwrap();
        assert_eq!(serde_json::from_str::<Manifest>(&signed).unwrap().version, "1.0.0", "an older app ignores the signature");
    }

    // Publishing pipeline L1: a feed package needs a signature of its own bytes for its own
    // version with the given key. No signature, a changed byte, another key or another version
    // is refused. The shipped feed key parses (a wrong file would refuse every package).
    #[test]
    fn a_feed_package_needs_its_signature() {
        let key = crate::test_signing::TestKey::default();
        let bytes = b"package bytes";
        let mut e = entry("1.0.0", "a");
        assert!(verify_package(bytes, &e, &key.public()).unwrap_err().contains("no Feature Machine signature"));
        e.signature = Some(key.sign(bytes, "1.0.0"));
        verify_package(bytes, &e, &key.public()).unwrap();
        assert!(verify_package(b"package byteZ", &e, &key.public()).is_err());
        assert!(verify_package(bytes, &e, &crate::test_signing::TestKey::default().public()).is_err());
        assert!(verify_package(bytes, &e, feed_key()).unwrap_err().contains("not valid"), "the feed key parses and did not sign it");
        e.signature = Some(key.sign(bytes, "1.0.1"));
        assert!(verify_package(bytes, &e, &key.public()).unwrap_err().contains("signed for version 1.0.1"));
    }

    // A feed from before signing: the same bytes again with a signature sign the old entry; a
    // retry without one changes nothing, and a second signature never replaces the first.
    #[test]
    fn the_same_bytes_with_a_signature_sign_an_old_entry() {
        let mut h = History::default();
        assert_eq!(h.add(entry("1.0.0", "a"), 1), Ok(true));
        let signed = |s: &str| CatalogEntry { signature: Some(s.into()), ..entry("1.0.0", "a") };
        assert_eq!(h.add(signed("s"), 2), Ok(true));
        assert_eq!((h.versions[0].entry.signature.as_deref(), h.versions[0].published), (Some("s"), 1));
        assert_eq!(h.add(entry("1.0.0", "a"), 3), Ok(false));
        assert_eq!(h.add(signed("t"), 3), Ok(false));
        assert_eq!(h.versions[0].entry.signature.as_deref(), Some("s"));
        assert!(h.add(CatalogEntry { sha256: "x".into(), ..signed("u") }, 4).is_err(), "other bytes stay refused");
    }
}
