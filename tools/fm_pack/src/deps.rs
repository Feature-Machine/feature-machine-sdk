// Dependency gate (submission review plan, R1 to R8, S2). `fm_pack build` lists each package of
// the feature (build.json "packages") and refuses code that we cannot observe: another registry,
// a git source that is not https, a committed binary, a closed binary that Feature Machine did
// not approve. `fm_pack deps <build dir>` then asks the dependency ledger (fm-api), crates.io and
// OSV, and decides for each package: reputation, reviewed before, or read by the AI review.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The crates.io registry as cargo names it in Cargo.lock and `cargo metadata`.
pub const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";
/// The closed-source binaries of a feature, next to its feature.json (R5).
pub const BINARY_LOCK: &str = "binaries.json";
/// The closed-source binaries that Feature Machine approved (R4: Feature Machine approves each first use).
const APPROVED: &str = include_str!("../review/binaries.approved.json");
/// The crates of the SDK repository (tools/sdk-export/export.mjs). In a local build (path SDK)
/// they are path packages of this repository, not creator code.
const SDK_CRATES: [&str; 6] = ["fm_feature_sdk", "fm_input", "fm_install", "fm_memory", "fm_paths", "fm_platform"];

/// Reputation floors (R7, 2026-10-29). A crates.io package that misses one is read by the
/// AI review (R6), not refused.
pub const MIN_DOWNLOADS: u64 = 10_000;
pub const MIN_CRATE_DAYS: i64 = 30;
pub const MIN_VERSION_DAYS: i64 = 7;
/// crates.io asks for one request per second and a user agent that names the tool.
const USER_AGENT: &str = concat!("fm_pack/", env!("CARGO_PKG_VERSION"), " (Feature Machine submission review; https://github.com/Feature-Machine/feature-machine-sdk)");
/// The largest closed-source binary that a build fetches.
const MAX_BINARY: u64 = 512 << 20;

/// One package of the build, from cargo's resolution for this feature.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub kind: Kind,
    /// creator: the folder relative to the repository root ("." for the root); git: "<url>#<commit>".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// crates.io: the SHA-256 of the `.crate` from Cargo.lock. With the name and the version it is
    /// the key of the reviewed code in the dependency ledger (plan S2 step 1): the same name and
    /// version from another registry copy has another checksum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Code of the submitted repository: the AI reads it.
    Creator,
    /// The Feature Machine SDK at the tag that the build checked (build.rs sdk_check).
    Sdk,
    /// A crates.io package: reputation decides (R3).
    CratesIo,
    /// A public git repository, pinned to a commit: the AI reads it (R8).
    Git,
}

/// One package of the build and the folder of its Cargo.toml on this machine. The folder is for
/// the machine code scan only; build.json gets the package without it (no local path).
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved { pub package: Package, pub dir: PathBuf }

/// The packages that build or link into the feature. `tree` is `cargo tree --locked -e
/// normal,build --target <host> --prefix none -f {p}` of the feature (features of this package
/// only, as `cargo build`; no depth digits, so a name can start with a digit, review r5); `meta` is `cargo metadata --locked`, for the full sources. `sdk` is
/// the SDK of build.json (null for a path SDK).
pub fn packages(tree: &str, meta: &serde_json::Value, src: &Path, sdk: &serde_json::Value) -> Result<Vec<Resolved>, String> {
    let all = meta["packages"].as_array().ok_or("cargo metadata has no packages")?;
    let mut out = Vec::new();
    for line in tree.lines() {
        // cargo writes `name v<version> [(proc-macro)] [(<source>)] [(*)]` (review r1 P2). Read the
        // marks only at their places, so a path that holds the same text stays whole (review r7).
        let entry = line.trim_end();
        let Some((name, rest)) = entry.split_once(" v") else { continue };
        let (version, rest) = rest.split_once(' ').unwrap_or((rest, ""));
        let rest = rest.strip_prefix("(proc-macro)").unwrap_or(rest).trim_start();
        let rest = rest.strip_suffix("(*)").unwrap_or(rest).trim_end();
        let shown = match rest {
            "" => None,
            s => Some(s.strip_prefix('(').and_then(|s| s.strip_suffix(')')).ok_or_else(|| format!("cargo tree: cannot read `{entry}`"))?),
        };
        let found: Vec<&serde_json::Value> = all.iter().filter(|p| p["name"] == name && p["version"] == version).collect();
        let pick = |want: &dyn Fn(&serde_json::Value) -> bool| match found.iter().copied().filter(|p| want(p)).collect::<Vec<_>>()[..] {
            [one] => Ok(one),
            _ => Err(format!("cargo metadata has no single package for `{entry}`")),
        };
        let p = match shown {
            None => pick(&|p| p["source"] == CRATES_IO).or_else(|_| pick(&|_| true))?,
            Some(s) if Path::new(s).is_absolute() => pick(&|p| p["source"].is_null() && p["manifest_path"].as_str().and_then(|m| Path::new(m).parent()).is_some_and(|d| same_dir(d, Path::new(s))))?,
            Some(s) => match s.rsplit_once('#') {
                Some((url, short)) => pick(&|p| p["source"].as_str().and_then(|x| x.strip_prefix(&format!("git+{url}#"))).is_some_and(|c| c.starts_with(short)))?,
                None => pick(&|_| true)?,
            },
        };
        let dir = p["manifest_path"].as_str().and_then(|m| Path::new(m).parent()).ok_or_else(|| format!("{name}: no manifest path"))?;
        out.push(Resolved { package: classify(name, version, p, src, sdk)?, dir: dir.to_path_buf() });
    }
    out.sort_by(|a, b| a.package.cmp(&b.package));
    out.dedup_by(|a, b| a.package == b.package);
    if out.is_empty() { return Err("cargo tree listed no package".into()); }
    Ok(out)
}

/// The checksum of each crates.io package from Cargo.lock (`lock`, version 3 or 4). A crates.io
/// package with no checksum stops the build: the ledger key needs it, and cargo always writes one
/// for a registry package in these lock versions.
pub fn with_checksums(mut packages: Vec<Package>, lock: &Path) -> Result<Vec<Package>, String> {
    let text = std::fs::read_to_string(lock).map_err(|e| format!("{}: {e}", lock.display()))?;
    let table: toml::Table = toml::from_str(&text).map_err(|e| format!("Cargo.lock: {e}"))?;
    let mut sums = std::collections::BTreeMap::new();
    for p in table.get("package").and_then(|p| p.as_array()).into_iter().flatten() {
        let field = |k: &str| p.get(k).and_then(|v| v.as_str());
        if let (Some(n), Some(v), Some(CRATES_IO), Some(c)) = (field("name"), field("version"), field("source"), field("checksum")) {
            sums.insert((n.to_string(), v.to_string()), c.to_ascii_lowercase());
        }
    }
    for p in packages.iter_mut().filter(|p| p.kind == Kind::CratesIo) {
        let sum = sums.get(&(p.name.clone(), p.version.clone())).ok_or_else(|| format!("Cargo.lock has no checksum for {} {}; commit a Cargo.lock of version 3 or 4", p.name, p.version))?;
        if sum.len() != 64 || !sum.bytes().all(|b| b.is_ascii_hexdigit()) { return Err(format!("Cargo.lock: the checksum of {} {} is not a SHA-256", p.name, p.version)); }
        p.checksum = Some(sum.clone());
    }
    Ok(packages)
}

/// Whether a URL holds user information ("https://user:secret@host/x"): credentials that let a
/// clean runner fetch private code, and that must never reach a log or a brief (review r2 P1).
fn has_userinfo(url: &str) -> bool {
    url.split_once("://").is_some_and(|(_, rest)| rest.split(['/', '?', '#']).next().is_some_and(|a| a.contains('@')))
}

/// A URL for messages, with its user information replaced by "***".
pub(crate) fn redact(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) if has_userinfo(url) => {
            let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            let host = rest[..end].rsplit_once('@').map_or(&rest[..end], |(_, h)| h);
            format!("{scheme}://***@{host}{}", &rest[end..])
        }
        _ => url.to_string(),
    }
}

/// The kind of one resolved package, or why the build refuses its source (R1: we must be able
/// to read it, R8: git over https only, so a runner with no credentials can fetch only public code).
fn classify(name: &str, version: &str, p: &serde_json::Value, src: &Path, sdk: &serde_json::Value) -> Result<Package, String> {
    let package = |kind, source| Package { name: name.into(), version: version.into(), kind, source, checksum: None };
    match p["source"].as_str() {
        None => {
            let dir = p["manifest_path"].as_str().and_then(|m| Path::new(m).parent()).ok_or_else(|| format!("{name}: no manifest path"))?;
            let rel = relative(dir, src).ok_or_else(|| format!("{name} {version} is a path dependency outside the repository"))?;
            match sdk.is_null() && SDK_CRATES.contains(&name) {
                true => Ok(package(Kind::Sdk, None)),
                false => Ok(package(Kind::Creator, Some(rel))),
            }
        }
        Some(CRATES_IO) => Ok(package(Kind::CratesIo, None)),
        Some(s) if s.starts_with("git+") => {
            let (url, commit) = s["git+".len()..].rsplit_once('#').ok_or_else(|| format!("{name}: git source {} has no commit", redact(s)))?;
            let url = url.split_once('?').map_or(url, |(u, _)| u);
            if !url.starts_with("https://") { return Err(format!("{name} {version} comes from {}; a git dependency must use https (a public repository)", redact(url))); }
            if has_userinfo(url) { return Err(format!("{name} {version} comes from {}; a git URL must not hold credentials (a public repository needs none)", redact(url))); }
            // The SDK repository is private, and the review job can read it: only the verified SDK
            // commit comes from it; another commit would pass as a public git dependency (r3).
            if crate::build::is_sdk_url(url) {
                return match sdk["commit"].as_str() == Some(commit) {
                    true => Ok(package(Kind::Sdk, None)),
                    false => Err(format!("{name} {version} comes from {url} at commit {commit}; from the SDK repository, a feature uses only the commit of its fm_feature_sdk release")),
                };
            }
            Ok(package(Kind::Git, Some(format!("{url}#{commit}"))))
        }
        Some(s) => Err(format!("{name} {version} comes from {}; a dependency comes from crates.io, a public git repository or the repository itself", redact(s))),
    }
}

