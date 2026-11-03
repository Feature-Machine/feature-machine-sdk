// Reproducible feature build (publishing pipeline plan, L2): exports the raw tree of one commit,
// builds the feature's cdylib with our pinned compiler and our flags only, packs it as `feature`
// does, and writes build.json. The bytes then depend only on the commit and the toolchain, so a
// second build gives the same package SHA-256 (the T3 proof of the mod platform plan: /Brepro and
// path remaps). The pipeline runner job calls only this command.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const TOOLCHAIN_FILE: &str = include_str!("../../../rust-toolchain.toml");
/// What the remaps put in place of the build folder and the cargo home in the library.
const WORK_AS: &str = "/fm";
const CARGO_AS: &str = "/cargo";

/// fm_pack build <repo> <commit> <feature dir> <out>: <feature dir> is relative to the repo root
/// ("." for a feature at the root). <out> must be new or empty; <out>/work stays for the caller.
pub fn command(repo: &str, commit: &str, feature: &str, out: &str) -> Result<(), String> {
    let out = clean_out(out)?;
    let (facts, pinned) = prepare_in(Path::new(repo), commit, feature, &out, false)?;
    build_in(&out, &facts, &pinned, None)
}

/// The facts of one commit that the review trusts. `prepare` writes them to prepare.json in the
/// review job, where no creator code runs; build.json repeats them, and review-brief compares the
/// two, because the build runs creator code (build.rs, proc macros, the library load).
pub(crate) const FACTS: [&str; 8] = ["commit", "source", "feature", "manifest", "sdk", "packages", "binaries", "prebuilt"];
/// The file of the trusted facts in an out folder of `prepare`.
pub(crate) const PREPARED: &str = "prepare.json";

/// fm_pack prepare <repo> <commit> <feature dir> <out>: the half of `build` that runs no creator
/// code (submission review plan, S1 step 4): the export, the checks, the gate facts and the
/// approved binaries, plus `cargo vendor`, so that `build-prepared` on another runner needs no
/// network and no credential. Writes prepare.json.
pub fn prepare(repo: &str, commit: &str, feature: &str, out: &str) -> Result<(), String> {
    let out = clean_out(out)?;
    let (facts, _) = prepare_in(Path::new(repo), commit, feature, &out, true)?;
    std::fs::write(out.join(PREPARED), serde_json::to_vec_pretty(&facts).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    println!("{}", serde_json::json!({ "prepared": out, "commit": facts["commit"] }));
    Ok(())
}

/// fm_pack build-prepared <dir>: builds the output of `prepare` offline, from its vendored
/// sources only, and writes build.json and the package as `build` does.
pub fn build_prepared(dir: &str) -> Result<(), String> {
    if !cfg!(windows) { return Err("fm_pack build makes Windows libraries (MSVC flags); run it on Windows".into()); }
    let dir = std::path::absolute(dir).map_err(|e| format!("{dir}: {e}"))?;
    if dir.join("build.json").exists() { return Err(format!("{} is already built", dir.display())); }
    let facts: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join(PREPARED)).map_err(|e| format!("{PREPARED}: {e}"))?).map_err(|e| format!("{PREPARED}: {e}"))?;
    let pinned = Pinned::find()?;
    if facts["channel"].as_str() != Some(pinned.channel.as_str()) { return Err(format!("{PREPARED} is for toolchain {}, this fm_pack builds {}", facts["channel"], pinned.channel)); }
    let vendor = facts["vendor"].as_str().ok_or(format!("{PREPARED}: no vendor config"))?;
    build_in(&dir, &facts, &pinned, Some(vendor))
}

/// The record that holds the gate facts of an out folder: prepare.json when the folder has one
/// (the trusted facts of the review job), else build.json.
pub(crate) fn facts_record(dir: &Path) -> Result<serde_json::Value, String> {
    let file = if dir.join(PREPARED).exists() { PREPARED } else { "build.json" };
    serde_json::from_slice(&std::fs::read(dir.join(file)).map_err(|e| format!("{file}: {e}"))?).map_err(|e| format!("{file}: {e}"))
}

/// The out folder as an absolute path, new or empty (a build starts in a clean folder).
fn clean_out(out: &str) -> Result<PathBuf, String> {
    if !cfg!(windows) { return Err("fm_pack build makes Windows libraries (MSVC flags); run it on Windows".into()); }
    let out = std::path::absolute(out).map_err(|e| format!("{out}: {e}"))?;
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    if std::fs::read_dir(&out).map_err(|e| e.to_string())?.next().is_some() {
        return Err(format!("{} is not empty (a build starts in a clean folder)", out.display()));
    }
    Ok(out)
}

/// Export, checks and facts (the FACTS keys). With `vendor`, also the toolchain channel and the
/// config that `cargo vendor` prints for the sources in <out>/work/vendor.
fn prepare_in(repo: &Path, commit: &str, feature: &str, out: &Path, vendor: bool) -> Result<(serde_json::Value, Pinned), String> {
    let rel = feature_dir(feature)?;
    let sha = resolve(repo, commit)?;
    let (work, src) = (out.join("work"), out.join("work").join("src"));
    export(repo, &sha, &src)?;
    let dir = src.join(&rel);
    for file in ["Cargo.toml", fm_install::FEATURE_MANIFEST] {
        if !dir.join(file).is_file() { return Err(format!("{feature}/{file} is not in commit {sha}")); }
    }
    let manifest_file = dir.join(fm_install::FEATURE_MANIFEST);
    let manifest: fm_install::feature::Manifest = serde_json::from_slice(&std::fs::read(&manifest_file).map_err(|e| format!("{}: {e}", manifest_file.display()))?)
        .map_err(|e| format!("{feature}/{}: {e}", fm_install::FEATURE_MANIFEST))?;
    let pinned = Pinned::find()?;
    let source = origin(repo);
    let sdk = sdk_source(&src, &dir.join("Cargo.toml"), source.as_deref(), &pinned)?;
    let root = cargo_text(&pinned, &src, &dir.join("Cargo.toml"), &["locate-project", "--workspace", "--message-format", "plain"])?;
    let lock = Path::new(root.trim()).with_file_name("Cargo.lock");
    crate::deps::locked_git(&lock, &sdk, &out.join("git-check"))?;
    // Everything that builds or links must be observable (submission review plan R1): the
    // package list goes to the record for `fm_pack deps`, and closed-source binaries come only
    // from binaries.json, approved and fetched here, never from the commit or from build.rs.
    let resolved = build_packages(&src, &dir.join("Cargo.toml"), &pinned, &sdk)?;
    crate::deps::committed_binaries(&src)?;
    let prebuilt = crate::deps::dependency_binaries(&resolved)?;
    let packages = crate::deps::with_checksums(resolved.into_iter().map(|r| r.package).collect(), &lock)?;
    let binaries = crate::deps::binary_lock(&dir)?;
    crate::deps::fetch_binaries(&binaries, &work.join("binaries"))?;
    // The manifest of the commit: review-brief refuses a package whose feature.json differs
    // (hosts, files, commands), because build code can change the tree before the pack.
    let mut facts = serde_json::json!({ "commit": sha, "source": source, "feature": feature, "manifest": manifest, "sdk": sdk, "packages": packages, "binaries": binaries, "prebuilt": prebuilt });
    if vendor {
        facts["channel"] = pinned.channel.clone().into();
        facts["vendor"] = cargo_vendor(&pinned, &src, &dir.join("Cargo.toml"), &work.join("vendor"))?.into();
    }
    Ok((facts, pinned))
}

