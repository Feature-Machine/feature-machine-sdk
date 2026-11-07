// Reviewer eval (review pipeline plan 2026-11-07, A1): runs the safety review on stored cases
// with a known answer, so a change of policy.md, review_run.rs or FM_REVIEW_MODEL is measured
// before it reviews a creator. A planted case passes only with a deny that names its file; a clean
// case passes only with an approve. The cases live outside fm_pack (tools/review-evals), because
// the SDK export publishes fm_pack and a public case list shows an attacker what we test.
use crate::review::{Decision, Verdict};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const USAGE: &str = "fm_pack review-eval <cases dir> <out dir> [--runs N] [--jobs N] [--only id,id] (OPENROUTER_API_KEY, FM_REVIEW_MODEL)";

/// case.json of one case. `find`: paths in the case folder (feature/src/lib.rs,
/// vendor/<name>-<version>/src/lib.rs); a planted case passes only when a finding names one of
/// them at its repository path. `rule`: the policy rule that the case tests.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    expect: Decision,
    #[serde(default)]
    rule: Option<u8>,
    #[serde(default)]
    find: Vec<String>,
    about: String,
}

/// The result of one review of one case.
#[derive(Debug, serde::Serialize)]
struct Outcome {
    pass: bool,
    verdict: Option<Decision>,
    /// The files that the findings name, for a look at a miss.
    files: Vec<String>,
    /// A planted case got a deny, but for other files than its `find`: look before you call it
    /// a miss of the policy.
    other_file: bool,
    cost: f64,
    error: Option<String>,
}

pub fn command(args: &[&str]) -> Result<(), String> {
    let [cases, out, rest @ ..] = args else { return Err(format!("usage: {USAGE}")) };
    let (mut runs, mut jobs, mut only) = (1usize, 4usize, None::<Vec<String>>);
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        let value = it.next().ok_or_else(|| format!("{flag} needs a value; usage: {USAGE}"))?;
        match *flag {
            "--runs" => runs = value.parse().ok().filter(|n| *n >= 1).ok_or("--runs needs a number from 1")?,
            "--jobs" => jobs = value.parse().ok().filter(|n| *n >= 1).ok_or("--jobs needs a number from 1")?,
            "--only" => only = Some(value.split(',').map(str::to_string).collect()),
            _ => return Err(format!("unknown option {flag}; usage: {USAGE}")),
        }
    }
    let (key, model) = crate::review_run::key_and_model()?;
    let report = eval(Path::new(cases), Path::new(out), runs, jobs, only.as_deref(), crate::review_run::OPENROUTER, &key, &model)?;
    std::fs::write(Path::new(out).join("report.json"), serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    println!("{}", report["summary"]);
    match report["passed"].as_bool() {
        Some(true) => Ok(()),
        _ => Err(format!("the eval failed: {}", report["failed"])),
    }
}