/// R8 for each git source of Cargo.lock: `cargo vendor` fetches the whole lock (also optional
/// packages that the build does not use) and their submodules with the review job's git, which
/// has the SDK token for all of github.com (review r4, r5). So a source in the SDK repository
/// must be the SDK commit, and every other one must come at its commit, with no submodule, to a
/// git with no credential and no config of this machine. `scratch` holds those fetches.
pub(crate) fn locked_git(lock: &Path, sdk: &serde_json::Value, scratch: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(lock).map_err(|e| format!("{}: {e}", lock.display()))?;
    let table: toml::Table = toml::from_str(&text).map_err(|e| format!("Cargo.lock: {e}"))?;
    let mut seen = std::collections::BTreeSet::new();
    for p in table.get("package").and_then(|p| p.as_array()).into_iter().flatten() {
        let Some(s) = p.get("source").and_then(|s| s.as_str()).and_then(|s| s.strip_prefix("git+")) else { continue };
        let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("?");
        let (url, commit) = s.rsplit_once('#').ok_or_else(|| format!("{name}: git source {} has no commit", redact(s)))?;
        let url = url.split_once('?').map_or(url, |(u, _)| u);
        if !url.starts_with("https://") || has_userinfo(url) { return Err(format!("{name} comes from {}; a git dependency must use https with no credentials (a public repository)", redact(url))); }
        if crate::build::is_sdk_url(url) {
            if sdk["commit"].as_str() != Some(commit) { return Err(format!("{name} comes from {url} at commit {commit}; from the SDK repository, a feature uses only the commit of its fm_feature_sdk release")); }
            continue;
        }
        if !seen.insert((url.to_string(), commit.to_string())) { continue; }
        let dir = scratch.join(seen.len().to_string());
        let links = anonymous_fetch(url, commit, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        match links {
            Some(0) => {}
            Some(_) => return Err(format!("{name} comes from {} at {commit}, which has git submodules; a git dependency must have none (R8)", redact(url))),
            None => return Err(format!("{name} comes from {} at {commit}, which a git with no credential cannot fetch; a git dependency must be a public repository (R8)", redact(url))),
        }
    }
    let _ = std::fs::remove_dir(scratch);
    Ok(())
}

/// The number of submodule links (mode 160000) in `commit` of `url`, fetched by `anonymous_git`
/// into the new bare repository `dir`; None when a git command fails.
fn anonymous_fetch(url: &str, commit: &str, dir: &Path) -> Option<usize> {
    let run = |args: &[&str]| anonymous_git().args(args).output().ok().filter(|o| o.status.success());
    let d = dir.to_string_lossy();
    run(&["init", "--quiet", "--bare", &d])?;
    run(&["-C", &d, "fetch", "--quiet", "--depth", "1", "--no-tags", url, commit])?;
    let tree = run(&["-C", &d, "ls-tree", "-r", commit])?;
    Some(String::from_utf8_lossy(&tree.stdout).lines().filter(|l| l.starts_with("160000 ")).count())
}

/// git with no GIT_* variable of the caller (GIT_CONFIG_* holds the review job's header), no
/// system or global config, no credential helper and no prompt.
fn anonymous_git() -> std::process::Command {
    let mut c = std::process::Command::new("git");
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().to_ascii_uppercase().starts_with("GIT_") { c.env_remove(k); }
    }
    c.env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_TERMINAL_PROMPT", "0")
        .args(["-c", "credential.helper=", "-c", "core.askPass="]).stdin(std::process::Stdio::null());
    c
}

/// Whether two folders are the same, also when one spelling differs (canonical paths).
fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

/// `dir` relative to `root` with "/" separators, "." for the root; None outside it.
fn relative(dir: &Path, root: &Path) -> Option<String> {
    let rel = dir.strip_prefix(root).ok().map(Path::to_path_buf)
        .or_else(|| std::fs::canonicalize(dir).ok().zip(std::fs::canonicalize(root).ok()).and_then(|(d, r)| d.strip_prefix(r).ok().map(Path::to_path_buf)))?;
    let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    Some(if parts.is_empty() { ".".into() } else { parts.join("/") })
}

/// A file that holds machine code, from its first bytes: what the object reader knows (COFF and
/// bigobj objects, static libraries, ELF, Mach-O, XCOFF), plus "MZ" (DOS and PE), WebAssembly and
/// LLVM bitcode (raw "BC\xC0\xDE" and the 0x0B17C0DE wrapper, review r3 P1) and clang
/// precompiled headers ("CPCH", r4) by their magic. Such a file is code that nobody can read (R1);
/// a closed-source part goes in binaries.json (R5). Limit: data that build code turns into code
/// (an archive, an encoded blob) has no magic; the AI read of the build code finds that
/// (policy rule 7), because no scan can (r4: the export also holds normal ZIP files).
fn is_binary(head: &[u8]) -> bool {
    const MAGIC: [&[u8]; 9] = [b"MZ", b"\x7fELF", b"!<arch>\n", b"!<thin>\n", b"\0asm", b"\xca\xfe\xba\xbe", b"BC\xc0\xde", b"\xde\xc0\x17\x0b", b"CPCH"];
    MAGIC.iter().any(|m| head.starts_with(m)) || object::FileKind::parse(head).is_ok()
}

/// The first bytes of a file that `is_binary` reads: more than the longest header it checks.
const HEAD: usize = 4096;

/// The files that hold machine code under `root`, as (path relative to `root`, full path).
fn machine_code(root: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let mut found = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for e in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let e = e.map_err(|e| e.to_string())?;
            if e.file_type().map_err(|e| e.to_string())?.is_dir() { dirs.push(e.path()); continue; }
            let mut head = Vec::with_capacity(HEAD);
            let file = std::fs::File::open(e.path()).map_err(|x| format!("{}: {x}", e.path().display()))?;
            std::io::Read::read_to_end(&mut std::io::Read::take(file, HEAD as u64), &mut head).map_err(|e| e.to_string())?;
            if is_binary(&head) { found.push((relative(&e.path(), root).unwrap_or_default(), e.path())); }
        }
    }
    found.sort();
    Ok(found)
}

/// Refuses a committed binary anywhere in the export of the commit: build.rs or a link argument
/// can reach any file of the repository, not only the folders of its packages (review r1 P1).
pub fn committed_binaries(src: &Path) -> Result<(), String> {
    let found: Vec<String> = machine_code(src)?.into_iter().map(|(rel, _)| rel).collect();
    match found.is_empty() {
        true => Ok(()),
        false => Err(format!("the commit holds machine code that nobody can read: {}; build it from source, or declare a closed-source binary in {BINARY_LOCK}", found.join(", "))),
    }
}

/// A file with machine code in a crates.io package (for example the import libraries of
/// windows_x86_64_msvc). It is a closed-source part: approved by its SHA-256 for the publisher
/// "crates.io <name> <version>", and named in the brief.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Prebuilt {
    pub name: String,
    pub version: String,
    /// The file, relative to the package folder, with "/" separators.
    pub file: String,
    pub sha256: String,
}

impl Prebuilt {
    /// The publisher of its approval: the registry and the exact package version.
    pub fn publisher(&self) -> String { format!("crates.io {} {}", self.name, self.version) }
}

/// The machine code of the dependencies (review r2 P1): their build scripts can link it, and the
/// scan of the commit does not see their folders. A git dependency is code that the AI reads
/// (R8), so machine code in its checkout stops the build. Each file of a crates.io package goes
/// to build.json "prebuilt"; `prebuilt_check` decides on it with the gate's facts (R19).
pub fn dependency_binaries(resolved: &[Resolved]) -> Result<Vec<Prebuilt>, String> {
    let (mut out, mut bad) = (Vec::new(), Vec::new());
    // Packages of one git repository share one checkout: scan it once (review r3 P3).
    let mut scanned = std::collections::BTreeSet::new();
    for r in resolved {
        let p = &r.package;
        match p.kind {
            Kind::Git => {
                let root = checkout_root(&r.dir)?;
                if !scanned.insert(std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone())) { continue; }
                let found = machine_code(&root)?;
                if !found.is_empty() {
                    let files: Vec<String> = found.into_iter().map(|(rel, _)| rel).collect();
                    bad.push(format!("the git dependency {} {} holds machine code that nobody can read: {}", p.name, p.version, files.join(", ")));
                }
            }
            Kind::CratesIo => {
                for (file, path) in machine_code(&r.dir)? {
                    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                    out.push(Prebuilt { name: p.name.clone(), version: p.version.clone(), file, sha256: crate::sha256_hex(&bytes) });
                }
            }
            Kind::Creator | Kind::Sdk => {}
        }
    }
    match bad.is_empty() {
        true => Ok(out),
        false => Err(bad.join("; ")),
    }
}

/// Whether each prebuilt file may ship (R19, 2026-10-29): a crate that passed the
/// reputation floors brings its files with it, as its Rust code (R3); the brief names them. In a
/// crate below a floor the AI reads the code but cannot read the file, so it needs an approval
/// by SHA-256 for "crates.io <name> <version>" (R4). `fm_pack deps` and `review-brief` both ask.
pub fn prebuilt_check(checked: &[Checked], prebuilt: &[Prebuilt]) -> Result<(), String> {
    prebuilt_check_with(checked, prebuilt, APPROVED)
}

fn prebuilt_check_with(checked: &[Checked], prebuilt: &[Prebuilt], approved: &str) -> Result<(), String> {
    let approved = approvals(approved)?;
    let bad: Vec<String> = prebuilt.iter().filter_map(|x| {
        let basis = checked.iter().find(|c| c.package.kind == Kind::CratesIo && c.package.name == x.name && c.package.version == x.version).map(|c| c.basis);
        // A file of a crate that is not in the list never passes, also with an approval (r1 P3).
        let ok = basis == Some(Basis::Reputation) || (basis.is_some() && approved.iter().any(|a| a.sha256.eq_ignore_ascii_case(&x.sha256) && a.publisher == x.publisher()));
        (!ok).then(|| match basis {
            None => format!("{} in {}: the crate is not in the dependency list", x.file, x.publisher()),
            Some(_) => format!("{} in {} (SHA-256 {}) is machine code in a crate below the reputation floors; the review cannot read it, so its first use needs an approval", x.file, x.publisher(), x.sha256),
        })
    }).collect();
    match bad.is_empty() {
        true => Ok(()),
        false => Err(bad.join("; ")),
    }
}

