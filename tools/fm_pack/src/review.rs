// Review verdicts (publishing pipeline plan, L3). The review job asks the model with
// review/policy.md and review/verdict.schema.json (made from `Verdict`; a test keeps them equal),
// then runs `fm_pack review-brief <build dir> <verdict.json>`. A verdict that passes the schema
// type and the checks against the built package becomes the public safety brief (approve) or the
// creator's review notes (deny), with the dependency decisions of `fm_pack deps` (deps.rs).
use fm_install::feature::{Capability, Manifest};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The longest text of one field: a brief line is one or two sentences.
const MAX_TEXT: usize = 600;

/// The answer of the safety reviewer (review/policy.md, "Answer").
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Verdict {
    /// The id and version in the reviewed feature.json.
    pub feature: FeatureRef,
    /// approve when there is no safety finding, else deny.
    pub verdict: Decision,
    /// One or two plain sentences: why.
    pub reason: String,
    /// Safety findings only, most severe first. Empty for approve.
    pub findings: Vec<Finding>,
    /// One row for each power that the feature declares or uses.
    pub powers: Vec<PowerUse>,
    /// One line for each capability in feature.json, for the member.
    pub brief: Vec<BriefLine>,
    /// Short notes for the admin, not for members or the creator; never a reason to deny.
    pub quality: Vec<String>,
    /// Risks that the reviewer checked and found safe.
    pub cleared: Vec<String>,
    /// One row for each dependency that the review read (not creator code): the powers of its
    /// own code. The dependency ledger keeps them (plan S2 step 2), so a later feature that uses
    /// the same code is judged by them without a new read.
    pub dependencies: Vec<DependencyUse>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DependencyUse {
    /// The name and version of a `read` package of dependencies.json.
    pub name: String,
    pub version: String,
    /// The capabilities of feature.json that its code needs (for example "direct_network");
    /// empty when it needs none.
    pub powers: Vec<String>,
    /// The hosts, programs, registry keys and files that its code reaches; empty when none.
    pub reaches: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FeatureRef {
    pub id: String,
    pub version: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Decision { Approve, Deny }

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Severity { High, Medium, Low }

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub severity: Severity,
    /// Relative to the repository root, with "/" separators.
    pub file: String,
    /// The first line of the problem.
    #[schemars(schema_with = "line_schema")]
    pub line: u32,
    /// The power that the code uses, for example "direct file access".
    pub power: String,
    /// Whether feature.json declares that power.
    pub declared: bool,
    pub what: String,
    pub why: String,
}

/// A plain integer from 1: structured output refuses the "uint32" format that schemars writes.
fn line_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({ "type": "integer", "minimum": 1 })
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PowerUse {
    pub power: String,
    /// What feature.json declares for it ("none" when nothing).
    pub declared: String,
    /// What the shipped code uses ("none" when nothing).
    pub used: String,
    /// True when the use stays inside the declaration.
    pub matches: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BriefLine {
    /// A capability of feature.json, named exactly as there (for example "vars_read").
    pub capability: String,
    /// What the feature does with it, in plain words.
    pub text: String,
}

impl Verdict {
    /// The checks that the schema cannot express, against the manifest of the built package.
    /// `machine` holds the folders of this machine (`machine_roots`) that no text may name.
    /// Every problem is listed, so the job can send them back to the reviewer in one step.
    pub fn check(&self, m: &Manifest, machine: &[String]) -> Result<(), String> {
        let mut bad = Vec::new();
        let private = |t: &str| {
            let n = normal(t);
            if machine.iter().any(|r| names_root(&n, r)) { Some("a folder of this machine") } else { not_public(t) }
        };
        if (self.feature.id.as_str(), self.feature.version.as_str()) != (m.id.as_str(), m.version.as_str()) {
            bad.push(format!("the verdict is for {} {}, the package is {} {}", self.feature.id, self.feature.version, m.id, m.version));
        }
        let texts = std::iter::once(&self.reason)
            .chain(self.findings.iter().flat_map(|f| [&f.file, &f.power, &f.what, &f.why]))
            .chain(self.powers.iter().flat_map(|p| [&p.power, &p.declared, &p.used]))
            .chain(self.brief.iter().flat_map(|b| [&b.capability, &b.text]))
            .chain(&self.quality).chain(&self.cleared)
            .chain(self.dependencies.iter().flat_map(|d| [&d.name, &d.version].into_iter().chain(&d.powers).chain(&d.reaches)));
        for t in texts {
            let start = t.chars().take(60).collect::<String>();
            if t.trim().is_empty() || t.chars().count() > MAX_TEXT || t.chars().any(char::is_control) {
                bad.push(format!("a text is empty, longer than {MAX_TEXT} characters or has control characters: {start:?}"));
            }
            if let Some(why) = private(t) { bad.push(format!("a text has {why}: {start:?}")); }
        }
        // The creator's name of the feature is public in the brief too (review R3-B2).
        if let Some(why) = private(&m.name) { bad.push(format!("the feature name in feature.json has {why}")); }
        match self.verdict {
            Decision::Approve if !self.findings.is_empty() => bad.push("approve with safety findings".into()),
            Decision::Approve if self.powers.iter().any(|p| !p.matches) => bad.push("approve with a power used outside its declaration".into()),
            Decision::Deny if self.findings.is_empty() => bad.push("deny without a safety finding".into()),
            _ => {}
        }
        if self.powers.is_empty() { bad.push("no powers table".into()); }
        for f in &self.findings {
            if fm_install::feature::safe_rel(&f.file).is_err() { bad.push(format!("finding file {:?} is not relative to the repository", f.file)); }
            if f.line == 0 { bad.push(format!("finding in {}: line 0 (lines start at 1)", f.file)); }
        }
        let mut seen = Vec::new();
        for b in &self.brief {
            match serde_json::from_value::<Capability>(serde_json::Value::String(b.capability.clone())) {
                Ok(c) if seen.contains(&c) => bad.push(format!("brief: {} twice", b.capability)),
                Ok(c) if m.capabilities.contains(&c) => seen.push(c),
                _ => bad.push(format!("brief: {} is not a capability of feature.json", b.capability)),
            }
        }
        if self.verdict == Decision::Approve {
            for c in m.capabilities.iter().filter(|c| !seen.contains(c)) {
                bad.push(format!("brief: no line for {}", serde_json::to_value(c).map(|v| v.to_string()).unwrap_or_default()));
            }
        }
        for d in &self.dependencies {
            for p in &d.powers {
                if matches!(serde_json::from_value::<Capability>(serde_json::Value::String(p.clone())), Err(_) | Ok(Capability::Unknown)) { bad.push(format!("dependencies: {} {}: {p} is not a capability name", d.name, d.version)); }
            }
        }
        match bad.is_empty() {
            true => Ok(()),
            false => Err(bad.join("; ")),
        }
    }

    /// The dependency rows against dependencies.json: exactly one row for each package that the
    /// review read and that is not creator code, and no other row. The ledger keys its entries by
    /// these packages, so a missing or invented row would record wrong powers (plan S2 step 2).
    pub fn check_dependencies(&self, deps: &crate::deps::Dependencies) -> Result<(), String> {
        use crate::deps::{Basis, Kind};
        let want: Vec<(&str, &str)> = deps.packages.iter().filter(|c| c.basis == Basis::Read && c.package.kind != Kind::Creator)
            .map(|c| (c.package.name.as_str(), c.package.version.as_str())).collect();
        let mut bad = Vec::new();
        for (n, v) in &want {
            match self.dependencies.iter().filter(|d| (d.name.as_str(), d.version.as_str()) == (*n, *v)).count() {
                1 => {}
                0 => bad.push(format!("dependencies: no row for {n} {v}")),
                _ => bad.push(format!("dependencies: {n} {v} twice")),
            }
        }
        for d in self.dependencies.iter().filter(|d| !want.contains(&(d.name.as_str(), d.version.as_str()))) {
            bad.push(format!("dependencies: {} {} is not a read dependency of dependencies.json", d.name, d.version));
        }
        match bad.is_empty() {
            true => Ok(()),
            false => Err(bad.join("; ")),
        }
    }

    /// The public safety brief (approve) or the admin review notes (deny), in Markdown. Only
    /// the title is Markdown; all model and manifest text is in one fenced text block, which
    /// GitHub and CommonMark show as written: no link (also no autolink of a URL, host or email),
    /// no HTML, no formatting. The commit and SHA-256 come from build.json; `supply` holds the
    /// lines of `supply_lines` (what the review read, what reputation accepted).
    pub fn render(&self, m: &Manifest, commit: &str, sha256: &str, supply: &[String]) -> String {
        let mut b = Vec::new();
        let list = |b: &mut Vec<String>, title: &str, items: &[String]| {
            if items.is_empty() { return; }
            b.push(String::new());
            b.push(format!("{title}:"));
            b.extend(items.iter().map(|i| format!("- {i}")));
        };
        let title = match self.verdict {
            Decision::Approve => {
                b.push(format!("{} {}: approved. {}", m.name, m.version, self.reason));
                b.push(format!("Built from commit {commit}. Package SHA-256 {sha256}."));
                b.push(String::new());
                b.push("What it does with each power:".into());
                b.extend(self.brief.iter().map(|l| format!("- {}: {}", l.capability, l.text)));
                "Safety brief"
            }
            Decision::Deny => {
                b.push(format!("{} {}: denied. {}", m.name, m.version, self.reason));
                b.push(format!("Commit {commit}."));
                b.push(String::new());
                b.push("Safety findings:".into());
                for f in &self.findings {
                    let declared = if f.declared { "declared" } else { "not declared" };
                    b.push(format!("- {:?}, {}:{}, {} ({declared}): {} {}", f.severity, f.file, f.line, f.power, f.what, f.why));
                }
                b.push(String::new());
                b.push("Powers:".into());
                for p in &self.powers {
                    let ok = if p.matches { "inside the declaration" } else { "outside the declaration" };
                    b.push(format!("- {}: declared {}; used {}; {ok}", p.power, p.declared, p.used));
                }
                "Review notes"
            }
        };
        if !supply.is_empty() {
            b.push(String::new());
            b.extend(supply.iter().cloned());
        }
        // Quality notes and cleared risks are a free code review, so only the admin notes of a
        // deny hold them; the public brief does not (creator feedback plan F1, 2026-11-02).
        if self.verdict == Decision::Deny {
            list(&mut b, "Quality notes", &self.quality);
            list(&mut b, "Cleared risks", &self.cleared);
        }
        fenced(title, &b)
    }
}

/// "# <title>" and the lines in one fenced text block. Control characters become spaces, so a
/// text cannot start a line; the fence is longer than any run of backticks in the text, so no
/// text can close it (CommonMark: a closing fence is at least as long as the opening one).
fn fenced(title: &str, lines: &[String]) -> String {
    let body: Vec<String> = lines.iter().map(|l| l.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()).collect();
    let longest = body.iter().flat_map(|l| l.split(|c| c != '`')).map(str::len).max().unwrap_or(0);
    let fence = "`".repeat((longest + 1).max(3));
    format!("# {title}\n\n{fence}text\n{}\n{fence}\n", body.join("\n"))
}

/// The folders of this machine that a public text must not name: home, temp, the current
/// folder, the runner and the cargo folders. This exact check is the guarantee against the
/// paths of the machine that runs the review; the `not_public` patterns also catch others.
pub(crate) fn machine_roots() -> Vec<String> {
    let vars = ["USERPROFILE", "HOME", "TEMP", "TMP", "TMPDIR", "RUNNER_TEMP", "RUNNER_TOOL_CACHE", "GITHUB_WORKSPACE", "CARGO_HOME", "RUSTUP_HOME"];
    roots_of(vars.iter().filter_map(std::env::var_os).map(std::path::PathBuf::from).chain(std::env::current_dir().ok()))
}

/// Each folder in the `normal` form, also without its drive and without its first "/" when two
/// or more names stay ("C:\Users\r" gives "c:/users/r", "/users/r" and "users/r"; "/root" does
/// not give "root", which is a normal word).
fn roots_of(dirs: impl Iterator<Item = std::path::PathBuf>) -> Vec<String> {
    let mut roots: Vec<String> = Vec::new();
    for d in dirs {
        let full = normal(&d.to_string_lossy()).trim_end_matches('/').to_string();
        let bare = full.get(2..).filter(|_| full.as_bytes().get(1) == Some(&b':')).unwrap_or(&full).to_string();
        let names = bare.trim_start_matches('/').to_string();
        let mut forms = vec![full.clone(), bare.clone()];
        if names.contains('/') { forms.push(names); }
        for r in forms { if r.len() >= 4 && !roots.contains(&r) { roots.push(r); } }
    }
    roots
}

/// A text in the form that `machine_roots` uses: lowercase, "\" as "/", and one "/" for a run
/// of them ("///home//r" is "/home/r" to the system).
fn normal(t: &str) -> String {
    let mut n = String::with_capacity(t.len());
    for c in t.chars().map(|c| if c == '\\' { '/' } else { c.to_ascii_lowercase() }) {
        if !(c == '/' && n.ends_with('/')) { n.push(c); }
    }
    n
}

/// Whether the normal text `n` names the root `r` as a whole folder: no name goes on before it
/// or after it, so "/root" is not in "src/root.rs" or "/rooted", but is in "/root/x" and "/root.".
fn names_root(n: &str, r: &str) -> bool {
    n.match_indices(r).any(|(i, _)| {
        let before = n[..i].chars().last().is_none_or(|b| !(b.is_alphanumeric() || "._-".contains(b)));
        let mut rest = n[i + r.len()..].chars();
        let after = match (rest.next(), rest.next()) {
            (None, _) | (Some('/'), _) => true,
            (Some('.'), next) => !next.is_some_and(char::is_alphanumeric),
            (Some(a), _) => !(a.is_alphanumeric() || "_-".contains(a)),
        };
        before && after
    })
}

/// Why a public text cannot be public: a local path shows the reviewer's or creator's PC. A path
/// is absolute when its run starts with slashes and then a name; a run starts after a sign that
/// is not a letter, a digit or "._-~/\", and "*" and "_" go with the sign before them ("**/x**"
/// starts a run, "a/*/x" not). An escape ("\n", "\0", "\x00", "\u{..}") and "http(s)://" are not
/// paths. Also a drive ("C:\"), "~/" or "file:/" after a sign that is not a letter.
fn not_public(t: &str) -> Option<&'static str> {
    let mut c: Vec<char> = t.chars().map(|ch| ch.to_ascii_lowercase()).collect();
    for i in 0..c.len() {
        for scheme in ["http://", "https://"] {
            if c[i..].iter().copied().take(scheme.len()).eq(scheme.chars()) { c[i + scheme.len() - 3..i + scheme.len()].fill('x'); }
        }
    }
    let at = |i: usize| c.get(i).copied().unwrap_or(' ');
    let slash = |ch: char| matches!(ch, '/' | '\\');
    let name = |ch: char| ch.is_alphanumeric() || "._?$%~-".contains(ch);
    let local = (0..c.len()).any(|i| {
        let p = if i == 0 { ' ' } else { c[i - 1] };
        let stars = c[..i].iter().rev().take_while(|ch| matches!(ch, '*' | '_')).count();
        let q = if i == stars { ' ' } else { c[i - stars - 1] };
        let run_start = !(q.is_alphanumeric() || "._-~/\\".contains(q));
        let escape = c[i] == '\\' && ((at(i + 1).is_ascii_alphanumeric() && !name(at(i + 2)))
            || (at(i + 1) == 'x' && at(i + 2).is_ascii_hexdigit() && at(i + 3).is_ascii_hexdigit() && !name(at(i + 4)))
            || (at(i + 1) == 'u' && at(i + 2) == '{'));
        let first_name = c[i..].iter().copied().find(|ch| !slash(*ch)).unwrap_or(' ');
        (run_start && slash(c[i]) && !escape && name(first_name))
            || !p.is_alphanumeric() && (
                (c[i].is_ascii_alphabetic() && at(i + 1) == ':' && slash(at(i + 2)))
                || (c[i] == '~' && slash(at(i + 1)))
                || (c[i..].starts_with(&['f', 'i', 'l', 'e', ':']) && slash(at(i + 5))))
    });
    local.then_some("a local path")
}

/// What the brief says about the code of the build (submission review plan R1 to R8): the code
/// that the review read, the code that an earlier approved review read (with its recorded
/// powers, S2 step 5), the SDK, the crates.io crates that reputation accepted, and each
/// closed-source part with its publisher. All facts come from build.json and dependencies.json.
fn supply_lines(deps: &crate::deps::Dependencies, sdk: &serde_json::Value, binaries: &[crate::deps::Binary], prebuilt: &[crate::deps::Prebuilt]) -> Vec<String> {
    use crate::deps::{Basis, Kind};
    let mut b = vec!["Code that the review read:".to_string()];
    for c in deps.packages.iter().filter(|c| c.basis == Basis::Read) {
        let p = &c.package;
        let again = if c.re_review { "; Feature Machine asked for a new full read" } else { "" };
        b.push(match p.kind {
            Kind::Creator => format!("- {} {} in {}", p.name, p.version, p.source.as_deref().unwrap_or(".")),
            Kind::Git => format!("- {} {} from {}", p.name, p.version, p.source.as_deref().unwrap_or("git")),
            _ => match &c.previous {
                Some(e) => format!("- {} {} from crates.io: {}; the changes since {} (an earlier approved review read {})", p.name, p.version, c.why.join("; "), e.version, e.version),
                None => format!("- {} {} from crates.io: {}", p.name, p.version, c.why.join("; ")),
            },
        } + again);
    }
    let earlier: Vec<String> = deps.packages.iter().filter(|c| c.basis == Basis::Reviewed).map(|c| {
        let p = &c.package;
        let from = if p.kind == Kind::Git { p.source.as_deref().unwrap_or("git") } else { "crates.io" };
        let powers = c.ledger.as_ref().map(|e| e.powers.join(", ")).filter(|s| !s.is_empty()).unwrap_or_else(|| "no power".into());
        format!("- {} {} from {from}: {powers}", p.name, p.version)
    }).collect();
    if !earlier.is_empty() {
        b.push("Code that an earlier approved review read, with the powers that its code needs:".into());
        b.extend(earlier);
    }
    b.push(match sdk["tag"].as_str() {
        Some(tag) => format!("Feature Machine SDK: {tag}."),
        None => "Feature Machine SDK: the local SDK of this repository.".into(),
    });
    let known: Vec<String> = deps.packages.iter().filter(|c| c.basis == Basis::Reputation).map(|c| format!("{} {}", c.package.name, c.package.version)).collect();
    match known.is_empty() {
        true => b.push("Open-source crates accepted by crates.io reputation: none.".into()),
        false => b.push(format!("Open-source crates accepted by crates.io reputation ({}): {}.", known.len(), known.join(", "))),
    }
    let closed: Vec<String> = binaries.iter().map(|x| format!("- {} {} from {} (SHA-256 {})", x.file, x.version, x.publisher, x.sha256))
        .chain(prebuilt.iter().map(|x| {
            // Why it ships (R19): the reputation of its crate, else an approval (prebuilt_check).
            let known = deps.packages.iter().any(|c| c.basis == Basis::Reputation && c.package.kind == Kind::CratesIo && c.package.name == x.name && c.package.version == x.version);
            format!("- {} in {} (SHA-256 {}), {}", x.file, x.publisher(), x.sha256, if known { "with the reputation of its crate" } else { "approved by Feature Machine" })
        })).collect();
    match closed.is_empty() {
        true => b.push("Closed-source parts: none.".into()),
        false => b.extend(std::iter::once("Closed-source parts:".to_string()).chain(closed)),
    }
    b
}

/// The decisions of dependencies.json for the packages of build.json: the same list, each
/// decision equal to `deps::decide` on its stored facts and date (a changed file shows), and
/// none refused.
fn checked_deps(dir: &Path, record: &serde_json::Value) -> Result<crate::deps::Dependencies, String> {
    let file = dir.join("dependencies.json");
    let deps: crate::deps::Dependencies = serde_json::from_slice(&std::fs::read(&file).map_err(|e| format!("{}: {e} (run fm_pack deps first)", file.display()))?).map_err(|e| format!("dependencies.json: {e}"))?;
    let packages: Vec<crate::deps::Package> = serde_json::from_value(record["packages"].clone()).map_err(|e| format!("build.json packages: {e}"))?;
    if deps.packages.iter().map(|c| &c.package).ne(packages.iter()) { return Err("dependencies.json is not for the packages of build.json; run fm_pack deps again".into()); }
    let at = time::OffsetDateTime::parse(&deps.checked, &time::format_description::well_known::Rfc3339).map_err(|e| format!("dependencies.json checked: {e}"))?;
    for c in &deps.packages {
        if crate::deps::decide(&c.package, c.facts.as_ref(), c.ledger.as_ref(), at) != (c.basis, c.why.clone()) || (c.ledger.is_some() && c.basis != crate::deps::Basis::Reviewed) { return Err(format!("dependencies.json: the decision for {} {} does not follow from its facts", c.package.name, c.package.version)); }
        if c.basis == crate::deps::Basis::Refused { return Err(format!("dependencies.json refuses {} {}: {}", c.package.name, c.package.version, c.why.join("; "))); }
        if c.re_review && (c.previous.is_some() || c.ledger.is_some()) { return Err(format!("dependencies.json: {} {} is marked for a new read but has a ledger base", c.package.name, c.package.version)); }
        if let Some(e) = &c.previous {
            if crate::deps::diff_base(&c.package, c.facts.as_ref(), c.basis, std::slice::from_ref(e)) != Some(e) { return Err(format!("dependencies.json: {} {} cannot have a diff read from {}", c.package.name, c.package.version, e.version)); }
        }
    }
    Ok(deps)
}

/// fm_pack review-brief <build dir> <verdict.json>: the package must still be the one in
/// build.json (SHA-256), dependencies.json must hold its decisions (`checked_deps`), and the
/// verdict must pass `check` for its manifest. Writes safety-brief.md or review-notes.md next
/// to build.json and prints the verdict and the file. With prepare.json (the review job), the
/// facts come from it, and build.json, which a runner with creator code wrote, must repeat them.
pub fn command(dir: &str, verdict: &str) -> Result<(), String> {
    let dir = Path::new(dir);
    let read = |p: &Path| std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()));
    let built: serde_json::Value = serde_json::from_slice(&read(&dir.join("build.json"))?).map_err(|e| format!("build.json: {e}"))?;
    let record = crate::build::facts_record(dir)?;
    if let Some(key) = crate::build::FACTS.into_iter().find(|k| built[*k] != record[*k]) {
        return Err(format!("build.json does not have the {key} of {}", crate::build::PREPARED));
    }
    let hex = |v: &serde_json::Value| v.as_str().filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())).map(String::from);
    let (commit, sha256) = (hex(&record["commit"]).ok_or("no commit in the facts")?, hex(&built["package"]["sha256"]).ok_or("build.json: no package sha256")?);
    let file = built["package"]["file"].as_str().filter(|f| fm_install::feature::safe_rel(f).is_ok()).ok_or("build.json: no package file")?;
    let bytes = read(&dir.join(file))?;
    if crate::sha256_hex(&bytes) != sha256 { return Err(format!("{file} is not the package of build.json (SHA-256)")); }
    let (m, _) = fm_install::feature::inspect(std::io::Cursor::new(&bytes)).map_err(|e| e.to_string())?;
    // The consent of the member and the verdict are for the feature.json of the commit; build code
    // can change the tree before the pack (review r1 B5).
    if serde_json::to_value(&m).map_err(|e| e.to_string())? != record["manifest"] { return Err(format!("the feature.json of {file} is not the feature.json of the commit")); }
    let v: Verdict = serde_json::from_slice(&read(Path::new(verdict))?).map_err(|e| format!("{verdict}: {e}"))?;
    let machine = machine_roots();
    v.check(&m, &machine).map_err(|e| format!("{verdict}: {e}"))?;
    let deps = checked_deps(dir, &record)?;
    v.check_dependencies(&deps).map_err(|e| format!("{verdict}: {e}"))?;
    let binaries: Vec<crate::deps::Binary> = serde_json::from_value(record["binaries"].clone()).map_err(|e| format!("build.json binaries: {e}"))?;
    let prebuilt: Vec<crate::deps::Prebuilt> = serde_json::from_value(record["prebuilt"].clone()).map_err(|e| format!("build.json prebuilt: {e}"))?;
    crate::deps::prebuilt_check(&deps.packages, &prebuilt)?;
    let supply = supply_lines(&deps, &record["sdk"], &binaries, &prebuilt);
    // Folder names of the creator's repository are public in these lines too (as the name).
    for line in &supply {
        if machine.iter().any(|r| names_root(&normal(line), r)) || not_public(line).is_some() { return Err(format!("a dependency line names a local path: {line:?}")); }
    }
    let (name, other) = match v.verdict { Decision::Approve => ("safety-brief.md", "review-notes.md"), Decision::Deny => ("review-notes.md", "safety-brief.md") };
    // The newest verdict is the only result: a deny after an approve must not leave the public brief.
    match std::fs::remove_file(dir.join(other)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("{other}: {e}")),
        _ => {}
    }
    std::fs::write(dir.join(name), v.render(&m, &commit, &sha256, &supply)).map_err(|e| e.to_string())?;
    println!("{}", serde_json::json!({ "verdict": v.verdict, "file": dir.join(name) }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const EXAMPLES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/review/examples");

    /// The answer format for the model, as review/verdict.schema.json holds it.
    /// Subschemas inline: OpenAI strict mode refuses a "$ref" with other keys next to it, such
    /// as the field description (live OpenRouter call, 2026-11-01).
    fn schema() -> serde_json::Value {
        let generator = schemars::generate::SchemaSettings::draft2020_12().with(|s| s.inline_subschemas = true).into_generator();
        serde_json::to_value(generator.into_root_schema_for::<Verdict>()).expect("a schema is JSON")
    }

    /// The strict structured output rules: no "$ref", and each object lists every property as
    /// required and allows no other property.
    fn strict_problems(v: &serde_json::Value, at: &str, bad: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(o) => {
                if o.contains_key("$ref") { bad.push(format!("{at}: $ref")); }
                if let Some(props) = o.get("properties").and_then(|p| p.as_object()) {
                    let required: Vec<&str> = o.get("required").and_then(|r| r.as_array()).into_iter().flatten().filter_map(|r| r.as_str()).collect();
                    if props.keys().any(|k| !required.contains(&k.as_str())) { bad.push(format!("{at}: a property is not required")); }
                    if o.get("additionalProperties") != Some(&serde_json::Value::Bool(false)) { bad.push(format!("{at}: additionalProperties is not false")); }
                }
                for (k, x) in o { strict_problems(x, &format!("{at}/{k}"), bad); }
            }
            serde_json::Value::Array(a) => for (i, x) in a.iter().enumerate() { strict_problems(x, &format!("{at}/{i}"), bad); },
            _ => {}
        }
    }

    fn example(name: &str) -> (Verdict, Manifest) {
        let dir = Path::new(EXAMPLES).join(name);
        let v = serde_json::from_slice(&std::fs::read(dir.join("verdict.json")).unwrap()).unwrap();
        let m = serde_json::from_slice(&std::fs::read(dir.join("feature.json")).unwrap()).unwrap();
        (v, m)
    }

    // The schema file that the job gives the model is the Verdict type. To update it after a
    // change of the type: FM_UPDATE_SCHEMA=1 cargo test -p fm_pack review.
    #[test]
    fn the_schema_file_is_the_verdict_type() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("review/verdict.schema.json");
        let want = serde_json::to_string_pretty(&schema()).unwrap() + "\n";
        if std::env::var_os("FM_UPDATE_SCHEMA").is_some() { std::fs::write(&path, &want).unwrap(); }
        assert_eq!(std::fs::read_to_string(&path).unwrap().replace("\r\n", "\n"), want, "run FM_UPDATE_SCHEMA=1 cargo test -p fm_pack review");
        let mut bad = vec![];
        strict_problems(&schema(), "", &mut bad);
        assert!(bad.is_empty(), "the model call uses strict mode: {bad:?}");
    }

    // The three simulated reviews of 2026-10-24 (model runs 08:54, 08:54 and 09:39), written
    // as verdict data with their feature.json: trip 0.2.1 approve, live_weather 0.1.7 deny
    // (std::fs and process listing), live_weather 0.1.8 approve (user files service).
    #[test]
    fn the_simulated_verdicts_pass() {
        for (name, decision) in [("trip-0.2.1", Decision::Approve), ("live-weather-0.1.7", Decision::Deny), ("live-weather-0.1.8", Decision::Approve)] {
            let (v, m) = example(name);
            v.check(&m, &[]).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(v.verdict, decision, "{name}");
            let text = v.render(&m, "abc123", "def456", &[]);
            assert!(text.contains(if decision == Decision::Approve { "# Safety brief" } else { "# Review notes" }), "{name}");
        }
    }

    // Each rule fails on its own: a verdict that the job must send back to the reviewer.
    #[test]
    fn a_verdict_against_the_rules_is_refused() {
        type Break = fn(&mut Verdict, &mut Manifest);
        let cases: Vec<(&str, Break)> = vec![
            ("is for", |v, _| v.feature.version = "9.9.9".into()),
            ("approve with safety findings", |v, _| { let (d, _) = example("live-weather-0.1.7"); v.findings = d.findings; }),
            ("outside its declaration", |v, _| v.powers[0].matches = false),
            ("deny without a safety finding", |v, _| v.verdict = Decision::Deny),
            ("no line for", |v, _| { v.brief.pop(); }),
            ("is not a capability of feature.json", |v, _| v.brief.push(BriefLine { capability: "network".into(), text: "x".into() })),
            ("is not a capability of feature.json", |v, _| v.brief.push(BriefLine { capability: "files".into(), text: "x".into() })),
            ("twice", |v, _| { let b = BriefLine { capability: v.brief[0].capability.clone(), text: "x".into() }; v.brief.push(b); }),
            ("longer than", |v, _| v.reason = "x".repeat(MAX_TEXT + 1)),
            ("control characters", |v, _| v.quality.push("a\nb".into())),
            ("empty", |v, _| v.cleared.push(" ".into())),
            ("no powers table", |v, _| v.powers.clear()),
            ("the feature name in feature.json has a local path", |_, m| m.name = r"Trip C:\Users\reviewer\repo".into()),
        ];
        for (want, change) in cases {
            let (mut v, mut m) = example("trip-0.2.1");
            change(&mut v, &mut m);
            let err = v.check(&m, &[]).unwrap_err();
            assert!(err.contains(want), "{want}: {err}");
        }
        let (mut v, m) = example("live-weather-0.1.7");
        v.findings[0].file = "F:/repo/features/live_weather/src/profile.rs".into();
        assert!(v.check(&m, &[]).unwrap_err().contains("not relative"));
        let (mut v, m) = example("trip-0.2.1");
        v.dependencies.push(DependencyUse { name: "x".into(), version: "1.0.0".into(), powers: vec!["net_raw".into()], reaches: vec![] });
        assert!(v.check(&m, &[]).unwrap_err().contains("net_raw is not a capability name"));
        v.dependencies[0].powers = vec!["direct_network".into()];
        v.dependencies[0].reaches = vec!["/home/reviewer/x".into()];
        assert!(v.check(&m, &[]).unwrap_err().contains("a local path"));
    }

    // S2 step 5: a reviewed package needs no row (the ledger has its powers), the brief names it
    // with the recorded powers, and dependencies.json cannot keep an entry that did not decide.
    #[test]
    fn reviewed_packages_show_their_ledger_powers() {
        use crate::deps::{Basis, Checked, Dependencies, Entry, Kind, Package};
        let sum = "a".repeat(64);
        let p = Package { name: "small".into(), version: "1.0.0".into(), kind: Kind::CratesIo, source: None, checksum: Some(sum.clone()) };
        let entry = Entry { kind: Kind::CratesIo, name: "small".into(), version: "1.0.0".into(), source: sum, powers: vec!["direct_network".into()], reaches: vec!["api.example.com".into()], run: "940".into() };
        let deps = Dependencies { checked: "2026-11-01T00:00:00Z".into(), packages: vec![Checked { package: p, basis: Basis::Reviewed, why: vec![], facts: None, ledger: Some(entry), previous: None, re_review: false }] };
        let (mut v, _) = example("trip-0.2.1");
        v.dependencies = vec![];
        v.check_dependencies(&deps).unwrap();
        let lines = supply_lines(&deps, &serde_json::json!({ "tag": "sdk-v1" }), &[], &[]);
        assert!(lines.iter().any(|l| l == "- small 1.0.0 from crates.io: direct_network"), "{lines:?}");
        assert!(!lines.iter().any(|l| l.contains("small") && l.contains("reputation")), "{lines:?}");
    }

    // S2 step 5: review-brief recomputes each decision with its ledger entry: a reviewed package
    // passes, an entry on a package that its facts make `read` is refused, and so is an entry of
    // other code (a changed dependencies.json shows).
    #[test]
    fn checked_deps_holds_the_ledger_to_the_decision() {
        use crate::deps::{Basis, Checked, Dependencies, Entry, Facts, Kind, Package};
        let sum = "a".repeat(64);
        let p = Package { name: "small".into(), version: "1.0.0".into(), kind: Kind::CratesIo, source: None, checksum: Some(sum.clone()) };
        let entry = Entry { kind: Kind::CratesIo, name: "small".into(), version: "1.0.0".into(), source: sum, powers: vec![], reaches: vec![], run: "940".into() };
        let facts = Facts { downloads: 10, published: "2026-01-01T00:00:00Z".into(), version_published: "2026-01-01T00:00:00Z".into(), yanked: false, advisories: vec![] };
        let why = vec!["10 downloads (floor 10000)".to_string()];
        let record = serde_json::json!({ "packages": [p] });
        let t = tempfile::tempdir().unwrap();
        let try_marked = |basis, ledger: Option<Entry>, previous: Option<Entry>, re_review| {
            let deps = Dependencies { checked: "2026-11-01T00:00:00Z".into(), packages: vec![Checked { package: p.clone(), basis, why: why.clone(), facts: Some(facts.clone()), ledger, previous, re_review }] };
            std::fs::write(t.path().join("dependencies.json"), serde_json::to_vec(&deps).unwrap()).unwrap();
            checked_deps(t.path(), &record).map(|_| ())
        };
        let try_full = |basis, ledger, previous| try_marked(basis, ledger, previous, false);
        let try_with = |basis, ledger| try_full(basis, ledger, None);
        // Cloud review r2: code marked for a new read has no ledger base (R17).
        try_marked(Basis::Read, None, None, true).unwrap();
        assert!(try_marked(Basis::Read, None, Some(Entry { version: "0.9.0".into(), ..entry.clone() }), true).unwrap_err().contains("marked for a new read"));
        assert!(try_marked(Basis::Reviewed, Some(entry.clone()), None, true).unwrap_err().contains("marked for a new read"));
        // S2 step 6: a diff base only on a read crate, and only of an older version.
        let older = Entry { version: "0.9.0".into(), ..entry.clone() };
        try_full(Basis::Read, None, Some(older.clone())).unwrap();
        assert!(try_full(Basis::Read, None, Some(entry.clone())).unwrap_err().contains("diff read"));
        assert!(try_full(Basis::Reviewed, Some(entry.clone()), Some(older)).unwrap_err().contains("diff read"));
        try_with(Basis::Reviewed, Some(entry.clone())).unwrap();
        try_with(Basis::Read, None).unwrap();
        assert!(try_with(Basis::Read, Some(entry.clone())).unwrap_err().contains("does not follow"));
        assert!(try_with(Basis::Reviewed, None).unwrap_err().contains("does not follow"));
        assert!(try_with(Basis::Reviewed, Some(Entry { source: "b".repeat(64), ..entry })).unwrap_err().contains("does not follow"));
    }

    // S2 step 2: one row for each read dependency that is not creator code, and no other.
    #[test]
    fn the_dependency_rows_match_the_read_packages() {
        use crate::deps::{Basis, Checked, Dependencies, Kind, Package};
        let pkg = |name: &str, kind| Package { name: name.into(), version: "1.0.0".into(), kind, source: None, checksum: None };
        let checked = |p, basis| Checked { package: p, basis, why: vec![], facts: None, ledger: None, previous: None, re_review: false };
        let deps = Dependencies { checked: "2026-11-01T00:00:00Z".into(), packages: vec![
            checked(pkg("mine", Kind::Creator), Basis::Read), checked(pkg("small", Kind::CratesIo), Basis::Read),
            checked(pkg("gitdep", Kind::Git), Basis::Read), checked(pkg("serde", Kind::CratesIo), Basis::Reputation)] };
        let row = |n: &str| DependencyUse { name: n.into(), version: "1.0.0".into(), powers: vec![], reaches: vec![] };
        let (mut v, _) = example("trip-0.2.1");
        v.dependencies = vec![row("small"), row("gitdep")];
        v.check_dependencies(&deps).unwrap();
        v.dependencies = vec![row("small")];
        assert!(v.check_dependencies(&deps).unwrap_err().contains("no row for gitdep 1.0.0"));
        v.dependencies = vec![row("small"), row("gitdep"), row("small")];
        assert!(v.check_dependencies(&deps).unwrap_err().contains("small 1.0.0 twice"));
        for extra in ["serde", "mine", "invented"] {
            v.dependencies = vec![row("small"), row("gitdep"), row(extra)];
            assert!(v.check_dependencies(&deps).unwrap_err().contains(&format!("{extra} 1.0.0 is not a read dependency")), "{extra}");
        }
    }

    // A local path shows the reviewer's PC: such text in any field sends the verdict back, also
    // inside a word or a wrapper such as "file=/home/x", "</x>" or "**C:\x**", and in each root
    // form (review R2-B1, R3-B1, R4-B1). Relative paths, globs, "::", "File::open", versions, host
    // names, URLs and escapes such as '\r\n' pass (R2-B3, R4-B2, R5-B2).
    #[test]
    fn public_text_has_no_local_path() {
        let bad = ["read C:/Users/reviewer/repo/src/lib.rs", r"in C:\Users\r\lib.rs", r"on \\server\share\x", "at /home/r/repo/lib.rs",
            "at (/Users/r/x)", "in ~/repo", "file=/home/reviewer/repo/lib.rs", r"file=C:\Users\r\lib.rs", "path:/etc/passwd", "`/root/x`", "/var/x",
            "</home/reviewer/repo/lib.rs>", r"**C:\Users\reviewer\repo\lib.rs**", "file:///home/r/x", "FILE:/x", "//server/share/x", "<p>x</p>",
            r"\\?\UNC\reviewer-pc\share\repo\lib.rs", r"\\?\C:\x", "/./home/reviewer/repo/lib.rs", "/.private/reviewer/repo/lib.rs", "HTTPS:/x",
            "Checked **/home/reviewer/repo/src/lib.rs**", r"**\\reviewer-pc\share\repo**", "__/root/x__", r"\Users\reviewer\x", "**/config.cfg",
            "Checked ///home//runner/work/creator/src/lib.rs", r"\\\server\x"];
        for t in bad {
            assert!(not_public(t).is_some(), "{t}");
            let (mut v, m) = example("trip-0.2.1");
            v.cleared.push(t.into());
            assert!(v.check(&m, &[]).is_err(), "{t}");
        }
        for t in ["features/trip/src/lib.rs:97", "profiles/*/config.cfg", "LifecycleHost::game_running", "version 0.1.8 and/or 1/2",
            "tgftp.nws.noaa.gov", "www.example.com", "https://tgftp.nws.noaa.gov/data", "Note: a/b", ".ssh/id_rsa", "C:drive", "profile: x", r"journal.rs preserves '\r\n' and '\n' line endings", r"the '\\' separator", "my_mod/x.rs",
            r"The ABI symbol name ends with '\0' for C lookup.", r"'\x00' and '\u{0}'",
            "Uses std::fs::File::open in its own data folder.", "File: features/trip/src/lib.rs", "read / write", "ends with /", "../ui/index.html", "HTTP://Host/x"] {
            assert_eq!(not_public(t), None, "{t}");
        }
    }

    // A folder of the machine that runs the review is refused in any form, also where the
    // patterns see no path ("Users\reviewer\repo" has no root), and in the feature name.
    #[test]
    fn a_folder_of_this_machine_is_refused() {
        let roots = roots_of([r"C:\Users\reviewer", "/root", "/home/runner/work", "/tmp"].into_iter().map(std::path::PathBuf::from));
        assert_eq!(roots, ["c:/users/reviewer", "/users/reviewer", "users/reviewer", "/root", "/home/runner/work", "home/runner/work", "/tmp"]);
        let cwd = normal(&std::env::current_dir().unwrap().to_string_lossy());
        assert!(machine_roots().contains(&cwd.trim_end_matches('/').to_string()));
        // R6-B1: repeated separators; "/root." at the end of a sentence.
        for t in [r"Checked Users\reviewer\repo\lib.rs", "see home/runner/work/repo", "see home//runner///work/repo", "in USERS/Reviewer."] {
            assert_eq!(not_public(t), None, "{t}");
            let (mut v, m) = example("trip-0.2.1");
            v.cleared.push(t.into());
            assert!(v.check(&m, &roots).unwrap_err().contains("a folder of this machine"), "{t}");
        }
        let (v, mut m) = example("trip-0.2.1");
        v.check(&m, &roots).unwrap();
        m.name = "Trip users/reviewer".into();
        assert!(v.check(&m, &roots).unwrap_err().contains("feature name"));
        let (mut v, m) = example("trip-0.2.1");
        // R6-B3: a root is a whole folder; a file or a word with the same name is not it.
        v.cleared.extend(["root and runner work are words", "src/root.rs:12: the parser checks the data.",
            "features/trip/src/tmp.rs:12: the loop has a size limit.", "users/reviewers is another name"].map(String::from));
        v.check(&m, &roots).unwrap();
    }

    // The schema type refuses fields and values that it does not know.
    #[test]
    fn unknown_fields_and_values_are_refused() {
        let mut j: serde_json::Value = serde_json::from_slice(&std::fs::read(Path::new(EXAMPLES).join("trip-0.2.1/verdict.json")).unwrap()).unwrap();
        j["note"] = "x".into();
        assert!(serde_json::from_value::<Verdict>(j.clone()).is_err());
        j.as_object_mut().unwrap().remove("note");
        j["verdict"] = "maybe".into();
        assert!(serde_json::from_value::<Verdict>(j).is_err());
    }

    // Text from the model and the manifest (and so, maybe, from creator code) stays inside the
    // one fenced block: a newline or a run of backticks cannot end it (review R2-B2). Inside the
    // block, Markdown makes no link, HTML or heading (checked with micromark GFM, results.md).
    #[test]
    fn model_text_stays_in_the_fence() {
        let (mut v, mut m) = example("trip-0.2.1");
        m.name = "Trip\n# Hi ````` www.evil.example".into();
        v.brief[0].text = "Click [here](https://evil.example) <script>x</script> ```` help@evil.example".into();
        let text = v.render(&m, "abc", "def", &[]);
        let lines: Vec<&str> = text.lines().collect();
        let fence = "`".repeat(6) + "text";
        assert_eq!(lines[..3], ["# Safety brief", "", fence.as_str()], "{text}");
        assert_eq!(lines.last(), Some(&"``````"), "{text}");
        let inside = &lines[3..lines.len() - 1];
        assert!(inside.iter().all(|l| !l.trim_start().starts_with("``````")), "{text}");
        assert!(inside[0].starts_with("Trip # Hi ````` www.evil.example 0.2.1: approved."), "{text}");
        assert!(text.contains("Click [here](https://evil.example) <script>"), "the text is shown as written: {text}");
    }

    // review-brief on a real package folder: the brief names the commit and the SHA-256 of
    // build.json; a package that changed after the build, or a deny, gives the right file.
    #[test]
    fn review_brief_writes_the_brief_for_the_built_package() {
        let t = tempfile::tempdir().unwrap();
        let (v, m) = example("trip-0.2.1");
        let src = t.path().join("src");
        std::fs::create_dir_all(src.join("ui")).unwrap();
        std::fs::write(src.join(fm_install::FEATURE_MANIFEST), serde_json::to_vec(&m).unwrap()).unwrap();
        std::fs::write(src.join("ui/index.html"), "<p>trip</p>").unwrap();
        let lib = t.path().join(format!("feature.{}", std::env::consts::DLL_EXTENSION));
        std::fs::write(&lib, b"library").unwrap();
        let zip = t.path().join("trip-0.2.1.zip");
        fm_install::feature::pack(&src, &[lib], &mut std::fs::File::create(&zip).unwrap()).unwrap();
        let sha = crate::sha256_hex(&std::fs::read(&zip).unwrap());
        let packages = serde_json::json!([
            { "name": "fm_trip", "version": "0.2.1", "kind": "creator", "source": "features/trip" },
            { "name": "serde", "version": "1.0.229", "kind": "crates_io" },
        ]);
        let record = serde_json::json!({ "commit": "0123abcd", "package": { "file": "trip-0.2.1.zip", "sha256": sha }, "manifest": m, "sdk": null, "packages": packages, "binaries": [],
            "prebuilt": [{ "name": "serde", "version": "1.0.229", "file": "lib/x.lib", "sha256": "ab" }] });
        std::fs::write(t.path().join("build.json"), record.to_string()).unwrap();
        let verdict = t.path().join("verdict.json");
        std::fs::write(&verdict, serde_json::to_vec(&v).unwrap()).unwrap();
        let (d, vs) = (t.path().to_string_lossy().into_owned(), verdict.to_string_lossy().into_owned());
        // No dependencies.json: no brief (the gate comes first, submission review plan R3).
        assert!(command(&d, &vs).unwrap_err().contains("run fm_pack deps first"));
        let facts = serde_json::json!({ "downloads": 900_000_000u64, "published": "2015-01-01T00:00:00Z", "version_published": "2026-09-01T00:00:00Z", "yanked": false, "advisories": [] });
        let deps = |serde_basis: &str, why: serde_json::Value| serde_json::json!({ "checked": "2026-10-29T12:00:00Z", "packages": [
            { "package": packages[0], "basis": "read", "why": [] },
            { "package": packages[1], "basis": serde_basis, "why": why, "facts": facts },
        ] }).to_string();
        let deps_file = t.path().join("dependencies.json");
        std::fs::write(&deps_file, deps("read", serde_json::json!(["x"]))).unwrap();
        assert!(command(&d, &vs).unwrap_err().contains("does not follow from its facts"), "an edited decision");
        let gone = serde_json::json!({ "checked": "2026-10-29T12:00:00Z", "packages": [
            { "package": packages[0], "basis": "read", "why": [] },
            { "package": packages[1], "basis": "refused", "why": ["not on crates.io"] },
        ] });
        std::fs::write(&deps_file, gone.to_string()).unwrap();
        assert!(command(&d, &vs).unwrap_err().contains("refuses serde 1.0.229"));
        std::fs::write(&deps_file, deps("reputation", serde_json::json!([]))).unwrap();
        command(&d, &vs).unwrap();
        let brief = std::fs::read_to_string(t.path().join("safety-brief.md")).unwrap();
        assert!(brief.contains("commit 0123abcd.") && brief.contains(&format!("SHA-256 {sha}.")) && brief.contains("\n- vars_read: "), "{brief}");
        // The public brief is no code review: quality notes and cleared risks stay with the admin.
        assert!(!brief.contains("Quality notes:") && !brief.contains("Cleared risks:") && !brief.contains("lib.rs:97"), "{brief}");
        assert!(brief.contains("Code that the review read:\n- fm_trip 0.2.1 in features/trip\nFeature Machine SDK: the local SDK of this repository.\nOpen-source crates accepted by crates.io reputation (1): serde 1.0.229.\nClosed-source parts:\n- lib/x.lib in crates.io serde 1.0.229 (SHA-256 ab), with the reputation of its crate"), "{brief}");
        let (other, _) = example("live-weather-0.1.7");
        std::fs::write(&verdict, serde_json::to_vec(&other).unwrap()).unwrap();
        assert!(command(&d, &vs).unwrap_err().contains("the verdict is for live-weather"));
        let (mut deny, _) = example("trip-0.2.1");
        (deny.verdict, deny.findings) = (Decision::Deny, other.findings);
        std::fs::write(&verdict, serde_json::to_vec(&deny).unwrap()).unwrap();
        command(&d, &vs).unwrap();
        let notes = std::fs::read_to_string(t.path().join("review-notes.md")).unwrap();
        assert!(notes.contains("# Review notes\n\n```text\nTrip 0.2.1: denied."), "{notes}");
        assert!(notes.contains("\nQuality notes:\n- features/trip/src/lib.rs:97: ") && notes.contains("\nCleared risks:\n- "), "the admin notes keep them: {notes}");
        // The newest verdict is the only result, in both directions (review R1-B4).
        assert!(!t.path().join("safety-brief.md").exists(), "a deny left the public brief");
        std::fs::write(&verdict, serde_json::to_vec(&v).unwrap()).unwrap();
        command(&d, &vs).unwrap();
        assert!(t.path().join("safety-brief.md").exists() && !t.path().join("review-notes.md").exists());
        let mut changed = record.clone();
        changed["manifest"]["version"] = "9.9.9".into();
        std::fs::write(t.path().join("build.json"), changed.to_string()).unwrap();
        assert!(command(&d, &vs).unwrap_err().contains("is not the feature.json of the commit"));
        std::fs::write(t.path().join("build.json"), record.to_string()).unwrap();
        std::fs::write(&zip, b"other bytes").unwrap();
        std::fs::write(&verdict, serde_json::to_vec(&v).unwrap()).unwrap();
        assert!(command(&d, &vs).unwrap_err().contains("not the package of build.json"));
    }
}