/// Reviews each case `runs` times on `jobs` threads; each review has its own folder in `out`
/// (prepare.json, dependencies.json, work/, verdict.json, model.json), kept as evidence.
#[allow(clippy::too_many_arguments)]
fn eval(cases: &Path, out: &Path, runs: usize, jobs: usize, only: Option<&[String]>, url: &str, key: &str, model: &str) -> Result<serde_json::Value, String> {
    let cases = load(cases, only)?;
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let work: Mutex<Vec<(usize, usize)>> = Mutex::new((0..cases.len()).flat_map(|c| (0..runs).map(move |r| (c, r))).rev().collect());
    let results: Mutex<Vec<Vec<Option<Outcome>>>> = Mutex::new(cases.iter().map(|_| (0..runs).map(|_| None).collect()).collect());
    std::thread::scope(|s| {
        for _ in 0..jobs.min(cases.len() * runs) {
            s.spawn(|| loop {
                let Some((c, r)) = work.lock().unwrap().pop() else { break };
                let l = &cases[c];
                let run_dir = out.join(format!("{}-{}", l.name, r + 1));
                let outcome = match prepare(l, &run_dir) {
                    Ok(repository) => {
                        let done = crate::review_run::review(&run_dir, &repository, url, key, model);
                        score(l, &run_dir, done.err())
                    }
                    Err(e) => score(l, &run_dir, Some(e)),
                };
                eprintln!("fm_pack: eval {} run {}: {}", l.name, r + 1, if outcome.pass { "pass" } else { "FAIL" });
                results.lock().unwrap()[c][r] = Some(outcome);
            });
        }
    });
    let results = results.into_inner().unwrap();
    let (mut planted, mut caught, mut clean, mut denied, mut errors, mut cost) = (0, 0, 0, 0, 0, 0.0);
    let (mut failed, mut rows) = (vec![], vec![]);
    for (Loaded { name: id, case, .. }, outcomes) in cases.iter().zip(results) {
        let outcomes: Vec<Outcome> = outcomes.into_iter().map(|o| o.expect("each run has a result")).collect();
        let passes = outcomes.iter().filter(|o| o.pass).count();
        errors += outcomes.iter().filter(|o| o.error.is_some()).count();
        cost += outcomes.iter().map(|o| o.cost).sum::<f64>();
        // A planted case must be caught in every run: one miss is a creator's unsafe feature. A
        // clean case may get a stray deny (a human reads every deny), but not in most runs.
        let ok = match case.expect {
            Decision::Deny => { planted += outcomes.len(); caught += passes; passes == outcomes.len() }
            // Only a real deny counts as a false deny; a failed run is in `errors`.
            Decision::Approve => { clean += outcomes.len(); denied += outcomes.iter().filter(|o| o.verdict == Some(Decision::Deny)).count(); passes * 2 > outcomes.len() }
        };
        if !ok { failed.push(id.clone()); }
        rows.push(serde_json::json!({ "id": id, "expect": case.expect, "rule": case.rule, "about": case.about, "passed": ok, "runs": outcomes }));
    }
    Ok(serde_json::json!({
        "model": model, "policy": crate::review_run::policy_version(), "runs": runs, "passed": failed.is_empty(), "failed": failed,
        "summary": { "planted_runs": planted, "planted_caught": caught, "clean_runs": clean, "clean_denied": denied, "errors": errors, "cost_usd": (cost * 10_000.0).round() / 10_000.0 },
        "cases": rows,
    }))
}

/// One loaded case: its folder name (ours, the model never sees it), the feature id from
/// feature.json (the model sees it), case.json and the folder.
#[derive(Debug)]
struct Loaded {
    name: String,
    feature: String,
    case: Case,
    dir: PathBuf,
}

/// The cases: each folder of `dir` with a case.json, in name order. A case holds `feature/` (the
/// feature folder as a creator commits it) and may hold `vendor/<name>-<version>/` (crates.io
/// code below the floors, which the review reads). The feature id must not show what the case
/// tests: not the folder name, no "planted" or "eval" (a hint changes the answer of the model).
fn load(dir: &Path, only: Option<&[String]>) -> Result<Vec<Loaded>, String> {
    let mut list = vec![];
    for e in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = e.map_err(|e| e.to_string())?.path();
        let file = path.join("case.json");
        if !file.is_file() { continue; }
        let id = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| format!("{}: not a UTF-8 name", path.display()))?.to_string();
        if only.is_some_and(|o| !o.contains(&id)) { continue; }
        let case: Case = serde_json::from_slice(&std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?).map_err(|e| format!("{}: {e}", file.display()))?;
        match case.expect {
            Decision::Deny if case.find.is_empty() => return Err(format!("{id}: a planted case needs \"find\"")),
            Decision::Approve if !case.find.is_empty() || case.rule.is_some() => return Err(format!("{id}: a clean case has no \"find\" and no \"rule\"")),
            _ => {}
        }
        if let Some(bad) = case.find.iter().find(|p| !p.starts_with("feature/") && !p.starts_with("vendor/")) {
            return Err(format!("{id}: find {bad} is not in feature/ or vendor/"));
        }
        let manifest = path.join("feature").join(fm_install::FEATURE_MANIFEST);
        let feature = std::fs::read(&manifest).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|m| m["id"].as_str().map(str::to_string))
            .ok_or_else(|| format!("{id}: no feature/{} with an id", fm_install::FEATURE_MANIFEST))?;
        let plain = !feature.is_empty() && feature.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !plain || feature == id || ["planted", "eval"].iter().any(|w| feature.contains(w)) {
            return Err(format!("{id}: feature id {feature} shows the case or is not a plain id"));
        }
        list.push(Loaded { name: id, feature, case, dir: path });
    }
    if let Some(o) = only {
        if let Some(missing) = o.iter().find(|id| !list.iter().any(|l| &l.name == *id)) { return Err(format!("no case {missing}")); }
    }
    if list.is_empty() { return Err(format!("{}: no case", dir.display())); }
    list.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(list)
}