/// The root of the git checkout that holds a package: cargo checks out each git source at
/// <cargo home>/git/checkouts/<repository>-<hash>/<commit>/, and a build script can reach any
/// file of it. An unknown layout stops the build rather than scan too little.
fn checkout_root(dir: &Path) -> Result<PathBuf, String> {
    dir.ancestors().find(|a| a.parent().and_then(Path::parent).and_then(Path::file_name).is_some_and(|n| n == "checkouts"))
        .map(Path::to_path_buf).ok_or_else(|| format!("{}: not in a cargo git checkout", dir.display()))
}

/// One closed-source binary of binaries.json (R5): where the build fetches it, its version,
/// SHA-256 and publisher, for the member brief.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Binary {
    /// The file name in the build's binaries folder (FM_BINARIES_DIR for build.rs).
    pub file: String,
    /// The publisher's version of the file (R5), for the member brief.
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub publisher: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BinaryLock { binaries: Vec<Binary> }

/// One approval of binaries.approved.json: the exact bytes (SHA-256) and the publisher. The date
/// and the note are for people (when and why), so the gate does not read them.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct Approval { sha256: String, publisher: String, approved: String, note: String }

/// The approvals of binaries.approved.json (R4).
fn approvals(text: &str) -> Result<Vec<Approval>, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Approved { binaries: Vec<Approval> }
    serde_json::from_str::<Approved>(text).map(|a| a.binaries).map_err(|e| format!("binaries.approved.json: {e}"))
}

/// The binaries of `feature_dir`/binaries.json, each one checked and approved. No file: none.
pub fn binary_lock(feature_dir: &Path) -> Result<Vec<Binary>, String> {
    binary_lock_with(feature_dir, APPROVED)
}

fn binary_lock_with(feature_dir: &Path, approved: &str) -> Result<Vec<Binary>, String> {
    let text = match std::fs::read_to_string(feature_dir.join(BINARY_LOCK)) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(format!("{BINARY_LOCK}: {e}")),
    };
    let lock: BinaryLock = serde_json::from_str(&text).map_err(|e| format!("{BINARY_LOCK}: {e}"))?;
    let approved = approvals(approved)?;
    let mut names = std::collections::BTreeSet::new();
    for b in &lock.binaries {
        let plain = !b.file.is_empty() && !b.file.contains(['/', '\\', ':']) && !b.file.starts_with('.');
        if !plain || !names.insert(b.file.to_ascii_lowercase()) { return Err(format!("{BINARY_LOCK}: {:?} is not one plain file name", b.file)); }
        if b.version.trim().is_empty() { return Err(format!("{BINARY_LOCK}: {} has no version", b.file)); }
        if !b.url.starts_with("https://") || has_userinfo(&b.url) { return Err(format!("{BINARY_LOCK}: {} must come from an https URL with no credentials", b.file)); }
        if b.sha256.len() != 64 || !b.sha256.bytes().all(|c| c.is_ascii_hexdigit()) { return Err(format!("{BINARY_LOCK}: {} has no SHA-256", b.file)); }
        if b.publisher.trim().is_empty() { return Err(format!("{BINARY_LOCK}: {} names no publisher", b.file)); }
        if !approved.iter().any(|a| a.sha256.eq_ignore_ascii_case(&b.sha256) && a.publisher == b.publisher) {
            return Err(format!("{} ({}, SHA-256 {}) is a closed-source binary that Feature Machine has not approved; the first use of each one needs an approval", b.file, b.publisher, b.sha256));
        }
    }
    Ok(lock.binaries)
}

/// An HTTP client whose requests stop after `seconds`, body included: ureq has no timeout by
/// default, and a stalled server must not hold a build or the publish queue (R19 review r2).
fn http(seconds: u64) -> ureq::Agent {
    ureq::Agent::new_with_config(ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(seconds))).build())
}

/// Fetches each binary into `dir` and checks its SHA-256 before it is written.
pub fn fetch_binaries(binaries: &[Binary], dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    // Up to MAX_BINARY (512 MiB) for each file: 10 minutes is 0.9 MB/s or more.
    let agent = http(600);
    for b in binaries {
        let mut answer = agent.get(&b.url).header("User-Agent", USER_AGENT).call().map_err(|e| format!("{}: {e}", b.url))?;
        let bytes = answer.body_mut().with_config().limit(MAX_BINARY).read_to_vec().map_err(|e| format!("{}: {e}", b.url))?;
        check_bytes(b, &bytes)?;
        std::fs::write(dir.join(&b.file), bytes).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn check_bytes(b: &Binary, bytes: &[u8]) -> Result<(), String> {
    match crate::sha256_hex(bytes).eq_ignore_ascii_case(&b.sha256) {
        true => Ok(()),
        false => Err(format!("{} from {} is not the approved file (SHA-256)", b.file, b.url)),
    }
}

/// What crates.io and OSV say about one package version.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Facts {
    /// All downloads of the crate.
    pub downloads: u64,
    /// When the crate and this version were first published (RFC 3339).
    pub published: String,
    pub version_published: String,
    pub yanked: bool,
    /// OSV advisory ids for this version (RUSTSEC-, GHSA-, MAL- for malicious packages).
    pub advisories: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// The AI reads it: creator code (R2), a git dependency (R8) or a crate below a floor (R6).
    Read,
    /// A package that the AI would read, whose exact code an approved review read before: the
    /// ledger entry gives its powers (plan S2 step 5). A new advisory or a yank sends it to Read.
    Reviewed,
    /// The SDK of the tag that the build checked.
    Sdk,
    /// crates.io reputation: every floor of R7 passed.
    Reputation,
    /// Not on crates.io, or a malicious-package advisory: the submission stops.
    Refused,
}

/// One package of dependencies.json: the package, the decision and why.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Checked {
    pub package: Package,
    pub basis: Basis,
    /// The floors that a Read crate missed, or why it is refused; empty otherwise.
    pub why: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts: Option<Facts>,
    /// The ledger entry of this exact code, when the ledger has an active one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger: Option<Entry>,
    /// The entry of an older version that a diff read starts from (`diff_base`): the AI reads
    /// only what changed since that code. Its `.crate` is in `previous/` (`previous_crate`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<Entry>,
    /// Feature Machine marked this exact code for a new read (R17): a full read, no diff base.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub re_review: bool,
}

/// What fm-api's ledger route gives for the keys of a build: the active entries of exactly those
/// keys, the active entries of other versions of the same crates (`diff_base`), and the keys in
/// Re-review (`marked`). Empty with no FM_API.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ledger {
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub previous: Vec<Entry>,
    #[serde(default)]
    pub marked: Vec<Entry>,
}

/// The decision of one package with the ledger (pure, for `command`): only an entry that decided
/// the basis stays, so dependencies.json shows what counted. Marked code has no ledger base: a
/// mark wins over an entry of the same key, so even a mixed answer gives a full read.
pub fn check_one(package: Package, facts: Option<Facts>, l: &Ledger, now: time::OffsetDateTime) -> Checked {
    let re_review = l.marked.iter().any(|e| e.is_for(&package));
    let ledger = l.entries.iter().find(|e| !re_review && e.is_for(&package)).cloned();
    let (basis, why) = decide(&package, facts.as_ref(), ledger.as_ref(), now);
    let ledger = ledger.filter(|_| basis == Basis::Reviewed);
    let previous = if re_review { None } else { diff_base(&package, facts.as_ref(), basis, &l.previous).cloned() };
    Checked { package, basis, why, facts, ledger, previous, re_review }
}

/// One active entry of the dependency ledger (fm-releases `dependency_reviews`, read through
/// fm-api `/v1/review/ledger`): code that a review read and Feature Machine approved, with the powers that
/// the review recorded (verdict `dependencies` rows). `source` is the key part after the name and
/// version: the Cargo.lock checksum for crates.io, "<url>#<commit>" for git.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub kind: Kind,
    pub name: String,
    pub version: String,
    pub source: String,
    pub powers: Vec<String>,
    pub reaches: Vec<String>,
    /// The review run that read the code (fm-releases `review_run`).
    pub run: String,
}

/// The ledger key of a package: crates.io with its checksum, git with its url#commit. Creator
/// and SDK code has no key: each submission reads the creator code again.
pub fn ledger_key(p: &Package) -> Option<(Kind, &str, &str, &str)> {
    let source = match p.kind {
        Kind::CratesIo => p.checksum.as_deref()?,
        Kind::Git => p.source.as_deref()?,
        Kind::Creator | Kind::Sdk => return None,
    };
    Some((p.kind, p.name.as_str(), p.version.as_str(), source))
}

impl Entry {
    /// Whether the entry is for the exact code of `p` (all four key parts).
    pub fn is_for(&self, p: &Package) -> bool {
        ledger_key(p) == Some((self.kind, self.name.as_str(), self.version.as_str(), self.source.as_str()))
    }
}

/// dependencies.json: the decisions for every package of build.json.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Dependencies {
    /// When the facts were read (RFC 3339): the floors depend on the date.
    pub checked: String,
    pub packages: Vec<Checked>,
}