/// Compiles and packs the export in <out>/work/src (from `prepare_in`), then writes build.json:
/// the facts, the toolchain and flags, the package and the libraries. With `vendor` (the config
/// of `cargo vendor`), cargo builds offline from <out>/work/vendor.
fn build_in(out: &Path, facts: &serde_json::Value, pinned: &Pinned, vendor: Option<&str>) -> Result<(), String> {
    let (work, src) = (out.join("work"), out.join("work").join("src"));
    let dir = src.join(feature_dir(facts["feature"].as_str().ok_or("the facts have no feature dir")?)?);
    let config = vendor.map(|v| vendor_config(v, &work)).transpose()?;
    let flags = rustflags(&work, &pinned.cargo_home);
    let libs = cargo_build(&src, &dir.join("Cargo.toml"), &work.join("target"), pinned, &flags, &work.join("binaries"), config.as_deref())?;
    let packed = out.join("package.zip");
    let (m, sha256, size) = crate::pack_feature(&dir, &packed, &libs)?;
    let name = format!("{}-{}.zip", m.id, m.version);
    std::fs::rename(&packed, out.join(&name)).map_err(|e| e.to_string())?;
    let libraries: Vec<_> = libs.iter().map(|l| {
        let bytes = std::fs::read(l).map_err(|e| format!("{}: {e}", l.display()))?;
        Ok(serde_json::json!({ "file": l.file_name().map(|f| f.to_string_lossy()), "sha256": crate::sha256_hex(&bytes), "tools": rich_tools(&bytes) }))
    }).collect::<Result<_, String>>()?;
    let mut record = serde_json::json!({
        "toolchain": { "channel": pinned.channel, "rustc": pinned.version()? },
        "rustflags": record_flags(&flags, &work, &pinned.cargo_home),
        "package": { "file": name, "id": m.id, "version": m.version, "sha256": sha256, "size": size },
        "libraries": libraries,
    });
    for key in FACTS { record[key] = facts[key].clone(); }
    std::fs::write(out.join("build.json"), serde_json::to_vec_pretty(&record).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    println!("{}", serde_json::json!({ "package": out.join(&name), "sha256": sha256, "commit": facts["commit"] }));
    Ok(())
}

/// `cargo vendor` of the feature into `dest` (the locked versions, one folder each); returns the
/// source replacement config that cargo prints. It fetches sources and runs no code of the commit.
fn cargo_vendor(pinned: &Pinned, src: &Path, manifest: &Path, dest: &Path) -> Result<String, String> {
    let out = facts_cargo(pinned, src)?.args(["vendor", "--locked", "--versioned-dirs", "--manifest-path"]).arg(manifest).arg(dest)
        .stdout(Stdio::piped()).stderr(Stdio::inherit()).output().map_err(|e| format!("cargo: {e}"))?;
    if !out.status.success() { return Err(format!("cargo vendor of {} failed ({})", manifest.display(), out.status)); }
    String::from_utf8(out.stdout).map_err(|e| format!("cargo vendor: {e}"))
}

/// The config of `cargo vendor` with each `directory` set to <work>/vendor of this machine (the
/// folder moves with the job artifact), written to <work>/vendor-config.toml.
fn vendor_config(text: &str, work: &Path) -> Result<PathBuf, String> {
    let mut config: toml::Table = toml::from_str(text).map_err(|e| format!("vendor config: {e}"))?;
    let vendor = work.join("vendor").to_string_lossy().into_owned();
    for (_, s) in config.get_mut("source").and_then(|s| s.as_table_mut()).into_iter().flatten() {
        if let Some(d) = s.as_table_mut().and_then(|t| t.get_mut("directory")) { *d = vendor.clone().into(); }
    }
    let path = work.join("vendor-config.toml");
    std::fs::write(&path, toml::to_string(&config).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok(path)
}

/// The MSVC tools that made a library, from its PE Rich header: one "product.build x count" text
/// per row (258 is the linker). rustc -vV does not name the linker. One MSVC folder can hold
/// another link.exe build, and that changes the bytes (2026-10-27: 35226 and 35229 gave two
/// trip hashes). None when the file has no PE signature or no Rich header (a non-MSVC linker).
fn rich_tools(bytes: &[u8]) -> Option<Vec<String>> {
    let word = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let pe = word(0x3c)? as usize;
    if bytes.get(pe..pe.checked_add(4)?) != Some(b"PE\0\0") {
        return None;
    }
    let last = pe.checked_sub(8)?;
    // Each "Rich" word up to PE is a candidate (stub text can hold it too). The first one that makes
    // a full header wins: DanS ^ key, three pad words equal to the key, one or more whole rows.
    (0x40..=last).step_by(4).filter(|&o| bytes.get(o..o + 4) == Some(b"Rich")).find_map(|end| {
        let key = word(end + 4)?;
        let start = (0x40..end).step_by(4).rev().find(|&o| word(o) == Some(0x536e_6144 ^ key))?;
        let size = end - start;
        if size < 24 || size % 8 != 0 || (1..4).any(|i| word(start + 4 * i) != Some(key)) {
            return None;
        }
        let rows = (start + 16..end).step_by(8).map(|o| {
            let (id, count) = (word(o)? ^ key, word(o + 4)? ^ key);
            Some(format!("{}.{} x {count}", id >> 16, id & 0xffff))
        });
        rows.collect()
    })
}

/// The feature folder inside the export: "." or a safe relative path (the package path rule).
fn feature_dir(feature: &str) -> Result<PathBuf, String> {
    match feature {
        "." => Ok(PathBuf::new()),
        f => fm_install::feature::safe_rel(f.trim_end_matches('/')).map_err(|e| format!("feature dir: {e}")),
    }
}

/// The full SHA of `commit`. --end-of-options: a commit text that starts with "-" is a name,
/// never a git option.
fn resolve(repo: &Path, commit: &str) -> Result<String, String> {
    let out = git(repo).args(["rev-parse", "--verify", "--quiet", "--end-of-options"]).arg(format!("{commit}^{{commit}}")).output().map_err(|e| format!("git: {e}"))?;
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    match out.status.success() && !sha.is_empty() && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        true => Ok(sha),
        false => Err(format!("{commit} is not a commit of {}", repo.display())),
    }
}

/// Writes the files of commit `sha` into `dest` as the raw blobs of its tree (ls-tree, cat-file):
/// the bytes that the review reads, with no attribute, filter, line-ending or replace setting of
/// this machine or repo (git archive applies those). Nothing from the worktree or the index.
/// A symlink, a submodule, an unsafe name or two names for one Windows file stop the build.
fn export(repo: &Path, sha: &str, dest: &Path) -> Result<(), String> {
    use std::io::{BufRead, Read, Write};
    let list = git(repo).args(["ls-tree", "-r", "-z", "--full-tree", sha]).stderr(Stdio::inherit()).output().map_err(|e| format!("git: {e}"))?;
    if !list.status.success() { return Err(format!("git ls-tree {sha} failed ({})", list.status)); }
    let mut files = vec![];
    for entry in list.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let entry = std::str::from_utf8(entry).map_err(|_| "a path in the commit is not UTF-8".to_string())?;
        let (meta, name) = entry.split_once('\t').ok_or_else(|| format!("git ls-tree: {entry:?}"))?;
        match meta.split(' ').collect::<Vec<_>>().as_slice() {
            ["100644" | "100755", "blob", oid] => {
                let path = dest.join(fm_install::feature::safe_rel(name).map_err(|e| e.to_string())?);
                files.push((oid.to_string(), name.to_string(), path));
            }
            ["120000", ..] => return Err(format!("{name}: the commit has a symlink; a feature build takes files only")),
            ["160000", ..] => return Err(format!("{name}: the commit has a submodule; a feature build takes files only")),
            _ => return Err(format!("{name}: unknown tree entry {meta}")),
        }
    }
    let mut child = git(repo).args(["cat-file", "--batch"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().map_err(|e| format!("git: {e}"))?;
    // The requests go from a thread: git answers while it reads, so one pipe must not wait on the other.
    let requests: String = files.iter().map(|(oid, ..)| format!("{oid}\n")).collect();
    let mut stdin = child.stdin.take().ok_or("git cat-file: no stdin")?;
    let writer = std::thread::spawn(move || stdin.write_all(requests.as_bytes()));
    let mut r = std::io::BufReader::new(child.stdout.take().ok_or("git cat-file: no stdout")?);
    for (oid, name, path) in &files {
        let mut header = String::new();
        r.read_line(&mut header).map_err(|e| e.to_string())?;
        let size: u64 = match header.trim_end().split(' ').collect::<Vec<_>>().as_slice() {
            [o, "blob", n] if o == oid => n.parse().map_err(|_| format!("git cat-file: {header:?}"))?,
            _ => return Err(format!("{name}: git cat-file gave {header:?}")),
        };
        std::fs::create_dir_all(path.parent().unwrap_or(dest)).map_err(|e| e.to_string())?;
        let mut w = std::fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| format!("{name}: {e} (two names for one file on Windows?)"))?;
        if std::io::copy(&mut (&mut r).take(size), &mut w).map_err(|e| format!("{name}: {e}"))? != size { return Err(format!("{name}: short read from git")); }
        r.read_exact(&mut [0u8; 1]).map_err(|e| e.to_string())?;
    }
    writer.join().map_err(|_| "git cat-file writer stopped")?.map_err(|e| format!("git cat-file: {e}"))?;
    drop(r);
    match child.wait().map_err(|e| e.to_string())?.success() {
        true => Ok(()),
        false => Err("git cat-file failed".into()),
    }
}

/// The flags of every build, replacing the repo's own rustflags (CARGO_ENCODED_RUSTFLAGS wins
/// over .cargo/config.toml): our static CRT, a linker output without time stamps, and the build
/// folder and cargo home renamed, so no path of this machine is in the library.
fn rustflags(work: &Path, cargo_home: &Path) -> Vec<String> {
    vec![
        "-C".into(), "target-feature=+crt-static".into(),
        "-C".into(), "link-arg=/Brepro".into(),
        format!("--remap-path-prefix={}={WORK_AS}", work.display()),
        format!("--remap-path-prefix={}={CARGO_AS}", cargo_home.display()),
    ]
}

/// The flags for build.json, with <work> and <cargo> in place of the local paths.
fn record_flags(flags: &[String], work: &Path, cargo_home: &Path) -> Vec<String> {
    flags.iter().map(|f| f.replace(&work.display().to_string(), "<work>").replace(&cargo_home.display().to_string(), "<cargo>")).collect()
}

/// The channel of our rust-toolchain.toml. The build forces it (RUSTUP_TOOLCHAIN), so a creator
/// repository cannot choose another compiler.
fn channel() -> Result<String, String> {
    TOOLCHAIN_FILE.lines()
        .filter_map(|l| l.trim().strip_prefix("channel"))
        .find_map(|l| l.trim().strip_prefix('=').map(|v| v.trim().trim_matches('"').to_string()))
        .filter(|c| !c.is_empty())
        .ok_or_else(|| "rust-toolchain.toml has no channel".into())
}

/// The cargo home as cargo finds it. The build sets CARGO_HOME to this value, so cargo and the
/// remap agree on the path.
fn cargo_home() -> Result<PathBuf, String> {
    let home = std::env::var_os("CARGO_HOME").map(PathBuf::from)
        .or_else(|| std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(|h| PathBuf::from(h).join(".cargo")))
        .ok_or("no CARGO_HOME and no home folder")?;
    std::path::absolute(&home).map_err(|e| e.to_string())
}

/// The pinned compiler: our channel, the cargo home, and the rustc file of that channel.
struct Pinned { channel: String, cargo_home: PathBuf, rustc: PathBuf }

impl Pinned {
    fn find() -> Result<Pinned, String> {
        let (channel, cargo_home) = (channel()?, cargo_home()?);
        let out = toolchain(Command::new("rustc"), &channel, &cargo_home).args(["--print", "sysroot"]).output().map_err(|e| format!("rustc: {e}"))?;
        let sysroot = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        let rustc = sysroot.join("bin").join(format!("rustc{}", std::env::consts::EXE_SUFFIX));
        if !out.status.success() || !rustc.is_file() { return Err(format!("toolchain {channel} is not installed (rustup toolchain install {channel})")); }
        Ok(Pinned { channel, cargo_home, rustc })
    }

    /// `rustc -vV` of the pinned rustc file, for the record.
    fn version(&self) -> Result<String, String> {
        let out = toolchain(Command::new(&self.rustc), &self.channel, &self.cargo_home).arg("-vV").output().map_err(|e| format!("rustc: {e}"))?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

/// cargo with the pinned toolchain. RUSTC and the empty wrappers win over a repo's build.rustc
/// and build.rustc-wrapper, so the compiler is the pinned one that build.json names, whatever
/// the repo's cargo config says (for `cargo metadata` too, which asks rustc for target facts).
fn pinned_cargo(pinned: &Pinned) -> Command {
    let mut c = toolchain(Command::new("cargo"), &pinned.channel, &pinned.cargo_home);
    c.env("RUSTC", &pinned.rustc).env("RUSTC_WRAPPER", "").env("RUSTC_WORKSPACE_WRAPPER", "");
    // git dependencies through the git command: the review job gives the SDK read token to git
    // only (an extraHeader for github.com in GIT_CONFIG_*), never to cargo or a URL.
    c.env("CARGO_NET_GIT_FETCH_WITH_CLI", "true");
    c
}

/// cargo for the facts of `prepare` (tree, metadata, vendor), in the review job that has the
/// credentials. It runs in the folder above the export, so cargo reads no `.cargo/config` of the
/// commit (cargo reads the config of its current folder and the folders above it). The build
/// flags of `rustflags` are the same as in the build, so `cfg(...)` dependencies resolve the same.
fn facts_cargo(pinned: &Pinned, src: &Path) -> Result<Command, String> {
    let work = src.parent().ok_or("the export has no parent folder")?;
    let mut c = pinned_cargo(pinned);
    c.current_dir(work).env("CARGO_ENCODED_RUSTFLAGS", rustflags(work, &pinned.cargo_home).join("\x1f"));
    Ok(c)
}

/// cargo build of one package; returns its cdylib files of this OS (cargo JSON messages).
/// FM_BINARIES_DIR is the folder of the approved closed-source binaries, for build.rs to link.
/// `vendor`: the config file of the vendored sources; the build is then offline.
fn cargo_build(src: &Path, manifest: &Path, target: &Path, pinned: &Pinned, flags: &[String], binaries: &Path, vendor: Option<&Path>) -> Result<Vec<PathBuf>, String> {
    let mut c = pinned_cargo(pinned);
    if let Some(v) = vendor { c.arg("--config").arg(v).arg("--offline"); }
    c.args(["build", "--release", "--locked", "--message-format=json-render-diagnostics", "--manifest-path"]).arg(manifest)
        .current_dir(src).env("CARGO_TARGET_DIR", target).env("CARGO_ENCODED_RUSTFLAGS", flags.join("\x1f")).env("FM_BINARIES_DIR", binaries)
        .stdout(Stdio::piped()).stderr(Stdio::inherit());
    let out = c.output().map_err(|e| format!("cargo: {e}"))?;
    if !out.status.success() { return Err(format!("cargo build of {} failed ({})", manifest.display(), out.status)); }
    let want = std::fs::canonicalize(manifest).map_err(|e| e.to_string())?;
    let mut libs = vec![];
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(m) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let ours = m["reason"] == "compiler-artifact"
            && m["target"]["kind"].as_array().is_some_and(|k| k.iter().any(|k| k == "cdylib"))
            && m["manifest_path"].as_str().and_then(|p| std::fs::canonicalize(p).ok()).as_ref() == Some(&want);
        if !ours { continue; }
        let files = m["filenames"].as_array().into_iter().flatten().filter_map(|f| f.as_str());
        libs.extend(files.map(PathBuf::from).filter(|f| f.extension().and_then(|e| e.to_str()) == Some(std::env::consts::DLL_EXTENSION)));
    }
    if libs.is_empty() { return Err(format!("{} built no cdylib (crate-type = [\"cdylib\"])", manifest.display())); }
    Ok(libs)
}

/// A cargo or rustc command with the pinned toolchain and without the cargo and rustc settings
/// of the calling environment (`cargo run` sets some; a user can set RUSTC_WRAPPER or flags).
fn toolchain(mut c: Command, channel: &str, cargo_home: &Path) -> Command {
    for (k, _) in std::env::vars_os() {
        if inherited_setting(&k.to_string_lossy()) { c.env_remove(k); }
    }
    c.env("RUSTUP_TOOLCHAIN", channel).env("CARGO_HOME", cargo_home);
    c
}

/// An environment variable that changes what cargo or rustc builds. RUSTUP_HOME stays: rustup
/// needs it to find the toolchain.
fn inherited_setting(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n.starts_with("CARGO") || n.starts_with("RUSTC") || n == "RUSTFLAGS" || n == "RUSTDOCFLAGS" || n == "RUSTUP_TOOLCHAIN"
}

/// git in `repo`, with no prompt. GIT_NO_REPLACE_OBJECTS: a local replace ref cannot put other
/// objects under the commit SHA that build.json records.
fn git(repo: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(repo).env("GIT_TERMINAL_PROMPT", "0").env("GIT_NO_REPLACE_OBJECTS", "1").stdin(Stdio::null());
    c
}

/// The SDK repository on GitHub (owner/name), in the Feature-Machine organization (owner
/// decision, 2026-10-27, move plan D9); tools/creator-ci/feature-machine.yml names the same repository.
pub(crate) const SDK_REPOSITORY: &str = "Feature-Machine/feature-machine-sdk";
/// The repository where the SDK is made (tools/sdk-export exports the SDK repository from it;
/// owner decision 2026-10-28, publish job plan). Its own features link the SDK by path.
const SDK_UPSTREAM: &str = "Feature-Machine/p42-feature-machine";

/// The tables that a repo's cargo config may set. The others can swap a dependency (`paths`,
/// `[source]`, `[patch]`), send the fetch elsewhere or run a program in the review job, which has
/// credentials (`[http]`, `[net]`, `[registries]`, `[registry]`, `[credential-provider]`), or
/// change the build (`[unstable]`, `[resolver]`). `build.target` is refused too.
const CONFIG_TABLES: [&str; 6] = ["build", "target", "env", "profile", "term", "alias"];

/// The SDK that the build links, for build.json, from cargo's own resolution (the real workspace
/// lock, after `[patch]`), so a changed copy of the SDK cannot skip its permission checks. The
/// repo's cargo config can set only CONFIG_TABLES, without `build.target` (only
/// `<export>/.cargo/config*` applies: cargo reads the config of its current folder, the export
/// root, and the folders above it).
fn sdk_source(src: &Path, manifest: &Path, origin: Option<&str>, pinned: &Pinned) -> Result<serde_json::Value, String> {
    for name in ["config.toml", "config"] {
        let file = src.join(".cargo").join(name);
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let config: toml::Table = toml::from_str(&text).map_err(|e| format!(".cargo/{name}: {e}"))?;
        let target = config.get("build").and_then(|b| b.get("target")).map(|_| "build.target".to_string());
        if let Some(key) = config.keys().find(|k| !CONFIG_TABLES.contains(&k.as_str())).cloned().or(target) {
            return Err(format!(".cargo/{name} sets `{key}`, which can replace a dependency, the target or the fetch; the build accepts only [{}] (no build.target)", CONFIG_TABLES.join("], [")));
        }
    }
    let host = pinned.version()?.lines().find_map(|l| l.strip_prefix("host: ")).map(str::to_string).ok_or("rustc -vV has no host")?;
    let tree = cargo_text(pinned, src, manifest, &["tree", "--locked", "-e", "normal", "--target", &host, "--prefix", "depth", "-f", "|{p}"])?;
    let meta = cargo_text(pinned, src, manifest, &["metadata", "--locked", "--format-version", "1"])?;
    let meta: serde_json::Value = serde_json::from_str(&meta).map_err(|e| format!("cargo metadata: {e}"))?;
    let sources = linked_sdks(&tree, &meta)?;
    let sdk = sdk_check(&sources.iter().map(Option::as_deref).collect::<Vec<_>>(), origin)?;
    // Cargo.lock names the commit, and cargo builds that commit, not the tag: the tag must be
    // that commit in the SDK repository (review r4). The review job's git has the SDK token.
    if let (Some(tag), Some(commit)) = (sdk["tag"].as_str(), sdk["commit"].as_str()) {
        let out = git(src).args(["ls-remote", "--tags", "--", &format!("https://github.com/{SDK_REPOSITORY}"), &format!("refs/tags/{tag}"), &format!("refs/tags/{tag}^{{}}")])
            .stderr(Stdio::inherit()).output().map_err(|e| format!("git ls-remote: {e}"))?;
        let want = out.status.success().then(|| tag_commit(&String::from_utf8_lossy(&out.stdout), tag)).flatten()
            .ok_or_else(|| format!("the SDK repository has no tag {tag}"))?;
        if !want.eq_ignore_ascii_case(commit) { return Err(format!("fm_feature_sdk tag {tag} is commit {want}, but Cargo.lock names {commit}; run cargo update -p fm_feature_sdk")); }
    }
    Ok(sdk)
}

/// The commit of `refs/tags/<tag>` in `git ls-remote` output: the peeled `^{}` line of an
/// annotated tag, else the tag line (a lightweight tag names the commit itself).
fn tag_commit(text: &str, tag: &str) -> Option<String> {
    let find = |r: &str| text.lines().find_map(|l| l.split_once('\t').filter(|(_, n)| *n == r).map(|(s, _)| s.to_string()));
    find(&format!("refs/tags/{tag}^{{}}")).or_else(|| find(&format!("refs/tags/{tag}")))
}

/// The packages that build or link into the feature (deps.rs): `cargo tree -e normal,build` of
/// the feature for the host target, so build scripts and proc macros count too; the full source
/// of each one from `cargo metadata`.
fn build_packages(src: &Path, manifest: &Path, pinned: &Pinned, sdk: &serde_json::Value) -> Result<Vec<crate::deps::Resolved>, String> {
    let host = pinned.version()?.lines().find_map(|l| l.strip_prefix("host: ")).map(str::to_string).ok_or("rustc -vV has no host")?;
    let tree = cargo_text(pinned, src, manifest, &["tree", "--locked", "-e", "normal,build", "--target", &host, "--prefix", "none", "-f", "{p}"])?;
    let meta = cargo_text(pinned, src, manifest, &["metadata", "--locked", "--format-version", "1"])?;
    let meta: serde_json::Value = serde_json::from_str(&meta).map_err(|e| format!("cargo metadata: {e}"))?;
    crate::deps::packages(&tree, &meta, src, sdk)
}

/// The stdout of one pinned cargo command on the feature manifest (`--locked` in `args`).
fn cargo_text(pinned: &Pinned, src: &Path, manifest: &Path, args: &[&str]) -> Result<String, String> {
    let out = facts_cargo(pinned, src)?.args(args).arg("--manifest-path").arg(manifest)
        .stdout(Stdio::piped()).stderr(Stdio::inherit()).output().map_err(|e| format!("cargo: {e}"))?;
    if !out.status.success() { return Err(format!("cargo {} --locked of {} failed ({}): commit an up-to-date Cargo.lock", args[0], manifest.display(), out.status)); }
    String::from_utf8(out.stdout).map_err(|e| format!("cargo {}: {e}", args[0]))
}

/// The sources of the fm_feature_sdk packages that the feature library links. `cargo tree -e
/// normal --prefix depth -f |{p}` of the feature manifest decides which ("|" ends the depth, as a
/// package name can start with digits and cannot hold "|", review r6): it resolves features for
/// this package only, as `cargo build` does (review L4 R3-B1: `cargo metadata` unifies the features
/// of all members), and skips dev- and build-dependencies. It shows a git commit short, so the
/// full source comes from `cargo metadata`. A path SDK gives None.
fn linked_sdks(tree: &str, meta: &serde_json::Value) -> Result<Vec<Option<String>>, String> {
    let mut shown: Vec<&str> = tree.lines().filter_map(|l| {
        let (depth, entry) = l.split_once('|')?;
        (!depth.is_empty() && depth != "0" && entry.starts_with("fm_feature_sdk v")).then(|| entry.trim_end_matches(" (*)"))
    }).collect();
    shown.sort_unstable();
    shown.dedup();
    let sources: Vec<&str> = meta["packages"].as_array().into_iter().flatten()
        .filter(|p| p["name"] == "fm_feature_sdk").filter_map(|p| p["source"].as_str()).collect();
    shown.into_iter().map(|entry| {
        let source = entry.split_once(" (").map(|(_, s)| s.strip_suffix(')').unwrap_or(s));
        match source.and_then(|s| s.rsplit_once('#')).filter(|(url, short)| url.contains("://") && !short.is_empty()) {
            Some((url, short)) => {
                let prefix = format!("git+{url}#");
                match sources.iter().filter(|s| s.strip_prefix(&prefix).is_some_and(|c| c.starts_with(short))).collect::<Vec<_>>()[..] {
                    [one] => Ok(Some(one.to_string())),
                    _ => Err(format!("cargo metadata has no single source for `{entry}`")),
                }
            }
            None => Ok(match source {
                Some(path) if Path::new(path).is_absolute() => None,
                other => Some(other.unwrap_or("crates.io").to_string()),
            }),
        }
    }).collect()
}

/// The rule for the resolved fm_feature_sdk packages (their sources): exactly one, from the git
/// source of the SDK repository at a release tag (`tag = ...`), or a path SDK in the SDK
/// repository itself or in a local repository (no origin: development, as this one).
fn sdk_check(sdks: &[Option<&str>], origin: Option<&str>) -> Result<serde_json::Value, String> {
    let want = format!("the git dependency on https://github.com/{SDK_REPOSITORY} with a release tag (tag = \"...\")");
    let source = match sdks {
        [] => return Err("the feature does not link fm_feature_sdk".into()),
        [one] => *one,
        _ => return Err("the build has two fm_feature_sdk packages; a feature links one SDK".into()),
    };
    let Some(source) = source else {
        return match origin.is_none_or(|o| is_github_repo(o, SDK_REPOSITORY) || is_github_repo(o, SDK_UPSTREAM)) {
            true => Ok(serde_json::Value::Null),
            false => Err(format!("fm_feature_sdk is a path dependency; it must be {want}")),
        };
    };
    let bad = || format!("fm_feature_sdk comes from {}; it must be {want}", crate::deps::redact(source));
    let (url, commit) = source.strip_prefix("git+").and_then(|s| s.rsplit_once('#')).ok_or_else(bad)?;
    let (url, query) = url.split_once('?').unwrap_or((url, ""));
    let tag = query.split('&').find_map(|kv| kv.strip_prefix("tag=")).filter(|t| !t.is_empty());
    match (is_sdk_url(url), tag, commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit())) {
        (true, Some(tag), true) => Ok(serde_json::json!({ "repository": format!("https://github.com/{SDK_REPOSITORY}"), "tag": tag, "commit": commit })),
        _ => Err(bad()),
    }
}

/// Whether a git source URL is the SDK repository over https, the one form that the git rule of
/// the dependency gate (R8, deps.rs) and the read token of the review job accept: ".git" and the
/// letter case can change, the transport cannot (review r3).
pub(crate) fn is_sdk_url(url: &str) -> bool {
    url.get(..19).is_some_and(|h| h.eq_ignore_ascii_case("https://github.com/")) && is_github_repo(url, SDK_REPOSITORY)
}

/// Whether a remote URL or remote id (`remote_id`) is the GitHub repository `repo` (owner/name):
/// GitHub over https, http, ssh or git, or the scp form, with or without ".git", in any case.
pub(crate) fn is_github_repo(url: &str, repo: &str) -> bool {
    let rest = ["https://github.com/", "http://github.com/", "ssh://github.com/", "ssh://git@github.com/", "git://github.com/", "github.com:", "git@github.com:"]
        .into_iter().find_map(|p| url.get(..p.len()).filter(|h| h.eq_ignore_ascii_case(p)).map(|_| &url[p.len()..]));
    rest.is_some_and(|r| r.trim_end_matches('/').trim_end_matches(".git").eq_ignore_ascii_case(repo))
}

/// The origin of the repo for build.json, when it is a remote (this repo has none).
fn origin(repo: &Path) -> Option<String> {
    let out = git(repo).args(["remote", "get-url", "origin"]).stderr(Stdio::null()).output().ok()?;
    if !out.status.success() { return None; }
    remote_id(String::from_utf8_lossy(&out.stdout).trim())
}

/// A remote URL without its user part (it can hold a token): "scheme://host/path" for http(s),
/// ssh and git URLs, "host:path" for the scp form. A local path or file:// URL is a path of
/// this machine, so it gives None.
fn remote_id(url: &str) -> Option<String> {
    let strip_user = |a: &str| a.rsplit_once('@').map_or(a, |(_, h)| h).to_string();
    for scheme in ["https://", "http://", "ssh://", "git://"] {
        if let Some(rest) = url.strip_prefix(scheme) {
            let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
            let host = strip_user(authority);
            return (!host.is_empty() && !path.is_empty()).then(|| format!("{scheme}{host}/{path}"));
        }
    }
    let (left, path) = url.split_once(':')?;
    let host = strip_user(left);
    let scp = host.len() > 1 && !left.contains(['/', '\\']) && !path.is_empty() && !path.starts_with(['/', '\\']);
    scp.then(|| format!("{host}:{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A git repo in a temp folder, with an empty global config: the fixture must not depend on
    /// the user's git settings (signing, hooks). The export itself runs with the normal config.
    struct Repo { _t: tempfile::TempDir, dir: PathBuf, config: PathBuf }
    impl Repo {
        fn new() -> Repo {
            let t = tempfile::tempdir().unwrap();
            let (dir, config) = (t.path().join("repo"), t.path().join("gitconfig"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&config, "[user]\n\tname = t\n\temail = t@example.com\n").unwrap();
            let r = Repo { _t: t, dir, config };
            r.git(&["init", "-q"]);
            r
        }
        fn git(&self, args: &[&str]) -> String {
            let out = Command::new("git").arg("-C").arg(&self.dir).args(args).env("GIT_CONFIG_GLOBAL", &self.config).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        fn commit(&self, files: &[(&str, &str)]) -> String {
            for (name, text) in files {
                let path = self.dir.join(name);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, text).unwrap();
            }
            self.git(&["add", "-A"]);
            self.git(&["commit", "-q", "-m", "c"]);
            self.git(&["rev-parse", "HEAD"])
        }
    }

    // The export holds the committed bytes only. Repo settings outside the commit change nothing
    // (review R1-B2): CRLF conversion, info/attributes with export-ignore and eol=crlf, a replace
    // ref for a blob. Also no worktree edit, no untracked file; index and status do not change.
    #[test]
    fn the_export_is_the_commit_and_nothing_else() {
        let r = Repo::new();
        let sha = r.commit(&[(".gitattributes", "* text=auto\nskip.txt export-ignore\n"), ("a/b.txt", "one\ntwo\n"), ("skip.txt", "kept\n")]);
        r.git(&["config", "core.autocrlf", "true"]);
        std::fs::write(r.dir.join(".git/info/attributes"), "* export-ignore\n*.txt eol=crlf\n").unwrap();
        std::fs::write(r.dir.join("other.txt"), "replaced\n").unwrap();
        let (old, new) = (r.git(&["rev-parse", "HEAD:a/b.txt"]), r.git(&["hash-object", "-w", "other.txt"]));
        r.git(&["replace", &old, &new]);
        std::fs::write(r.dir.join("a/b.txt"), "changed\n").unwrap();
        let status = r.git(&["status", "--porcelain"]);
        let dest = r._t.path().join("export");
        export(&r.dir, &sha, &dest).unwrap();
        assert_eq!(std::fs::read(dest.join("a/b.txt")).unwrap(), b"one\ntwo\n");
        assert_eq!(std::fs::read(dest.join("skip.txt")).unwrap(), b"kept\n");
        assert!(!dest.join("other.txt").exists());
        assert_eq!(r.git(&["status", "--porcelain"]), status);
        assert_eq!(resolve(&r.dir, "HEAD").unwrap(), sha);
    }

    // A submodule in the commit stops the export too.
    #[test]
    fn a_submodule_in_the_commit_is_refused() {
        let r = Repo::new();
        let sha = r.commit(&[("f.txt", "x")]);
        r.git(&["update-index", "--add", "--cacheinfo", &format!("160000,{sha},sub")]);
        r.git(&["commit", "-q", "-m", "sub"]);
        let err = export(&r.dir, &r.git(&["rev-parse", "HEAD"]), &r._t.path().join("export")).unwrap_err();
        assert!(err.contains("submodule"), "{err}");
    }

    // A repo's cargo config cannot choose the compiler (review R1-B1): build.rustc and both
    // wrappers name programs that do not exist, and the build still runs with the pinned rustc.
    #[cfg(windows)]
    #[test]
    fn a_repo_cannot_choose_the_compiler() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        std::fs::create_dir_all(src.join("src")).unwrap();
        std::fs::create_dir_all(src.join(".cargo")).unwrap();
        std::fs::write(src.join("Cargo.toml"), "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\ncrate-type = [\"cdylib\"]\n\n[workspace]\n").unwrap();
        std::fs::write(src.join("Cargo.lock"), "version = 4\n\n[[package]]\nname = \"probe\"\nversion = \"0.1.0\"\n").unwrap();
        std::fs::write(src.join("src/lib.rs"), "#[no_mangle]\npub extern \"C\" fn probe() -> u32 { 7 }\n").unwrap();
        std::fs::write(src.join(".cargo/config.toml"), "[build]\nrustc = \"no-such-rustc\"\nrustc-wrapper = \"no-such-wrapper\"\nrustc-workspace-wrapper = \"no-such-wrapper\"\n").unwrap();
        let pinned = Pinned::find().unwrap();
        let flags = rustflags(t.path(), &pinned.cargo_home);
        let libs = cargo_build(&src, &src.join("Cargo.toml"), &t.path().join("target"), &pinned, &flags, &t.path().join("binaries"), None).unwrap();
        assert_eq!(libs.len(), 1, "{libs:?}");
        assert!(pinned.version().unwrap().contains(&format!("release: {}", pinned.channel)));
    }

    // build.json keeps only a remote id: no user part (it can hold a token), no local path
    // (review R1-N1).
    #[test]
    fn the_record_keeps_only_a_remote_origin() {
        assert_eq!(remote_id("https://x-access-token:secret@github.com/org/f.git").as_deref(), Some("https://github.com/org/f.git"));
        assert_eq!(remote_id("ssh://git@github.com/org/f").as_deref(), Some("ssh://github.com/org/f"));
        assert_eq!(remote_id("git@github.com:org/f.git").as_deref(), Some("github.com:org/f.git"));
        for local in [r"C:\repos\f", "C:/repos/f", "/home/u/f", "file:///C:/repos/f", r"\\server\share\f", "../f", "https://github.com"] {
            assert_eq!(remote_id(local), None, "{local}");
        }
    }

    // A symlink in the commit stops the export: the build takes regular files only.
    #[test]
    fn a_symlink_in_the_commit_is_refused() {
        let r = Repo::new();
        r.commit(&[("f.txt", "x")]);
        let blob = r.git(&["hash-object", "-w", "f.txt"]);
        r.git(&["update-index", "--add", "--cacheinfo", &format!("120000,{blob},link")]);
        r.git(&["commit", "-q", "-m", "link"]);
        let sha = r.git(&["rev-parse", "HEAD"]);
        let err = export(&r.dir, &sha, &r._t.path().join("export")).unwrap_err();
        assert!(err.contains("symlink"), "{err}");
    }

    // An unknown commit or an option-like text is an error, never a git option.
    #[test]
    fn only_a_commit_resolves() {
        let r = Repo::new();
        r.commit(&[("f.txt", "x")]);
        for bad in ["nope", "--output=x", "-h", "HEAD:f.txt"] {
            assert!(resolve(&r.dir, bad).is_err(), "{bad}");
        }
        assert!(!r.dir.join("x").exists());
    }

    // The feature dir follows the package path rule; "." is the repo root.
    #[test]
    fn the_feature_dir_is_a_safe_relative_path() {
        assert_eq!(feature_dir(".").unwrap(), PathBuf::new());
        assert_eq!(feature_dir("features/trip/").unwrap(), Path::new("features").join("trip"));
        for bad in ["../x", "C:/x", "/x", "a/../b", "a\\b", ""] {
            assert!(feature_dir(bad).is_err(), "{bad}");
        }
    }

    // The flags are ours: the static CRT that our config sets, /Brepro, and both remaps. The
    // record hides the local paths. The toolchain is the pinned channel.
    #[test]
    fn the_build_uses_our_flags_and_toolchain() {
        let (work, cargo) = (Path::new(r"F:\b x\work"), Path::new(r"C:\Users\u\.cargo"));
        let flags = rustflags(work, cargo);
        assert!(flags.contains(&"target-feature=+crt-static".to_string()) && flags.contains(&"link-arg=/Brepro".to_string()));
        assert!(flags.contains(&format!(r"--remap-path-prefix=F:\b x\work={WORK_AS}")));
        assert!(flags.contains(&format!(r"--remap-path-prefix=C:\Users\u\.cargo={CARGO_AS}")));
        let shown = record_flags(&flags, work, cargo);
        assert!(shown.contains(&format!("--remap-path-prefix=<work>={WORK_AS}")) && shown.contains(&format!("--remap-path-prefix=<cargo>={CARGO_AS}")));
        let c = channel().unwrap();
        assert!(c.chars().next().is_some_and(|c| c.is_ascii_digit()) && TOOLCHAIN_FILE.contains(&format!("channel = \"{c}\"")), "{c}");
    }

    // Settings from the calling environment that change a build are removed; rustup's home stays.
    #[test]
    fn caller_build_settings_do_not_reach_the_build() {
        for k in ["CARGO_TARGET_DIR", "CARGO_BUILD_RUSTFLAGS", "RUSTC_WRAPPER", "RUSTFLAGS", "RUSTUP_TOOLCHAIN", "cargo_profile_release_lto"] {
            assert!(inherited_setting(k), "{k}");
        }
        for k in ["RUSTUP_HOME", "PATH", "USERPROFILE"] {
            assert!(!inherited_setting(k), "{k}");
        }
    }

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    // The SDK must come from the public SDK repository at a release tag (owner decision, plan L4): a fork,
    // another source, no tag, a path SDK in a creator repository or two SDKs stop the build.
    #[test]
    fn the_sdk_comes_from_the_public_repository_at_a_tag() {
        let creator = Some("https://github.com/creator/feature");
        for url in [format!("git+https://github.com/{SDK_REPOSITORY}?tag=v0.1.0#{SHA}"), format!("git+https://github.com/feature-machine/feature-machine-sdk.git?tag=v1#{SHA}")] {
            let sdk = sdk_check(&[Some(&url)], creator).unwrap();
            assert_eq!((sdk["commit"].as_str(), sdk["tag"].as_str().is_some()), (Some(SHA), true), "{url}");
        }
        let refused = [
            (format!("git+https://github.com/someone/feature-machine-sdk?tag=v0.1.0#{SHA}"), "comes from git+https://github.com/someone"),
            (format!("git+https://github.com/parallel42/feature-machine-sdk?tag=v0.1.0#{SHA}"), "comes from git+https://github.com/parallel42"),
            (format!("git+https://github.com/{SDK_REPOSITORY}-fork?tag=v1#{SHA}"), "comes from"),
            (format!("git+https://github.com/{SDK_REPOSITORY}?rev=abc#{SHA}"), "release tag"),
            (format!("git+https://github.com/{SDK_REPOSITORY}?branch=main#{SHA}"), "release tag"),
            (format!("git+https://github.com/{SDK_REPOSITORY}#{SHA}"), "release tag"),
            (format!("git+https://github.com/{SDK_REPOSITORY}?tag=v1#abc"), "comes from"),
            ("registry+https://github.com/rust-lang/crates.io-index".into(), "comes from registry"),
            (format!("git+ssh://git@github.com/{SDK_REPOSITORY}?tag=v1#{SHA}"), "comes from git+ssh"),
            (format!("git+http://github.com/{SDK_REPOSITORY}?tag=v1#{SHA}"), "comes from git+http"),
        ];
        for (url, want) in refused {
            let err = sdk_check(&[Some(&url)], creator).unwrap_err();
            assert!(err.contains(want), "{want}: {err}");
        }
        assert!(sdk_check(&[None], creator).unwrap_err().contains("path dependency"));
        assert!(sdk_check(&[], creator).unwrap_err().contains("does not link"));
        let tag = format!("git+https://github.com/{SDK_REPOSITORY}?tag=v1#{SHA}");
        assert!(sdk_check(&[Some(&tag), Some(&tag)], creator).unwrap_err().contains("two fm_feature_sdk"));
        // A path SDK builds only in the SDK repository, in the repository that makes it, or in a
        // local repository.
        for origin in [None, Some("https://github.com/feature-machine/feature-machine-sdk.git"), Some("git@github.com:Feature-Machine/feature-machine-sdk.git"), Some("https://github.com/Feature-Machine/p42-feature-machine")] {
            assert_eq!(sdk_check(&[None], origin).unwrap(), serde_json::Value::Null, "{origin:?}");
        }
        assert!(sdk_check(&[None], Some("github.com:other/feature-machine-sdk")).is_err());
        assert!(sdk_check(&[None], Some("https://github.com/other/p42-feature-machine")).is_err());
        let upstream = format!("git+https://github.com/{SDK_UPSTREAM}?tag=v1#{SHA}");
        assert!(sdk_check(&[Some(&upstream)], None).unwrap_err().contains("comes from"), "a git SDK comes from the SDK repository only");
    }

    // cargo tree decides which SDK the feature links; cargo metadata gives the full git source of
    // its short commit. Repeated entries (*) count once; the root line (depth 0) is the feature.
    #[test]
    fn the_linked_sdk_comes_from_cargo_tree() {
        let git = format!("git+https://github.com/{SDK_REPOSITORY}?tag=v1#{SHA}");
        let v2 = format!("git+https://github.com/{SDK_REPOSITORY}?tag=v2#{}", "f".repeat(40));
        let meta = serde_json::json!({ "packages": [
            { "name": "fm_feature_sdk", "source": git }, { "name": "fm_feature_sdk", "source": v2 }, { "name": "fm_feature_sdk", "source": null }] });
        let line = |tag: &str, short: &str| format!("fm_feature_sdk v0.1.0 (https://github.com/{SDK_REPOSITORY}?tag={tag}#{short})");
        let tree = format!("0|probe v0.1.0 (C:\\x\\probe)\n1|{}\n2|serde v1.0.0\n1|helper v0.1.0 (C:\\x\\helper)\n2|{} (*)\n", line("v1", &SHA[..8]), line("v1", &SHA[..8]));
        assert_eq!(linked_sdks(&tree, &meta).unwrap(), vec![Some(git.clone())]);
        assert_eq!(linked_sdks(&format!("1|{}\n3|{}\n", line("v1", &SHA[..8]), line("v2", "ffffffff")), &meta).unwrap().len(), 2);
        let dir = std::env::temp_dir();
        assert_eq!(linked_sdks(&format!("0|x v0.1.0 (C:\\x)\n1|fm_feature_sdk v0.1.0 ({})\n", dir.display()), &meta).unwrap(), vec![None]);
        assert_eq!(linked_sdks("1|fm_feature_sdk v0.1.0\n", &meta).unwrap(), vec![Some("crates.io".to_string())]);
        assert!(linked_sdks("0|fm_feature_sdk v0.1.0 (C:\\sdk)\n", &meta).unwrap().is_empty());
        assert!(linked_sdks(&format!("1|{}\n", line("v1", "deadbeef")), &meta).unwrap_err().contains("no single source"));
        // Review r6: a package name can start with digits; "|" keeps them apart from the depth.
        let tree = format!("0|3probe v0.1.0 (C:\\x\\probe)\n1|{}\n1|3fm_feature_sdk v1.0.0\n12|0fm_feature_sdk v1.0.0\n", line("v1", &SHA[..8]));
        assert_eq!(linked_sdks(&tree, &meta).unwrap(), vec![Some(git)]);
    }

    // A repo cargo config that can replace a dependency or the target stops the build before cargo runs.
    #[test]
    fn a_cargo_config_cannot_swap_the_sdk() {
        for (config, key) in [("paths = [\"vendor/sdk\"]\n", "paths"), ("[source.vendored]\ndirectory = \"vendor\"\n", "source"), ("[patch.'https://github.com/x/y']\n", "patch"),
            ("[build]\ntarget = \"aarch64-pc-windows-msvc\"\n", "build.target")] {
            let t = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(t.path().join(".cargo")).unwrap();
            std::fs::write(t.path().join(".cargo/config.toml"), config).unwrap();
            let pinned = Pinned { channel: "unused".into(), cargo_home: t.path().into(), rustc: t.path().into() };
            let err = sdk_source(t.path(), &t.path().join("Cargo.toml"), None, &pinned).unwrap_err();
            assert!(err.contains(&format!("sets `{key}`")), "{key}: {err}");
        }
    }

    // cargo's resolution decides, not the nearest Cargo.lock (review L4 R1-B2): a workspace member
    // with an old lock that names the git SDK still builds against the path SDK of the root lock.
    // Only the SDK that the feature links counts (R2-B1): not the SDK of another member, and not
    // the optional SDK of a shared helper that only another member turns on (R3-B1).
    #[cfg(windows)]
    #[test]
    fn the_sdk_check_reads_cargo_resolution() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        let files = [
            ("Cargo.toml", "[workspace]\nmembers = [\"feature\", \"other\", \"helper\"]\nresolver = \"2\"\n".to_string()),
            ("other/Cargo.toml", "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nfm_feature_sdk = { path = \"../../sdk2\" }\nhelper = { path = \"../helper\", features = [\"x\"] }\n".into()),
            ("other/src/lib.rs", String::new()),
            ("helper/Cargo.toml", "[package]\nname = \"helper\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nfm_feature_sdk = { path = \"../../sdk2\", optional = true }\n\n[features]\nx = [\"dep:fm_feature_sdk\"]\n".into()),
            ("helper/src/lib.rs", String::new()),
            ("../sdk2/Cargo.toml", "[package]\nname = \"fm_feature_sdk\"\nversion = \"0.2.0\"\nedition = \"2021\"\n\n[workspace]\n".into()),
            ("../sdk2/src/lib.rs", String::new()),
            ("sdk/Cargo.toml", "[package]\nname = \"fm_feature_sdk\"\nversion = \"0.1.0\"\nedition = \"2021\"\n".into()),
            ("sdk/src/lib.rs", String::new()),
            ("feature/Cargo.toml", "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nfm_feature_sdk = { path = \"../sdk\" }\nhelper = { path = \"../helper\" }\n".into()),
            ("feature/src/lib.rs", String::new()),
            ("feature/Cargo.lock", format!("version = 4\n\n[[package]]\nname = \"fm_feature_sdk\"\nversion = \"0.1.0\"\nsource = \"git+https://github.com/{SDK_REPOSITORY}?tag=v1#{SHA}\"\n")),
        ];
        for (name, text) in &files {
            std::fs::create_dir_all(src.join(name).parent().unwrap()).unwrap();
            std::fs::write(src.join(name), text).unwrap();
        }
        let pinned = Pinned::find().unwrap();
        assert!(pinned_cargo(&pinned).args(["generate-lockfile", "--offline"]).current_dir(&src).status().unwrap().success());
        let manifest = src.join("feature/Cargo.toml");
        let err = sdk_source(&src, &manifest, Some("https://github.com/creator/feature"), &pinned).unwrap_err();
        assert!(err.contains("path dependency"), "{err}");
        assert_eq!(sdk_source(&src, &manifest, None, &pinned).unwrap(), serde_json::Value::Null);
        // The facts commands read no cargo config of the commit: with it, cargo stops on the
        // color and rustc on the -Z flag (review r1 B1).
        std::fs::create_dir_all(src.join(".cargo")).unwrap();
        std::fs::write(src.join(".cargo/config.toml"), "[term]\ncolor = \"neon\"\n\n[build]\nrustflags = [\"-Zfm-probe\"]\n").unwrap();
        assert_eq!(sdk_source(&src, &manifest, None, &pinned).unwrap(), serde_json::Value::Null);
        std::fs::remove_dir_all(src.join(".cargo")).unwrap();
        // The SDK only as a dev-dependency: the library does not link it.
        let dev = "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dev-dependencies]\nfm_feature_sdk = { path = \"../sdk\" }\n";
        std::fs::write(&manifest, dev).unwrap();
        assert!(pinned_cargo(&pinned).args(["generate-lockfile", "--offline"]).current_dir(&src).status().unwrap().success());
        assert!(sdk_source(&src, &manifest, None, &pinned).unwrap_err().contains("does not link"));
    }

    // The tag commit of an annotated tag is its peeled line; a lightweight tag is its own line.
    #[test]
    fn the_sdk_tag_resolves_to_its_commit() {
        let text = "1111111111111111111111111111111111111111\trefs/tags/v1\n2222222222222222222222222222222222222222\trefs/tags/v1^{}\n3333333333333333333333333333333333333333\trefs/tags/v10\n";
        assert_eq!(tag_commit(text, "v1").as_deref(), Some("2222222222222222222222222222222222222222"));
        assert_eq!(tag_commit(text, "v10").as_deref(), Some("3333333333333333333333333333333333333333"));
        assert_eq!(tag_commit(text, "v2"), None);
    }

    // A non-empty out folder or a bad feature dir stops the build before any git or cargo work.
    #[cfg(windows)]
    #[test]
    fn a_build_starts_in_a_clean_folder() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("old"), "x").unwrap();
        let out = t.path().to_string_lossy().into_owned();
        assert!(command("no-repo", "HEAD", ".", &out).unwrap_err().contains("not empty"));
        assert!(command("no-repo", "HEAD", "../x", &t.path().join("new").to_string_lossy()).unwrap_err().contains("feature dir"));
    }

    /// A DOS header with a Rich header of `rows` (product, build, count) under `key`, PE at 0x100.
    fn with_rich(rows: &[(u32, u32, u32)], key: u32) -> Vec<u8> {
        let mut b = vec![0u8; 0x104];
        b[..2].copy_from_slice(b"MZ");
        b[0x3c..0x40].copy_from_slice(&0x100u32.to_le_bytes());
        b[0x100..].copy_from_slice(b"PE\0\0");
        let mut words = vec![0x536e_6144 ^ key, key, key, key];
        for &(p, v, c) in rows { words.extend([((p << 16) | v) ^ key, c ^ key]); }
        let at = 0x80;
        for (i, w) in words.iter().enumerate() { b[at + i * 4..at + i * 4 + 4].copy_from_slice(&w.to_le_bytes()); }
        let end = at + words.len() * 4;
        b[end..end + 4].copy_from_slice(b"Rich");
        b[end + 4..end + 8].copy_from_slice(&key.to_le_bytes());
        b
    }

    // build.json names the linker build: two machines with the same MSVC folder but another
    // link.exe build gave two hashes (2026-10-27), and only the Rich header showed why.
    #[test]
    fn the_record_names_the_msvc_tools_of_each_library() {
        let rows = [(261, 33145, 137), (258, 35226, 1)];
        let tools = rich_tools(&with_rich(&rows, 0x29bd_7c30)).unwrap();
        assert_eq!(tools, ["261.33145 x 137", "258.35226 x 1"]);
        assert_ne!(tools, rich_tools(&with_rich(&[(261, 33145, 137), (258, 35229, 1)], 0x29bd_7c3c)).unwrap());
        let mut none = with_rich(&rows, 7);
        none[0x80..0x84].copy_from_slice(&[0; 4]);
        assert_eq!(rich_tools(&none), None, "no DanS start: no record, never a wrong one");
        assert_eq!(rich_tools(b"MZ"), None, "a short file");
    }

    // Review r1 (2026-10-27): a "Rich" word in the stub, a header that ends at PE, and broken headers.
    #[test]
    fn the_rich_parser_skips_false_marks_and_refuses_broken_headers() {
        let rows = [(261, 33145, 137), (258, 35226, 1)];
        let want = ["261.33145 x 137", "258.35226 x 1"];
        let mut stub = with_rich(&rows, 0x29bd_7c30);
        stub[0x50..0x54].copy_from_slice(b"Rich");
        assert_eq!(rich_tools(&stub).unwrap(), want, "a false mark before the header is skipped");
        let mut tight = with_rich(&rows, 0x29bd_7c30);
        tight[0x3c..0x40].copy_from_slice(&(0x80u32 + 32 + 8).to_le_bytes());
        tight[0xa8..0xac].copy_from_slice(b"PE\0\0");
        assert_eq!(rich_tools(&tight).unwrap(), want, "the key ends exactly at PE");
        for bad_pe in [0xffff_ffffu32, 0x104, 0x0f0] {
            let mut b = with_rich(&rows, 0x29bd_7c30);
            b[0x3c..0x40].copy_from_slice(&bad_pe.to_le_bytes());
            assert_eq!(rich_tools(&b), None, "PE offset {bad_pe:#x} has no PE signature");
        }
        let mut pad = with_rich(&rows, 0x29bd_7c30);
        pad[0x88..0x8c].copy_from_slice(&[1, 2, 3, 4]);
        assert_eq!(rich_tools(&pad), None, "a pad word that is not the key");
        assert_eq!(rich_tools(&with_rich(&[], 0x29bd_7c30)), None, "no rows");
        let mut half = with_rich(&rows, 0x29bd_7c30);
        half.copy_within(0xa0..0xa8, 0x9c);
        assert_eq!(rich_tools(&half), None, "a half row");
    }
}