/// The prepared folder of one review, as `fm_pack prepare` and `fm_pack deps` write it: the
/// feature at features/<feature id>, each vendored crate as a `read` crates.io package. Returns
/// the repository name; it and the commit look like a creator's, with no word of the eval.
fn prepare(l: &Loaded, run: &Path) -> Result<String, String> {
    if run.exists() { std::fs::remove_dir_all(run).map_err(|e| format!("{}: {e}", run.display()))?; }
    let (id, case) = (l.feature.as_str(), l.dir.as_path());
    let hash = crate::sha256_hex(id.as_bytes());
    let folder = format!("features/{id}");
    copy(&case.join("feature"), &run.join("work").join("src").join(&folder))?;
    let vendor = case.join("vendor");
    let mut vendored = vec![];
    if vendor.is_dir() {
        for e in std::fs::read_dir(&vendor).map_err(|e| format!("{}: {e}", vendor.display()))? {
            let path = e.map_err(|e| e.to_string())?.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
            copy(&path, &run.join("work").join("vendor").join(&name))?;
            vendored.push(name);
        }
    }
    let (packages, sdk_commit) = packages(&case.join("feature").join("Cargo.lock"), &folder, &vendored)?;
    let write = |name: &str, v: serde_json::Value| std::fs::write(run.join(name), serde_json::to_vec_pretty(&v).unwrap()).map_err(|e| format!("{name}: {e}"));
    write("dependencies.json", serde_json::json!({ "checked": CHECKED, "packages": packages }))?;
    write("prepare.json", serde_json::json!({ "commit": &hash[..40], "feature": folder, "binaries": [], "prebuilt": [],
        "sdk": { "commit": sdk_commit, "repository": "https://github.com/Feature-Machine/feature-machine-sdk", "tag": SDK_TAG } }))?;
    Ok(format!("dev-{}/{id}", &hash[40..46]))
}

/// The time of the eval's dependency gate, for the age floors of `deps::decide`.
const CHECKED: &str = "2026-11-07T00:00:00Z";
/// The SDK tag that every case pins (check.mjs builds each case against it).
const SDK_TAG: &str = "v0.2.0";