/// The decision for one package (R3, R6, R7, R8, S2 step 5). Pure: the facts, the ledger entry
/// and the date come in. An entry counts only for its exact code (`Entry::is_for`), and only
/// for a crate that misses no more than the age and download floors: a yank or an advisory
/// comes after the review, so the AI reads the code again.
pub fn decide(p: &Package, facts: Option<&Facts>, entry: Option<&Entry>, now: time::OffsetDateTime) -> (Basis, Vec<String>) {
    let known = entry.is_some_and(|e| e.is_for(p));
    match p.kind {
        Kind::Creator => (Basis::Read, vec![]),
        Kind::Sdk => (Basis::Sdk, vec![]),
        Kind::Git if known => (Basis::Reviewed, vec!["a git dependency".into()]),
        Kind::Git => (Basis::Read, vec!["a git dependency".into()]),
        Kind::CratesIo => {
            let Some(f) = facts else { return (Basis::Refused, vec!["not on crates.io".into()]) };
            let bad: Vec<String> = f.advisories.iter().filter(|a| a.starts_with("MAL-")).map(|a| format!("malicious package advisory {a}")).collect();
            if !bad.is_empty() { return (Basis::Refused, bad); }
            let days = |t: &str| time::OffsetDateTime::parse(t, &time::format_description::well_known::Rfc3339).ok().map(|d| (now - d).whole_days());
            let mut why = vec![];
            if f.downloads < MIN_DOWNLOADS { why.push(format!("{} downloads (floor {MIN_DOWNLOADS})", f.downloads)); }
            match days(&f.published) {
                Some(d) if d >= MIN_CRATE_DAYS => {}
                Some(d) => why.push(format!("first published {d} days ago (floor {MIN_CRATE_DAYS})")),
                None => why.push("no publish date".into()),
            }
            match days(&f.version_published) {
                Some(d) if d >= MIN_VERSION_DAYS => {}
                Some(d) => why.push(format!("version published {d} days ago (floor {MIN_VERSION_DAYS})")),
                None => why.push("no version publish date".into()),
            }
            let fresh = f.yanked || !f.advisories.is_empty();
            if f.yanked { why.push("this version is yanked".into()); }
            if !f.advisories.is_empty() { why.push(format!("advisories {}", f.advisories.join(", "))); }
            let basis = match (why.is_empty(), known && !fresh) {
                (true, _) => Basis::Reputation,
                (false, true) => Basis::Reviewed,
                (false, false) => Basis::Read,
            };
            (basis, why)
        }
    }
}

/// The entry that a diff read starts from (R16, plan S2 step 6): for a crates.io crate that the
/// AI reads for its floors only (a yank or an advisory asks for a full read), the entry of the
/// nearest older version of the same crate, with a SHA-256 key. Pure, so review-brief checks it.
pub fn diff_base<'a>(p: &Package, facts: Option<&Facts>, basis: Basis, candidates: &'a [Entry]) -> Option<&'a Entry> {
    let f = facts?;
    if basis != Basis::Read || p.kind != Kind::CratesIo || f.yanked || !f.advisories.is_empty() { return None; }
    let new = semver::Version::parse(&p.version).ok()?;
    let sha = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
    candidates.iter().filter(|e| e.kind == Kind::CratesIo && e.name == p.name && sha(&e.source))
        .filter_map(|e| semver::Version::parse(&e.version).ok().filter(|v| *v < new).map(|v| (v, e)))
        .max_by(|a, b| a.0.cmp(&b.0)).map(|(_, e)| e)
}

/// Where `fm_pack deps` keeps the `.crate` of a diff base, in the build dir.
pub fn previous_crate(dir: &Path, e: &Entry) -> PathBuf {
    dir.join("previous").join(format!("{}-{}.crate", e.name, e.version))
}

/// The largest `.crate` of a diff base (crates.io allows 10 MB by default), and the largest tar
/// stream that it unpacks to (headers, long names and files: a gzip bomb stops there).
const MAX_CRATE: u64 = 32 << 20;
const MAX_UNPACKED: u64 = 64 << 20;

/// Keeps the `.crate` of diff base `e` when its bytes are the reviewed code (the SHA-256 of the
/// `.crate` is the Cargo.lock checksum of the entry key).
fn store_previous(dir: &Path, e: &Entry, bytes: &[u8]) -> Result<(), String> {
    if !crate::sha256_hex(bytes).eq_ignore_ascii_case(&e.source) { return Err("not the reviewed code (SHA-256)".into()); }
    let path = previous_crate(dir, e);
    std::fs::create_dir_all(path.parent().unwrap_or(dir)).map_err(|e| e.to_string())?;
    std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// The files of a diff base (R16) from the `.crate` that `store_previous` kept, checked again
/// against the entry and unpacked in memory: regular files under its `<name>-<version>/` folder,
/// by path relative to it. `fm_pack deps` runs it before it writes dependencies.json, so a crate
/// that does not unpack has no `previous`; review-run runs it again for the files.
pub fn previous_files(dir: &Path, e: &Entry) -> Result<std::collections::BTreeMap<String, Vec<u8>>, String> {
    let bytes = std::fs::read(previous_crate(dir, e)).map_err(|e| e.to_string())?;
    if !crate::sha256_hex(&bytes).eq_ignore_ascii_case(&e.source) { return Err("not the reviewed code (SHA-256)".into()); }
    unpack(&bytes, &format!("{}-{}", e.name, e.version), MAX_UNPACKED)
}

/// Unpacks a gzip tar into memory. `limit` counts the whole tar stream, so tar headers, GNU
/// long names and PAX data cannot grow past it before a file is counted (review r1 of step 6).
fn unpack(bytes: &[u8], top: &str, limit: u64) -> Result<std::collections::BTreeMap<String, Vec<u8>>, String> {
    use std::io::Read;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes).take(limit));
    let mut out = std::collections::BTreeMap::new();
    for f in archive.entries().map_err(|e| e.to_string())? {
        let mut f = f.map_err(|e| e.to_string())?;
        if !f.header().entry_type().is_file() { continue; }
        let path = f.path().map_err(|e| e.to_string())?.into_owned();
        let parts: Vec<String> = path.components().map(|c| match c { std::path::Component::Normal(s) => Ok(s.to_string_lossy().into_owned()), _ => Err(format!("{}: not a plain path", path.display())) }).collect::<Result<_, _>>()?;
        if parts.len() < 2 || parts[0] != top { return Err(format!("{}: not in {top}/", path.display())); }
        let mut data = vec![];
        f.read_to_end(&mut data).map_err(|e| e.to_string())?;
        out.insert(parts[1..].join("/"), data);
    }
    // A stream cut at the limit can look like a short archive: refuse it, never use part of it.
    if archive.into_inner().limit() == 0 { return Err(format!("larger than {limit} bytes unpacked")); }
    Ok(out)
}

/// Keeps the `.crate` of diff base `e` when it is the reviewed code and unpacks; else removes it.
fn keep_previous(dir: &Path, e: &Entry, bytes: &[u8]) -> Result<(), String> {
    let kept = store_previous(dir, e, bytes).and_then(|()| previous_files(dir, e).map(|_| ()));
    if kept.is_err() { let _ = std::fs::remove_file(previous_crate(dir, e)); }
    kept
}

/// Downloads the `.crate` of each diff base from crates.io. A base that fails is dropped: the AI
/// then reads that crate in full, as with no ledger, so a failure costs reading time only.
fn fetch_previous(dir: &Path, checked: &mut [Checked]) {
    let agent = http(120);
    for c in checked.iter_mut() {
        let Some(e) = c.previous.as_ref() else { continue };
        let url = format!("https://static.crates.io/crates/{0}/{0}-{1}.crate", e.name, e.version);
        let got = agent.get(&url).header("User-Agent", USER_AGENT).call()
            .and_then(|mut r| r.body_mut().with_config().limit(MAX_CRATE).read_to_vec()).map_err(|e| e.to_string())
            .and_then(|bytes| keep_previous(dir, e, &bytes));
        if let Err(why) = got {
            eprintln!("{} {}: no diff read from {} ({url}: {why}); the review reads it in full", c.package.name, c.package.version, e.version);
            c.previous = None;
        }
    }
}