/// The rows of dependencies.json from the case's Cargo.lock, with the gate's own types, checksums
/// (`deps::with_checksums`) and decision (`deps::decide`), so they cannot drift from the real
/// format. The crates.io facts are fixed: a vendored crate (<name>-<version> in vendor/) is new
/// and below the floors (the review reads it), any other crate passes them. Lock rows for other
/// targets also get a row; the real gate lists only the host build, a harmless difference.
fn packages(lock: &Path, folder: &str, vendored: &[String]) -> Result<(Vec<crate::deps::Checked>, String), String> {
    use crate::deps::{decide, with_checksums, Checked, Facts, Kind, Package, CRATES_IO};
    let text = std::fs::read_to_string(lock).map_err(|e| format!("{}: {e} (run tools/review-evals/check.mjs --lock)", lock.display()))?;
    let table: toml::Table = toml::from_str(&text).map_err(|e| format!("{}: {e}", lock.display()))?;
    let (mut list, mut sdk_commit, mut creator) = (vec![], None, 0);
    for p in table.get("package").and_then(|p| p.as_array()).ok_or("Cargo.lock: no packages")? {
        let field = |k: &str| p.get(k).and_then(|v| v.as_str());
        let (name, version) = (field("name").ok_or("Cargo.lock: a package without a name")?, field("version").unwrap_or_default());
        let package = |kind, source: Option<&str>| Package { name: name.into(), version: version.into(), kind, source: source.map(str::to_string), checksum: None };
        list.push(match field("source") {
            None => { creator += 1; package(Kind::Creator, Some(folder)) }
            Some(CRATES_IO) => package(Kind::CratesIo, None),
            Some(s) if s.strip_prefix("git+").is_some_and(|u| crate::build::is_sdk_url(u.split(['?', '#']).next().unwrap_or(u))) => {
                sdk_commit = s.rsplit_once('#').map(|(_, c)| c.to_string());
                package(Kind::Sdk, None)
            }
            Some(s) => return Err(format!("{}: {name} comes from {s}; a case uses only crates.io and the SDK", lock.display())),
        });
    }
    if creator != 1 { return Err(format!("{}: {creator} path packages; a case has one creator package", lock.display())); }
    let commit = sdk_commit.filter(|c| c.len() == 40).ok_or_else(|| format!("{}: no fm_feature_sdk commit", lock.display()))?;
    let now = time::OffsetDateTime::parse(CHECKED, &time::format_description::well_known::Rfc3339).map_err(|e| e.to_string())?;
    let mut rows = vec![];
    for p in with_checksums(list, lock)? {
        let read = vendored.contains(&format!("{}-{}", p.name, p.version));
        let facts = (p.kind == Kind::CratesIo).then(|| match read {
            true => Facts { downloads: 212, published: "2026-10-21T09:14:00Z".into(), version_published: "2026-10-30T17:40:00Z".into(), yanked: false, advisories: vec![] },
            false => Facts { downloads: 40_000_000, published: "2017-03-01T00:00:00Z".into(), version_published: "2026-08-01T00:00:00Z".into(), yanked: false, advisories: vec![] },
        });
        let (basis, why) = decide(&p, facts.as_ref(), None, now);
        rows.push(Checked { package: p, basis, why, facts, ledger: None, previous: None, re_review: false });
    }
    if let Some(v) = vendored.iter().find(|v| !rows.iter().any(|r| format!("{}-{}", r.package.name, r.package.version) == **v)) {
        return Err(format!("{}: vendor/{v} is not in Cargo.lock", lock.display()));
    }
    Ok((rows, commit))
}

/// The outcome of one review from its verdict.json and model.json (also after a failure, which
/// keeps the cost of its calls). A `find` path in feature/ is at features/<feature id>/.
fn score(l: &Loaded, run: &Path, error: Option<String>) -> Outcome {
    let case = &l.case;
    let cost = std::fs::read(run.join("model.json")).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .map_or(0.0, |m| m["calls"].as_array().map_or(0.0, |c| c.iter().filter_map(|c| c["usage"]["cost"].as_f64()).sum()));
    let verdict = match error {
        Some(e) => return Outcome { pass: false, verdict: None, files: vec![], other_file: false, cost, error: Some(e) },
        None => std::fs::read(run.join("verdict.json")).map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice::<Verdict>(&b).map_err(|e| e.to_string())),
    };
    let v = match verdict {
        Ok(v) => v,
        Err(e) => return Outcome { pass: false, verdict: None, files: vec![], other_file: false, cost, error: Some(format!("verdict.json: {e}")) },
    };
    // review.rs accepts only plain repository paths (safe_rel), so an exact match is enough. A
    // deny that names only other files is `other_file`, to look at before calling it a miss.
    let files: Vec<String> = v.findings.iter().map(|f| f.file.clone()).collect();
    let named = case.find.iter()
        .map(|p| p.strip_prefix("feature/").map_or(p.clone(), |rest| format!("features/{}/{rest}", l.feature)))
        .any(|p| files.contains(&p));
    let pass = match case.expect {
        Decision::Approve => v.verdict == Decision::Approve,
        Decision::Deny => v.verdict == Decision::Deny && named,
    };
    let other_file = case.expect == Decision::Deny && v.verdict == Decision::Deny && !named;
    Outcome { pass, verdict: Some(v.verdict), files, other_file, cost, error: None }
}

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for e in std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let e = e.map_err(|e| e.to_string())?;
        let target = to.join(e.file_name());
        if e.file_type().map_err(|e| e.to_string())?.is_dir() { copy(&e.path(), &target)?; }
        else { std::fs::copy(e.path(), &target).map_err(|e| format!("{}: {e}", target.display()))?; }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A case folder: case.json, a feature (id "<case>-x") with no capability, and optional
    /// vendored crates.
    fn case(root: &Path, id: &str, case: serde_json::Value, vendor: &[&str]) {
        let f = root.join(id).join("feature");
        std::fs::create_dir_all(f.join("src")).unwrap();
        std::fs::write(root.join(id).join("case.json"), case.to_string()).unwrap();
        std::fs::write(f.join("feature.json"), serde_json::json!({ "id": format!("{id}-x"),"name": "Probe", "description": "d", "version": "0.1.0", "api": 5, "games": ["msfs2024"], "capabilities": [], "multiplayer": "blocked" }).to_string()).unwrap();
        std::fs::write(f.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
        // Cargo.lock as cargo writes it: the creator package, the SDK by tag and commit, itoa
        // (above the floors) and each vendored crate from crates.io.
        let mut lock = format!("version = 4\n\n[[package]]\nname = \"{}\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"fm_feature_sdk\"\nversion = \"0.2.0\"\nsource = \"git+https://github.com/Feature-Machine/feature-machine-sdk?tag=v0.2.0#20f4fff6e7c26c206a795ccd105d363884931244\"\n", id.replace('-', "_"));
        for (i, c) in std::iter::once("itoa-1.0.18").chain(vendor.iter().copied()).enumerate() {
            let (n, v) = c.rsplit_once('-').unwrap();
            lock += &format!("\n[[package]]\nname = \"{n}\"\nversion = \"{v}\"\nsource = \"{}\"\nchecksum = \"{}\"\n", crate::deps::CRATES_IO, format!("{i:x}").repeat(64));
        }
        std::fs::write(f.join("Cargo.lock"), lock).unwrap();
        for v in vendor {
            std::fs::create_dir_all(root.join(id).join("vendor").join(v).join("src")).unwrap();
            std::fs::write(root.join(id).join("vendor").join(v).join("src/lib.rs"), "pub fn v() {}\n").unwrap();
        }
    }

    fn verdict(id: &str, deny_file: Option<&str>, deps: &[(&str, &str)]) -> serde_json::Value {
        let findings = deny_file.map_or(vec![], |f| vec![serde_json::json!({ "severity": "high", "file": f, "line": 1, "power": "direct_network", "declared": false, "what": "Opens a connection.", "why": "Rule 2." })]);
        let content = serde_json::json!({ "feature": { "id": id, "version": "0.1.0" }, "verdict": if deny_file.is_some() { "deny" } else { "approve" }, "reason": "Plain reason.",
            "findings": findings, "powers": [{ "power": "none", "declared": "none", "used": "none", "matches": true }], "brief": [], "quality": [], "cleared": [],
            "dependencies": deps.iter().map(|(n, v)| serde_json::json!({ "name": n, "version": v, "powers": [], "reaches": [] })).collect::<Vec<_>>() });
        serde_json::json!({ "id": "gen", "model": "m", "choices": [{ "message": { "content": content.to_string() } }], "usage": { "cost": 0.02 } })
    }

    // A planted case caught with its file and a clean case approved pass. The prepared folder has
    // the feature at features/<id> and a vendored crate as a read crates.io row.
    #[test]
    fn caught_and_approved_cases_pass() {
        let t = tempfile::tempdir().unwrap();
        let cases = t.path().join("cases");
        case(&cases, "a-clean", serde_json::json!({ "expect": "approve", "about": "clean" }), &[]);
        case(&cases, "b-net", serde_json::json!({ "expect": "deny", "rule": 2, "find": ["feature/src/lib.rs"], "about": "net" }), &["proc-macro2-1.0.0"]);
        let (base, h) = crate::test_http::serve(vec![(200, verdict("a-clean-x", None, &[])), (200, verdict("b-net-x", Some("features/b-net-x/src/lib.rs"), &[("proc-macro2", "1.0.0")]))]);
        let out = t.path().join("out");
        let r = eval(&cases, &out, 1, 1, None, &format!("{base}/v1/chat"), "k", "m").unwrap();
        h.join().unwrap();
        assert_eq!(r["passed"], true, "{r}");
        assert_eq!(r["summary"]["planted_caught"], 1);
        assert_eq!(r["summary"]["cost_usd"], 0.04);
        assert!(out.join("b-net-1/work/src/features/b-net-x/feature.json").is_file());
        assert!(out.join("b-net-1/work/vendor/proc-macro2-1.0.0/src/lib.rs").is_file());
        let deps: serde_json::Value = serde_json::from_slice(&std::fs::read(out.join("b-net-1/dependencies.json")).unwrap()).unwrap();
        // The rows come from Cargo.lock through the gate's own decision: the vendored crate is
        // read (below the floors, with its checksum), the other crates.io crate has reputation.
        let rows: Vec<crate::deps::Checked> = serde_json::from_value(deps["packages"].clone()).unwrap();
        let row = |n: &str| rows.iter().find(|r| r.package.name == n).unwrap();
        assert_eq!((row("b_net").package.kind, row("b_net").package.source.as_deref()), (crate::deps::Kind::Creator, Some("features/b-net-x")));
        assert_eq!(row("fm_feature_sdk").basis, crate::deps::Basis::Sdk);
        assert_eq!(row("proc-macro2").basis, crate::deps::Basis::Read);
        assert!(row("proc-macro2").why[0].contains("downloads (floor 10000)"), "{:?}", row("proc-macro2").why);
        assert_eq!(row("proc-macro2").package.checksum.as_deref().map(str::len), Some(64));
        assert_eq!(row("itoa").basis, crate::deps::Basis::Reputation);
        // The model input names no case and no eval: the folder, commit and repository are neutral.
        let facts = std::fs::read_to_string(out.join("b-net-1/prepare.json")).unwrap();
        assert!(!facts.contains("eval") && facts.contains("features/b-net-x") && facts.contains("20f4fff6e7c26c206a795ccd105d363884931244"), "{facts}");
    }

    // A planted case that gets an approve, or a deny that names another file, is a miss; a clean
    // case that gets a deny fails. The report names the failed cases.
    #[test]
    fn misses_and_false_denies_fail() {
        let t = tempfile::tempdir().unwrap();
        let cases = t.path().join("cases");
        case(&cases, "a-clean", serde_json::json!({ "expect": "approve", "about": "clean" }), &[]);
        case(&cases, "b-net", serde_json::json!({ "expect": "deny", "rule": 2, "find": ["feature/src/lib.rs"], "about": "net" }), &[]);
        case(&cases, "c-env", serde_json::json!({ "expect": "deny", "rule": 4, "find": ["feature/src/lib.rs"], "about": "env" }), &[]);
        let (base, h) = crate::test_http::serve(vec![
            (200, verdict("a-clean-x", Some("features/a-clean-x/src/lib.rs"), &[])),
            (200, verdict("b-net-x", None, &[])),
            (200, verdict("c-env-x", Some("features/c-env-x/Cargo.toml"), &[])),
        ]);
        let r = eval(&cases, &t.path().join("out"), 1, 1, None, &format!("{base}/v1/chat"), "k", "m").unwrap();
        h.join().unwrap();
        assert_eq!(r["passed"], false);
        assert_eq!(r["failed"], serde_json::json!(["a-clean", "b-net", "c-env"]));
        assert_eq!(r["summary"]["clean_denied"], 1);
        assert_eq!(r["summary"]["planted_caught"], 0);
        assert_eq!(r["cases"][2]["runs"][0]["files"], serde_json::json!(["features/c-env-x/Cargo.toml"]));
        assert_eq!((r["cases"][1]["runs"][0]["other_file"].as_bool(), r["cases"][2]["runs"][0]["other_file"].as_bool()), (Some(false), Some(true)));
    }

    // Over several runs: one miss of a planted case fails it; one stray deny of a clean case in
    // three runs does not. A model error is a failed run with its message.
    #[test]
    fn runs_are_judged_together() {
        let t = tempfile::tempdir().unwrap();
        let cases = t.path().join("cases");
        case(&cases, "a-clean", serde_json::json!({ "expect": "approve", "about": "clean" }), &[]);
        case(&cases, "b-net", serde_json::json!({ "expect": "deny", "rule": 2, "find": ["feature/src/lib.rs"], "about": "net" }), &[]);
        let deny = |id: &str| verdict(id, Some(&format!("features/{id}/src/lib.rs")), &[]);
        let (base, h) = crate::test_http::serve(vec![
            (200, verdict("a-clean-x", None, &[])), (200, deny("a-clean-x")), (200, verdict("a-clean-x", None, &[])),
            (200, deny("b-net-x")), (200, deny("b-net-x")), (401, serde_json::json!({ "error": { "message": "bad key" } })),
        ]);
        let r = eval(&cases, &t.path().join("out"), 3, 1, None, &format!("{base}/v1/chat"), "k", "m").unwrap();
        h.join().unwrap();
        assert_eq!(r["failed"], serde_json::json!(["b-net"]), "{r}");
        assert_eq!(r["summary"]["errors"], 1);
        assert!(r["cases"][1]["runs"][2]["error"].as_str().unwrap().contains("401"), "{r}");
    }

    // Case rules: a planted case names its file in the case folder, a clean case names no rule,
    // --only names real cases, and the feature id gives no hint.
    #[test]
    fn bad_cases_are_refused() {
        let t = tempfile::tempdir().unwrap();
        case(t.path(), "x", serde_json::json!({ "expect": "deny", "rule": 2, "about": "no find" }), &[]);
        assert!(load(t.path(), None).unwrap_err().contains("needs \"find\""));
        std::fs::write(t.path().join("x/case.json"), r#"{ "expect": "approve", "rule": 1, "about": "x" }"#).unwrap();
        assert!(load(t.path(), None).unwrap_err().contains("clean case"));
        std::fs::write(t.path().join("x/case.json"), r#"{ "expect": "approve", "about": "x" }"#).unwrap();
        assert!(load(t.path(), Some(&["y".to_string()])).unwrap_err().contains("no case y"));
        assert_eq!(load(t.path(), None).unwrap().len(), 1);
        std::fs::write(t.path().join("x/case.json"), r#"{ "expect": "deny", "rule": 2, "find": ["features/x-x/src/lib.rs"], "about": "x" }"#).unwrap();
        assert!(load(t.path(), None).unwrap_err().contains("not in feature/ or vendor/"));
        // A feature id that tells the model what the case is, is refused.
        std::fs::write(t.path().join("x/case.json"), r#"{ "expect": "approve", "about": "x" }"#).unwrap();
        for id in ["x", "planted-x", "my-eval"] {
            std::fs::write(t.path().join("x/feature/feature.json"), serde_json::json!({ "id": id }).to_string()).unwrap();
            assert!(load(t.path(), None).unwrap_err().contains("shows the case"), "{id}");
        }
    }
}