/// fm_pack deps <build dir>: reads the packages of build.json, asks crates.io, OSV and the
/// ledger (`ledger`), and writes dependencies.json next to it. A refused package stops the
/// submission (exit 1), after the file is written, so the creator sees why.
pub fn command(dir: &str) -> Result<(), String> {
    let dir = Path::new(dir);
    let record = crate::build::facts_record(dir)?;
    let packages: Vec<Package> = serde_json::from_value(record["packages"].clone()).map_err(|e| format!("packages: {e} (build it again with this fm_pack)"))?;
    let prebuilt: Vec<Prebuilt> = serde_json::from_value(record["prebuilt"].clone()).map_err(|e| format!("prebuilt: {e} (build it again with this fm_pack)"))?;
    let now = time::OffsetDateTime::now_utc();
    // The ledger first: the OIDC token of the review job lives about 5 minutes, and the crates.io
    // facts take one second for each request (review r1 of S2).
    let l = ledger(std::env::var("FM_API").ok().filter(|a| !a.is_empty()), std::env::var("FM_API_TOKEN").ok(), &packages)?;
    let facts = fetch_facts(&packages)?;
    let mut checked: Vec<Checked> = packages.into_iter().zip(facts).map(|(package, facts)| check_one(package, facts, &l, now)).collect();
    fetch_previous(dir, &mut checked);
    let checked_at = now.format(&time::format_description::well_known::Rfc3339).map_err(|e| e.to_string())?;
    let deps = Dependencies { checked: checked_at, packages: checked };
    std::fs::write(dir.join("dependencies.json"), serde_json::to_vec_pretty(&deps).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let count = |b: Basis| deps.packages.iter().filter(|c| c.basis == b).count();
    println!("{}", serde_json::json!({ "read": count(Basis::Read), "reviewed": count(Basis::Reviewed), "reputation": count(Basis::Reputation), "sdk": count(Basis::Sdk), "refused": count(Basis::Refused) }));
    let mut refused: Vec<String> = deps.packages.iter().filter(|c| c.basis == Basis::Refused).map(|c| format!("{} {}: {}", c.package.name, c.package.version, c.why.join("; "))).collect();
    refused.extend(prebuilt_check(&deps.packages, &prebuilt).err());
    match refused.is_empty() {
        true => Ok(()),
        false => Err(format!("refused dependencies: {}", refused.join(" | "))),
    }
}

/// The ledger for the keys of `packages` (`Ledger`), from fm-api (`api` is FM_API, `token` the
/// review job's OIDC token in FM_API_TOKEN). With no FM_API (a creator's own CI) there is no
/// ledger and every package keeps the basis of its facts. A failed read stops the gate.
fn ledger(api: Option<String>, token: Option<String>, packages: &[Package]) -> Result<Ledger, String> {
    let Some(api) = api else { return Ok(Ledger::default()) };
    let token = token.filter(|t| !t.is_empty()).ok_or("FM_API is set, FM_API_TOKEN is not")?;
    let keys: Vec<_> = packages.iter().filter_map(ledger_key).map(|(kind, name, version, source)| serde_json::json!({ "kind": kind, "name": name, "version": version, "source": source })).collect();
    if keys.is_empty() { return Ok(Ledger::default()); }
    let url = format!("{}/v1/review/ledger", api.trim_end_matches('/'));
    http(60).post(&url).header("Authorization", &format!("Bearer {token}")).header("User-Agent", USER_AGENT)
        .send_json(serde_json::json!({ "keys": keys })).and_then(|mut r| r.body_mut().read_json()).map_err(|e| format!("the dependency ledger ({url}): {e}"))
}

/// The facts of each crates.io package, in the order of `packages` (None for other kinds and for
/// a crate that crates.io does not have). Any other failure stops the gate: no facts, no decision.
fn fetch_facts(packages: &[Package]) -> Result<Vec<Option<Facts>>, String> {
    let crates: Vec<&Package> = packages.iter().filter(|p| p.kind == Kind::CratesIo).collect();
    let advisories = osv(&crates)?;
    let agent = http(60);
    let mut out = Vec::new();
    let mut first = true;
    for p in packages {
        if p.kind != Kind::CratesIo { out.push(None); continue; }
        let mut get = |path: String| -> Result<Option<serde_json::Value>, String> {
            if !first { std::thread::sleep(std::time::Duration::from_secs(1)); }
            first = false;
            let url = format!("https://crates.io/api/v1/crates/{path}");
            match agent.get(&url).header("User-Agent", USER_AGENT).call() {
                Ok(mut r) => r.body_mut().read_json().map(Some).map_err(|e| format!("{url}: {e}")),
                Err(ureq::Error::StatusCode(404)) => Ok(None),
                Err(e) => Err(format!("{url}: {e}")),
            }
        };
        let (Some(c), Some(v)) = (get(format!("{}?include=", p.name))?, get(format!("{}/{}", p.name, p.version))?) else { out.push(None); continue };
        let text = |j: &serde_json::Value| j.as_str().unwrap_or_default().to_string();
        out.push(Some(Facts {
            downloads: c["crate"]["downloads"].as_u64().unwrap_or(0),
            published: text(&c["crate"]["created_at"]),
            version_published: text(&v["version"]["created_at"]),
            yanked: v["version"]["yanked"].as_bool().unwrap_or(true),
            advisories: advisories.get(&(p.name.clone(), p.version.clone())).cloned().unwrap_or_default(),
        }));
    }
    Ok(out)
}

/// OSV advisory ids for each crate version, in one batch query. A result with a next page has
/// more advisories than one page; the gate stops rather than decide on part of the list.
fn osv(crates: &[&Package]) -> Result<std::collections::BTreeMap<(String, String), Vec<String>>, String> {
    let mut out = std::collections::BTreeMap::new();
    let agent = http(60);
    for chunk in crates.chunks(1000) {
        let queries: Vec<_> = chunk.iter().map(|p| serde_json::json!({ "package": { "name": p.name, "ecosystem": "crates.io" }, "version": p.version })).collect();
        let answer: serde_json::Value = agent.post("https://api.osv.dev/v1/querybatch").header("User-Agent", USER_AGENT)
            .send_json(serde_json::json!({ "queries": queries })).and_then(|mut r| r.body_mut().read_json()).map_err(|e| format!("OSV: {e}"))?;
        let results = answer["results"].as_array().filter(|r| r.len() == chunk.len()).ok_or("OSV: one result for each query expected")?;
        for (p, r) in chunk.iter().zip(results) {
            if r.get("next_page_token").is_some() { return Err(format!("OSV: {} {} has more advisories than one page; check it by hand", p.name, p.version)); }
            let ids = r["vulns"].as_array().into_iter().flatten().filter_map(|v| v["id"].as_str().map(String::from)).collect();
            out.insert((p.name.clone(), p.version.clone()), ids);
        }
    }
    Ok(out)
}

/// A `.crate` as crates.io makes it, for the tests: a gzip tar with each file under `top/`.
#[cfg(test)]
pub(crate) fn dot_crate(top: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut t = tar::Builder::new(flate2::write::GzEncoder::new(vec![], flate2::Compression::default()));
    for (rel, bytes) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(bytes.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        t.append_data(&mut h, format!("{top}/{rel}"), *bytes).unwrap();
    }
    t.into_inner().unwrap().finish().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixed date of the floor tests.
    fn now() -> time::OffsetDateTime {
        time::OffsetDateTime::parse("2026-10-29T12:00:00Z", &time::format_description::well_known::Rfc3339).unwrap()
    }

    fn krate(name: &str) -> Package { Package { name: name.into(), version: "1.0.0".into(), kind: Kind::CratesIo, source: None, checksum: None } }
    fn facts(downloads: u64, published: &str, version: &str) -> Facts {
        Facts { downloads, published: published.into(), version_published: version.into(), yanked: false, advisories: vec![] }
    }

    // S2 step 5: a ledger entry makes a crate below a floor `reviewed`, only for its exact code
    // (name, version, checksum; git url#commit), never for a yank or an advisory, and it does not
    // change a crate that passes the floors or creator code.
    #[test]
    fn the_ledger_counts_only_for_the_exact_code() {
        let sum = "a".repeat(64);
        let p = Package { checksum: Some(sum.clone()), ..krate("small") };
        let entry = Entry { kind: Kind::CratesIo, name: "small".into(), version: "1.0.0".into(), source: sum.clone(), powers: vec![], reaches: vec![], run: "940".into() };
        let low = facts(10, "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z");
        let floor = vec!["10 downloads (floor 10000)".to_string()];
        assert_eq!(decide(&p, Some(&low), Some(&entry), now()), (Basis::Reviewed, floor.clone()));
        assert_eq!(decide(&p, Some(&low), None, now()), (Basis::Read, floor.clone()));
        for other in [Entry { source: "b".repeat(64), ..entry.clone() }, Entry { version: "1.0.1".into(), ..entry.clone() }, Entry { name: "smal".into(), ..entry.clone() }, Entry { kind: Kind::Git, ..entry.clone() }] {
            assert_eq!(decide(&p, Some(&low), Some(&other), now()).0, Basis::Read, "{other:?}");
        }
        assert_eq!(decide(&Package { checksum: None, ..p.clone() }, Some(&low), Some(&entry), now()).0, Basis::Read);
        assert_eq!(decide(&p, Some(&Facts { yanked: true, ..low.clone() }), Some(&entry), now()).0, Basis::Read);
        assert_eq!(decide(&p, Some(&Facts { advisories: vec!["RUSTSEC-2026-0001".into()], ..low.clone() }), Some(&entry), now()).0, Basis::Read);
        assert_eq!(decide(&p, Some(&Facts { advisories: vec!["MAL-2026-1".into()], ..low.clone() }), Some(&entry), now()).0, Basis::Refused);
        assert_eq!(decide(&p, Some(&facts(10_000, "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z")), Some(&entry), now()).0, Basis::Reputation);
        let git = Package { kind: Kind::Git, source: Some("https://github.com/o/r#0123".into()), ..krate("g") };
        let git_entry = Entry { kind: Kind::Git, name: "g".into(), source: "https://github.com/o/r#0123".into(), ..entry.clone() };
        assert_eq!(decide(&git, None, Some(&git_entry), now()).0, Basis::Reviewed);
        assert_eq!(decide(&git, None, Some(&Entry { source: "https://github.com/o/r#4567".into(), ..git_entry.clone() }), now()).0, Basis::Read);
        let mine = Package { kind: Kind::Creator, source: Some(".".into()), ..krate("mine") };
        assert_eq!(ledger_key(&mine), None);
        assert_eq!(decide(&mine, None, Some(&Entry { kind: Kind::Git, name: "mine".into(), source: ".".into(), ..entry }), now()).0, Basis::Read);
    }

    // The ledger call: the keys of crates.io (checksum) and git packages only, the bearer token,
    // the entries of the answer; an error answer stops the gate; no FM_API, no call.
    #[test]
    fn the_ledger_read_sends_the_keys_and_the_token() {
        let sum = "a".repeat(64);
        let packages = [Package { checksum: Some(sum.clone()), ..krate("small") }, Package { kind: Kind::Creator, source: Some(".".into()), ..krate("mine") },
            Package { kind: Kind::Git, source: Some("https://github.com/o/r#0123".into()), ..krate("g") }];
        let entry = serde_json::json!({ "kind": "crates_io", "name": "small", "version": "1.0.0", "source": sum, "powers": ["direct_network"], "reaches": [], "run": "940" });
        let old = serde_json::json!({ "kind": "crates_io", "name": "small", "version": "0.9.0", "source": "c".repeat(64), "powers": [], "reaches": [], "run": "930" });
        let (url, h) = crate::test_http::serve(vec![(200, serde_json::json!({ "entries": [entry], "previous": [old.clone()], "marked": [old] })), (401, serde_json::json!({ "error": "not_the_review_job" }))]);
        let got = ledger(Some(url.clone()), Some("tok-1".into()), &packages).unwrap();
        assert_eq!(got.entries.len(), 1);
        assert!(got.entries[0].is_for(&packages[0]) && got.entries[0].powers == ["direct_network"]);
        assert_eq!((got.previous.len(), got.previous[0].version.as_str(), got.marked.len()), (1, "0.9.0", 1));
        assert!(ledger(Some(url), Some("tok-1".into()), &packages).unwrap_err().contains("dependency ledger"));
        let seen = h.join().unwrap();
        assert!(seen[0].0.starts_with("POST /v1/review/ledger "), "{}", seen[0].0);
        assert!(seen[0].0.to_ascii_lowercase().contains("authorization: bearer tok-1\r\n"), "{}", seen[0].0);
        assert_eq!(seen[0].1, serde_json::json!({ "keys": [{ "kind": "crates_io", "name": "small", "version": "1.0.0", "source": sum },
            { "kind": "git", "name": "g", "version": "1.0.0", "source": "https://github.com/o/r#0123" }] }));
        assert_eq!(ledger(None, None, &packages).unwrap(), Ledger::default());
        assert!(ledger(Some("http://127.0.0.1:9".into()), None, &packages).unwrap_err().contains("FM_API_TOKEN"));
    }

    // S2 step 6: a crate that the AI reads for its floors gets the nearest older version of the
    // same crate as its diff base; never a newer or equal version, another crate, git code, a
    // key that is not a SHA-256, or a crate that a yank or an advisory sends to a full read.
    #[test]
    fn the_diff_base_is_the_nearest_older_reviewed_version() {
        let p = Package { version: "1.2.0".into(), ..krate("small") };
        let e = |name: &str, version: &str| Entry { kind: Kind::CratesIo, name: name.into(), version: version.into(), source: "a".repeat(64), powers: vec![], reaches: vec![], run: "9".into() };
        let all = [e("small", "0.9.0"), e("small", "1.1.3"), e("small", "1.10.0"), e("small", "1.2.0"), e("other", "1.1.9"), Entry { kind: Kind::Git, ..e("small", "1.1.8") },
            Entry { source: "zz".into(), ..e("small", "1.1.9") }];
        let low = facts(10, "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z");
        assert_eq!(diff_base(&p, Some(&low), Basis::Read, &all).map(|e| e.version.as_str()), Some("1.1.3"));
        assert_eq!(diff_base(&p, Some(&low), Basis::Read, &all[2..4]), None);
        assert_eq!(diff_base(&p, Some(&low), Basis::Reviewed, &all), None);
        assert_eq!(diff_base(&p, Some(&Facts { yanked: true, ..low.clone() }), Basis::Read, &all), None);
        assert_eq!(diff_base(&p, Some(&Facts { advisories: vec!["RUSTSEC-2026-0001".into()], ..low.clone() }), Basis::Read, &all), None);
        assert_eq!(diff_base(&Package { kind: Kind::Git, ..p.clone() }, Some(&low), Basis::Read, &all), None);
        assert_eq!(diff_base(&p, None, Basis::Read, &all), None);
        // The `.crate` is kept only when its SHA-256 is the key of the entry.
        let t = tempfile::tempdir().unwrap();
        let bytes = b"crate bytes";
        let good = Entry { source: crate::sha256_hex(bytes), ..e("small", "1.1.3") };
        store_previous(t.path(), &good, bytes).unwrap();
        assert_eq!(std::fs::read(previous_crate(t.path(), &good)).unwrap(), bytes);
        assert!(store_previous(t.path(), &e("small", "1.1.4"), bytes).unwrap_err().contains("SHA-256"));
        assert!(!previous_crate(t.path(), &e("small", "1.1.4")).exists());
        // Review r1: a base is kept only when it also unpacks; else its file goes.
        assert!(keep_previous(t.path(), &good, bytes).is_err() && !previous_crate(t.path(), &good).exists());
        let real = dot_crate("small-1.1.3", &[("src/lib.rs", b"pub fn f() {}\n")]);
        let real_e = Entry { source: crate::sha256_hex(&real), ..good.clone() };
        keep_previous(t.path(), &real_e, &real).unwrap();
        assert_eq!(previous_files(t.path(), &real_e).unwrap()["src/lib.rs"], b"pub fn f() {}\n");
    }

    // Review r2 of the S2 cloud part: code that Feature Machine marked for Re-review (R17) gets a
    // full read, also when an older version could be its diff base.
    #[test]
    fn marked_code_gets_a_full_read() {
        let sum = "a".repeat(64);
        let p = Package { version: "1.2.0".into(), checksum: Some(sum.clone()), ..krate("small") };
        let older = Entry { kind: Kind::CratesIo, name: "small".into(), version: "1.1.0".into(), source: "b".repeat(64), powers: vec![], reaches: vec![], run: "9".into() };
        let low = facts(10, "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z");
        let mine = Entry { version: "1.2.0".into(), source: sum, ..older.clone() };
        let free = check_one(p.clone(), Some(low.clone()), &Ledger { previous: vec![older.clone()], ..Ledger::default() }, now());
        assert_eq!((free.basis, free.previous.as_ref().map(|e| e.version.as_str()), free.re_review), (Basis::Read, Some("1.1.0"), false));
        let marked = check_one(p.clone(), Some(low.clone()), &Ledger { previous: vec![older.clone()], marked: vec![mine.clone()], ..Ledger::default() }, now());
        assert_eq!((marked.basis, marked.previous, marked.re_review), (Basis::Read, None, true));
        // Review r4: an answer with the same key active and marked still gives a full read.
        let mixed = check_one(p, Some(low), &Ledger { entries: vec![mine.clone()], previous: vec![older], marked: vec![mine] }, now());
        assert_eq!((mixed.basis, mixed.ledger, mixed.previous, mixed.re_review), (Basis::Read, None, None, true));
    }

    // Review r1 of step 6: the limit counts the tar stream, so a long name (GNU long-name data)
    // or many small files stop at it, and a stream cut at the limit is refused, not used in part.
    #[test]
    fn the_unpack_limit_counts_the_whole_stream() {
        let long = format!("src/{}.rs", "n".repeat(3000));
        let named = dot_crate("x-1.0.0", &[(long.as_str(), b"x")]);
        assert!(unpack(&named, "x-1.0.0", 2048).is_err());
        assert_eq!(unpack(&named, "x-1.0.0", 1 << 20).unwrap().len(), 1);
        let many: Vec<(String, Vec<u8>)> = (0..20).map(|i| (format!("f{i}"), vec![b'a'; 100])).collect();
        let many = dot_crate("x-1.0.0", &many.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect::<Vec<_>>());
        assert!(unpack(&many, "x-1.0.0", 8 * 1024).is_err());
        // A cut at a header boundary reads as a short archive: the limit check refuses it.
        assert!(unpack(&many, "x-1.0.0", 4 * 1024).unwrap_err().contains("larger than"));
        assert!(unpack(b"not gzip", "x-1.0.0", 1 << 20).is_err());
    }

    // R7 floors: each one sends the crate to the AI (R6) on its own; all passed is reputation.
    #[test]
    fn floors_decide_reputation_or_read() {
        let ok = facts(10_000, "2026-09-29T12:00:00Z", "2026-10-22T11:59:59.5+00:00");
        assert_eq!(decide(&krate("a"), Some(&ok), None, now()), (Basis::Reputation, vec![]));
        let cases: [(&str, Facts); 6] = [
            ("9999 downloads (floor 10000)", Facts { downloads: 9_999, ..ok.clone() }),
            ("first published 29 days ago (floor 30)", Facts { published: "2026-09-30T00:00:00Z".into(), ..ok.clone() }),
            ("version published 6 days ago (floor 7)", Facts { version_published: "2026-10-23T00:00:00Z".into(), ..ok.clone() }),
            ("no version publish date", Facts { version_published: String::new(), ..ok.clone() }),
            ("this version is yanked", Facts { yanked: true, ..ok.clone() }),
            ("advisories RUSTSEC-2020-0071", Facts { advisories: vec!["RUSTSEC-2020-0071".into()], ..ok.clone() }),
        ];
        for (want, f) in cases {
            assert_eq!(decide(&krate("a"), Some(&f), None, now()), (Basis::Read, vec![want.to_string()]), "{want}");
        }
    }

    // A crate that crates.io does not have (removed) or one with a malicious-package advisory
    // stops the submission; creator code and git code are read; the SDK is the checked tag.
    #[test]
    fn refusals_and_other_kinds() {
        assert_eq!(decide(&krate("gone"), None, None, now()).0, Basis::Refused);
        let bad = Facts { advisories: vec!["GHSA-x".into(), "MAL-2025-1".into()], ..facts(1 << 40, "2015-01-01T00:00:00Z", "2015-01-01T00:00:00Z") };
        assert_eq!(decide(&krate("evil"), Some(&bad), None, now()), (Basis::Refused, vec!["malicious package advisory MAL-2025-1".to_string()]));
        let kind = |kind| decide(&Package { kind, ..krate("x") }, None, None, now()).0;
        assert_eq!([kind(Kind::Creator), kind(Kind::Git), kind(Kind::Sdk)], [Basis::Read, Basis::Read, Basis::Sdk]);
    }

    /// `cargo metadata` packages for the fixture tree.
    fn meta(src: &str) -> serde_json::Value {
        serde_json::json!({ "packages": [
            { "name": "creator_feature", "version": "0.1.0", "source": null, "manifest_path": format!("{src}/features/x/Cargo.toml") },
            { "name": "helper", "version": "0.1.0", "source": null, "manifest_path": format!("{src}/Cargo.toml") },
            { "name": "3d-helper", "version": "0.1.0", "source": null, "manifest_path": format!("{src}/crates/3d/Cargo.toml") },
            { "name": "odd", "version": "0.1.0", "source": null, "manifest_path": format!("{src}/crates/odd (proc-macro)/Cargo.toml") },
            { "name": "odd_mac", "version": "0.1.0", "source": null, "manifest_path": format!("{src}/crates/m (proc-macro) (*)/Cargo.toml") },
            { "name": "d-helper", "version": "0.1.0", "source": null, "manifest_path": format!("{src}/crates/d/Cargo.toml") },
            { "name": "fm_feature_sdk", "version": "0.1.0", "source": null, "manifest_path": format!("{src}/crates/fm_feature_sdk/Cargo.toml") },
            { "name": "serde", "version": "1.0.229", "source": CRATES_IO, "manifest_path": "/cargo/registry/serde/Cargo.toml" },
            { "name": "serde_derive", "version": "1.0.229", "source": CRATES_IO, "manifest_path": "/cargo/registry/serde_derive/Cargo.toml" },
            { "name": "gitdep", "version": "0.2.0", "source": "git+https://github.com/someone/gitdep?tag=v0.2.0#0123456789abcdef0123456789abcdef01234567", "manifest_path": "/cargo/git/gitdep/Cargo.toml" },
            { "name": "fm_feature_sdk", "version": "0.2.0", "source": "git+https://github.com/Feature-Machine/feature-machine-sdk?tag=v0.2.0#89abcdef0123456789abcdef0123456789abcdef", "manifest_path": "/cargo/git/sdk/Cargo.toml" },
        ] })
    }

    // The real `cargo tree -f {p}` forms (2026-10-29): a path in parentheses, "(proc-macro)" and
    // "(*)" marks, a git source with its short commit; one entry for each package.
    #[test]
    fn packages_come_from_the_tree_with_full_sources() {
        let src = std::env::temp_dir().join("fm-deps-fixture");
        let s = src.to_string_lossy().replace('\\', "/");
        // `--prefix none`: a name that starts with a digit stays whole (review r5).
        let tree = format!("creator_feature v0.1.0 ({s}/features/x)\nfm_feature_sdk v0.1.0 ({s}/crates/fm_feature_sdk)\nserde v1.0.229\nserde_derive v1.0.229 (proc-macro)\nhelper v0.1.0 ({s})\nserde v1.0.229 (*)\ngitdep v0.2.0 (proc-macro) (https://github.com/someone/gitdep?tag=v0.2.0#01234567)\ngitdep v0.2.0 (proc-macro) (https://github.com/someone/gitdep?tag=v0.2.0#01234567) (*)\n3d-helper v0.1.0 ({s}/crates/3d)\nd-helper v0.1.0 ({s}/crates/d)\nodd v0.1.0 ({s}/crates/odd (proc-macro))\nodd_mac v0.1.0 (proc-macro) ({s}/crates/m (proc-macro) (*))\n");
        let got: Vec<Package> = packages(&tree, &meta(&s), Path::new(&s), &serde_json::Value::Null).unwrap().into_iter().map(|r| r.package).collect();
        let want = [
            Package { name: "3d-helper".into(), version: "0.1.0".into(), kind: Kind::Creator, source: Some("crates/3d".into()), checksum: None },
            Package { name: "creator_feature".into(), version: "0.1.0".into(), kind: Kind::Creator, source: Some("features/x".into()), checksum: None },
            Package { name: "d-helper".into(), version: "0.1.0".into(), kind: Kind::Creator, source: Some("crates/d".into()), checksum: None },
            Package { name: "fm_feature_sdk".into(), version: "0.1.0".into(), kind: Kind::Sdk, source: None, checksum: None },
            Package { name: "gitdep".into(), version: "0.2.0".into(), kind: Kind::Git, source: Some("https://github.com/someone/gitdep#0123456789abcdef0123456789abcdef01234567".into()), checksum: None },
            Package { name: "helper".into(), version: "0.1.0".into(), kind: Kind::Creator, source: Some(".".into()), checksum: None },
            // Review r7: a path that holds "(proc-macro)" or "(*)" stays whole.
            Package { name: "odd".into(), version: "0.1.0".into(), kind: Kind::Creator, source: Some("crates/odd (proc-macro)".into()), checksum: None },
            Package { name: "odd_mac".into(), version: "0.1.0".into(), kind: Kind::Creator, source: Some("crates/m (proc-macro) (*)".into()), checksum: None },
            Package { name: "serde".into(), version: "1.0.229".into(), kind: Kind::CratesIo, source: None, checksum: None },
            Package { name: "serde_derive".into(), version: "1.0.229".into(), kind: Kind::CratesIo, source: None, checksum: None },
        ];
        assert_eq!(got, want);
        // The git SDK at the checked commit is the SDK, not a git dependency to read.
        let sdk = serde_json::json!({ "repository": "https://github.com/Feature-Machine/feature-machine-sdk", "tag": "v0.2.0", "commit": "89abcdef0123456789abcdef0123456789abcdef" });
        let tree = "creator_feature v0.1.0 (".to_string() + &s + "/features/x)\nfm_feature_sdk v0.2.0 (https://github.com/Feature-Machine/feature-machine-sdk?tag=v0.2.0#89abcdef)\n";
        let got = packages(&tree, &meta(&s), Path::new(&s), &sdk).unwrap();
        assert_eq!(got[1].dir, Path::new("/cargo/git/sdk"));
        assert_eq!(got[1].package, Package { name: "fm_feature_sdk".into(), version: "0.2.0".into(), kind: Kind::Sdk, source: None, checksum: None });
    }

    // Sources that nobody can observe, or that a runner with no credentials should not fetch.
    #[test]
    fn unobservable_sources_are_refused() {
        let src = Path::new("/r");
        let one = |p: serde_json::Value| classify("x", "1.0.0", &p, src, &serde_json::Value::Null).unwrap_err();
        assert!(one(serde_json::json!({ "source": "registry+https://my.registry/index" })).contains("comes from registry+https://my.registry/index"));
        assert!(one(serde_json::json!({ "source": "sparse+https://my.registry/index/" })).contains("comes from sparse+"));
        assert!(one(serde_json::json!({ "source": "git+ssh://git@github.com/a/b#0123" })).contains("must use https"));
        // Review r2 P1: credentials in a git URL fetch private code; no message shows them.
        for s in ["git+https://user:secret@github.com/a/b?tag=v1#0123", "git+https://secret@github.com/a/b#0123", "git+http://user:secret@github.com/a/b#0123", "registry+https://user:secret@my.registry/index"] {
            let err = one(serde_json::json!({ "source": s }));
            assert!(!err.contains("secret") && err.contains("***@"), "{s}: {err}");
        }
        assert!(one(serde_json::json!({ "source": "git+https://user:secret@github.com/a/b#0123" })).contains("must not hold credentials"));
        assert_eq!(redact("https://github.com/a/b@v1"), "https://github.com/a/b@v1", "an @ in the path is not user information");
        assert!(one(serde_json::json!({ "source": null, "manifest_path": "/elsewhere/x/Cargo.toml" })).contains("outside the repository"));
        // An SDK crate name outside a local build is creator code, not the SDK.
        let sdk = serde_json::json!({ "commit": "0" });
        let p = classify("fm_feature_sdk", "0.1.0", &serde_json::json!({ "source": null, "manifest_path": "/r/sdk/Cargo.toml" }), src, &sdk).unwrap();
        assert_eq!((p.kind, p.source.as_deref()), (Kind::Creator, Some("sdk")));
        // From the SDK repository only the commit of the verified SDK (review r3).
        let sdk = serde_json::json!({ "commit": "aaaa" });
        let git = |c: &str| serde_json::json!({ "source": format!("git+https://github.com/Feature-Machine/feature-machine-sdk?tag=v1#{c}") });
        assert_eq!(classify("fm_paths", "0.1.0", &git("aaaa"), src, &sdk).unwrap().kind, Kind::Sdk);
        assert!(classify("fm_paths", "0.1.0", &git("bbbb"), src, &sdk).unwrap_err().contains("only the commit of its fm_feature_sdk release"));
    }

    // S2 step 1: each crates.io package gets its Cargo.lock checksum; none is an error.
    #[test]
    fn crates_io_packages_get_the_lock_checksum() {
        let t = tempfile::tempdir().unwrap();
        let lock = t.path().join("Cargo.lock");
        let sum = "AB".repeat(32);
        std::fs::write(&lock, format!("version = 4\n\n[[package]]\nname = \"a\"\nversion = \"1.0.0\"\nsource = \"{CRATES_IO}\"\nchecksum = \"{sum}\"\n\n[[package]]\nname = \"probe\"\nversion = \"0.1.0\"\n")).unwrap();
        let creator = Package { name: "probe".into(), version: "0.1.0".into(), kind: Kind::Creator, source: Some(".".into()), checksum: None };
        let got = with_checksums(vec![krate("a"), creator.clone()], &lock).unwrap();
        assert_eq!(got[0].checksum.as_deref(), Some("ab".repeat(32).as_str()));
        assert_eq!(got[1], creator);
        assert!(with_checksums(vec![krate("b")], &lock).unwrap_err().contains("no checksum for b 1.0.0"));
        let json = serde_json::to_value(&got[0]).unwrap();
        assert_eq!(json["checksum"], "ab".repeat(32));
        let old: Package = serde_json::from_value(serde_json::json!({ "name": "a", "version": "1.0.0", "kind": "crates_io" })).unwrap();
        assert_eq!(old.checksum, None, "a build.json of an older fm_pack still reads");
    }

    // Every git source of Cargo.lock, also one that the build does not use (review r5): the SDK
    // repository only at the SDK commit; others must come to a git with no credential.
    #[test]
    fn every_locked_git_source_is_checked() {
        let t = tempfile::tempdir().unwrap();
        let lock = t.path().join("Cargo.lock");
        let sdk = serde_json::json!({ "commit": "aaaa" });
        let entry = |name: &str, source: &str| format!("[[package]]\nname = \"{name}\"\nversion = \"0.1.0\"\nsource = \"{source}\"\n\n");
        let base = entry("fm_feature_sdk", "git+https://github.com/Feature-Machine/feature-machine-sdk?tag=v1#aaaa") + &entry("itoa", "registry+https://github.com/rust-lang/crates.io-index");
        std::fs::write(&lock, format!("version = 4\n\n{base}")).unwrap();
        locked_git(&lock, &sdk, &t.path().join("g")).unwrap();
        std::fs::write(&lock, format!("version = 4\n\n{base}{}", entry("fm_paths", "git+https://github.com/Feature-Machine/feature-machine-sdk?branch=main#bbbb"))).unwrap();
        assert!(locked_git(&lock, &sdk, &t.path().join("g")).unwrap_err().contains("only the commit of its fm_feature_sdk release"));
        std::fs::write(&lock, format!("version = 4\n\n{base}{}", entry("x", "git+https://127.0.0.1:9/closed/x#0123"))).unwrap();
        assert!(locked_git(&lock, &sdk, &t.path().join("g")).unwrap_err().contains("cannot fetch"));
    }

    // The anonymous fetch counts submodule links in the commit (a local repository here).
    #[test]
    fn a_fetched_commit_shows_its_submodules() {
        let t = tempfile::tempdir().unwrap();
        let repo = t.path().join("r");
        let git = |args: &[&str]| assert!(anonymous_git().arg("-C").arg(&repo).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap().status.success(), "{args:?}");
        std::fs::create_dir_all(&repo).unwrap();
        git(&["init", "--quiet"]);
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "--quiet", "-m", "a"]);
        let head = |r: &Path| String::from_utf8(anonymous_git().arg("-C").arg(r).args(["rev-parse", "HEAD"]).output().unwrap().stdout).unwrap().trim().to_string();
        let plain = head(&repo);
        git(&["update-index", "--add", "--cacheinfo", &format!("160000,{plain},sub")]);
        git(&["commit", "--quiet", "-m", "sub"]);
        let linked = head(&repo);
        let url = repo.to_string_lossy().into_owned();
        assert_eq!(anonymous_fetch(&url, &plain, &t.path().join("f1")), Some(0));
        assert_eq!(anonymous_fetch(&url, &linked, &t.path().join("f2")), Some(1));
        assert_eq!(anonymous_fetch(&url, &"0".repeat(40), &t.path().join("f3")), None);
    }

    // Machine code anywhere in the commit stops the build, also outside the package folders (a
    // build.rs can link ../../native/x.lib) and as a bare COFF object (review r1 P1); text,
    // images and data pass.
    #[test]
    fn committed_machine_code_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("features/x");
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::create_dir_all(t.path().join("native")).unwrap();
        std::fs::write(dir.join("lib.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.join("assets/icon.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        std::fs::write(dir.join("empty.txt"), b"").unwrap();
        committed_binaries(t.path()).unwrap();
        let objects = [("features/x/assets/helper.dll", b"MZ\x90\0".to_vec()), ("native/libx.a", b"!<arch>\n".to_vec()),
            ("native/m.wasm", b"\0asm\x01".to_vec()), ("native/helper.obj", coff_object()),
            // LLVM bitcode (review r3 P1): raw, as `rustc --emit=llvm-bc` writes it, and wrapped.
            ("native/m.bc", b"BC\xc0\xde\x35\x14\0\0".to_vec()), ("native/w.bc", b"\xde\xc0\x17\x0b\0\0\0\0".to_vec()),
            // A clang precompiled header (review r4).
            ("native/h.pch", b"CPCH\x1f\0\0\0".to_vec())];
        for (name, bytes) in objects {
            std::fs::write(t.path().join(name), bytes).unwrap();
            let err = committed_binaries(t.path()).unwrap_err();
            assert!(err.contains(name) && err.contains(BINARY_LOCK), "{name}: {err}");
            std::fs::remove_file(t.path().join(name)).unwrap();
        }
    }

    /// An x64 COFF object with one empty .text section, as cl.exe /c writes the header: machine
    /// 0x8664, 1 section, no symbols, then the 40-byte section header.
    fn coff_object() -> Vec<u8> {
        let mut o = vec![0x64, 0x86, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        o.extend(b".text\0\0\0");
        o.extend([0u8; 28]);
        o.extend(0x6000_0020u32.to_le_bytes());
        // The parser must accept it as an object, so the test object is a real one.
        assert!(object::File::parse(&o[..]).is_ok());
        o
    }

    // binaries.json (R5): every entry is checked, and only approved bytes pass (R4). The shipped
    // approval list is empty, so each closed binary needs its first-use approval.
    #[test]
    fn closed_binaries_need_an_approval() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(binary_lock(t.path()).unwrap(), vec![]);
        let sha = "ab".repeat(32);
        let entry = |file: &str, url: &str| serde_json::json!({ "binaries": [{ "file": file, "version": "3.7.10", "url": url, "sha256": sha, "publisher": "NVIDIA Corporation" }] }).to_string();
        let write = |text: String| std::fs::write(t.path().join(BINARY_LOCK), text).unwrap();
        write(entry("nvngx.dll", "https://github.com/NVIDIA/x/releases/download/v1/nvngx.dll"));
        assert!(binary_lock(t.path()).unwrap_err().contains("has not approved"));
        let approved = serde_json::json!({ "binaries": [{ "sha256": sha.to_uppercase(), "publisher": "NVIDIA Corporation", "approved": "2026-10-29", "note": "test" }] }).to_string();
        assert_eq!(binary_lock_with(t.path(), &approved).unwrap().len(), 1);
        for (text, want) in [
            (entry("../x.dll", "https://h/x"), "plain file name"),
            (entry("x.dll", "http://h/x"), "https"),
            (entry("x.dll", "https://h/x").replace(&sha, "zz"), "SHA-256"),
            (entry("x.dll", "https://h/x").replace("NVIDIA Corporation", " "), "publisher"),
            (entry("x.dll", "https://h/x").replace("\"file\"", "\"run\":1,\"file\""), "unknown field"),
            (entry("x.dll", "https://h/x").replace("3.7.10", " "), "no version"),
            (entry("x.dll", "https://h/x").replace(",\"version\":\"3.7.10\"", ""), "missing field `version`"),
            (entry("x.dll", "https://u:p@h/x"), "no credentials"),
        ] {
            write(text);
            let err = binary_lock_with(t.path(), &approved).unwrap_err();
            assert!(err.contains(want), "{want}: {err}");
        }
        let b = Binary { file: "x.dll".into(), version: "1".into(), url: "https://h/x".into(), sha256: crate::sha256_hex(b"right"), publisher: "P".into() };
        check_bytes(&b, b"right").unwrap();
        assert!(check_bytes(&b, b"wrong").unwrap_err().contains("not the approved file"));
        approvals(APPROVED).unwrap();
    }

    // Review r2 P1: dependency folders are outside the scan of the commit, but their build
    // scripts can link machine code. In a git checkout (code that the AI reads, R8) it stops the
    // build, also outside the package folder; each file of a crates.io package goes to
    // build.json "prebuilt" (R19 decides on it, below).
    #[test]
    fn dependency_machine_code_is_recorded_or_stops_the_build() {
        let t = tempfile::tempdir().unwrap();
        let checkout = t.path().join("git/checkouts/helper-0123abcd/0123456");
        let crate_dir = t.path().join("registry/src/index.crates.io-x/winlib-1.0.0");
        for d in [checkout.join("crates/helper/src"), checkout.join("native"), crate_dir.join("lib")] { std::fs::create_dir_all(d).unwrap(); }
        std::fs::write(checkout.join("crates/helper/src/lib.rs"), "pub fn f() {}").unwrap();
        let git = Resolved { package: Package { name: "helper".into(), version: "0.1.0".into(), kind: Kind::Git, source: Some("https://h/x#0".into()), checksum: None }, dir: checkout.join("crates/helper") };
        let krate = Resolved { package: Package { kind: Kind::CratesIo, ..krate("winlib") }, dir: crate_dir.clone() };
        assert_eq!(dependency_binaries(&[git.clone(), krate.clone()]).unwrap(), vec![]);
        std::fs::write(checkout.join("native/helper.obj"), coff_object()).unwrap();
        let err = dependency_binaries(std::slice::from_ref(&git)).unwrap_err();
        assert!(err.contains("git dependency helper 0.1.0 holds machine code") && err.contains("native/helper.obj"), "{err}");
        // A second package of the same checkout: one scan, one message (review r3 P3).
        let sibling = Resolved { package: Package { name: "helper-macros".into(), ..git.package.clone() }, dir: checkout.join("crates/macros") };
        let err = dependency_binaries(&[git.clone(), sibling]).unwrap_err();
        assert_eq!(err.matches("native/helper.obj").count(), 1, "{err}");
        std::fs::write(crate_dir.join("lib/win.lib"), b"!<arch>\nimport").unwrap();
        let sha = crate::sha256_hex(b"!<arch>\nimport");
        let pre = dependency_binaries(&[krate]).unwrap();
        assert_eq!(pre, vec![Prebuilt { name: "winlib".into(), version: "1.0.0".into(), file: "lib/win.lib".into(), sha256: sha }]);
        let outside = Resolved { dir: t.path().join("elsewhere"), ..git };
        assert!(dependency_binaries(&[outside]).unwrap_err().contains("not in a cargo git checkout"));
    }

    // R19 (2026-10-29): a crate that passed the floors brings its prebuilt files (as its
    // Rust code, R3); in a crate below a floor, or with no gate decision, a file needs an approval
    // for the exact version.
    #[test]
    fn prebuilt_files_pass_with_the_crate_reputation_or_an_approval() {
        let sha = crate::sha256_hex(b"!<arch>\nimport");
        let pre = [Prebuilt { name: "winlib".into(), version: "1.0.0".into(), file: "lib/win.lib".into(), sha256: sha.clone() }];
        let checked = |basis: Basis| vec![Checked { package: Package { kind: Kind::CratesIo, ..krate("winlib") }, basis, why: vec![], facts: None, ledger: None, previous: None, re_review: false }];
        prebuilt_check_with(&checked(Basis::Reputation), &pre, APPROVED).unwrap();
        let err = prebuilt_check_with(&checked(Basis::Read), &pre, APPROVED).unwrap_err();
        assert!(err.contains(&format!("lib/win.lib in crates.io winlib 1.0.0 (SHA-256 {sha})")) && err.contains("below the reputation floors"), "{err}");
        assert!(prebuilt_check_with(&[], &pre, APPROVED).unwrap_err().contains("not in the dependency list"));
        let approved = |publisher: &str| serde_json::json!({ "binaries": [{ "sha256": sha, "publisher": publisher, "approved": "2026-10-29", "note": "test" }] }).to_string();
        assert!(prebuilt_check_with(&checked(Basis::Read), &pre, &approved("crates.io winlib 1.0.1")).is_err(), "another version is another approval");
        prebuilt_check_with(&checked(Basis::Read), &pre, &approved("crates.io winlib 1.0.0")).unwrap();
        assert!(prebuilt_check_with(&[], &pre, &approved("crates.io winlib 1.0.0")).unwrap_err().contains("not in the dependency list"), "an approval does not cover a crate that is not in the list");
    }
}
