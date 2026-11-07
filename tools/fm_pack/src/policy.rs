// Game policies on a feed (plan 2026-11-04-game-policies, slice 1). The rules are
// fm_install::policy (the app reads the same file). `policy-set` replaces the whole rule set from
// a source file, raises the revision and deletes the old signature, so the feed never serves a
// signature of other bytes; sign it again (app/scripts/sign-feature.mjs) before upload.
use fm_install::module::Power;
use fm_install::policy::{Policies, Rule, FEED, FEED_SIG, KIND};
use fm_install::feature::Capability;
use std::path::Path;

pub const USAGE: &str = "fm_pack policy-set <feed> <rules.json> | fm_pack policy-verify <feed>";

pub fn command(args: &[&str]) -> Result<(), String> {
    match args {
        ["policy-set", feed, source] => set(feed, source, now()),
        ["policy-verify", feed] => verify(feed, crate::feed::key()),
        _ => Err(format!("usage: {USAGE}")),
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The feed's policy file, or the built-in base (revision 0) when the feed has none yet. Any
/// other read error stops before a write.
fn read(feed: &str) -> Result<Policies, String> {
    let file = Path::new(feed).join(FEED);
    match std::fs::read(&file) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Policies::baseline()),
        Err(e) => Err(format!("{}: {e}", file.display())),
    }
}

/// A rule that the app would apply as written: known power names only (a typo would deny
/// nothing), a reason for the card, and a unique id (the app keys seen notices by it).
fn check(rules: &[Rule]) -> Result<(), String> {
    let known = |p: &str| {
        let v = serde_json::Value::String(p.into());
        serde_json::from_value::<Capability>(v.clone()).is_ok_and(|c| c != Capability::Unknown) || serde_json::from_value::<Power>(v).is_ok_and(|m| m != Power::Unknown)
    };
    let mut ids = std::collections::BTreeSet::new();
    for r in rules {
        if !ids.insert(r.id.as_str()) {
            return Err(format!("rule {}: the id is used twice", r.id));
        }
        if r.reason.trim().is_empty() {
            return Err(format!("rule {}: a rule needs a reason (the card shows it)", r.id));
        }
        if let Some(p) = r.deny.iter().find(|p| !known(p)) {
            return Err(format!("rule {}: unknown power {p}", r.id));
        }
    }
    Ok(())
}

/// Replaces the rule set with `source` (a JSON list of rules). A rule with no `since` keeps the
/// time of the rule with its id in the feed, or gets `now`. Nothing is written on an error.
fn set(feed: &str, source: &str, now: u64) -> Result<(), String> {
    let old = read(feed)?;
    let text = std::fs::read(source).map_err(|e| format!("{source}: {e}"))?;
    let list: Vec<serde_json::Value> = serde_json::from_slice(&text).map_err(|e| format!("{source}: {e}"))?;
    let mut rules = Vec::new();
    for mut v in list {
        let id = v.get("id").and_then(|i| i.as_str()).unwrap_or_default().to_string();
        if v.get("since").is_none() {
            let since = old.rules.iter().find(|r| r.id == id).map_or(now, |r| r.since);
            v["since"] = since.into();
        }
        rules.push(serde_json::from_value::<Rule>(v).map_err(|e| format!("rule {id}: {e}"))?);
    }
    check(&rules)?;
    let p = Policies { schema: 1, kind: KIND.into(), revision: old.revision + 1, issued: now, rules, skipped: 0 };
    let path = Path::new(feed).join(FEED);
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new(feed))).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(Path::new(feed).join(FEED_SIG));
    std::fs::write(&path, serde_json::to_vec_pretty(&p).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    println!("policies: revision {}, {} rules (sign it)", p.revision, p.rules.len());
    Ok(())
}

/// Checks the feed's policy file with `key` (the app check). The publish job runs it before an
/// upload, so a policy file that the key did not sign never goes out.
fn verify(feed: &str, key: &str) -> Result<(), String> {
    let (file, sig) = (Path::new(feed).join(FEED), Path::new(feed).join(FEED_SIG));
    let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let signature = std::fs::read_to_string(&sig).map_err(|e| format!("{}: {e}", sig.display()))?;
    let p = Policies::verify(&bytes, signature.trim(), key)?;
    if p.skipped > 0 {
        return Err(format!("policies: {} rules that this tool cannot read", p.skipped));
    }
    println!("policies: revision {} signed by the feed key", p.revision);
    Ok(())
}

/// For `fm_pack inspect` (sign-feature.mjs): a policy file signs for its revision.
pub fn inspect(bytes: &[u8]) -> Option<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    if v.get("kind")?.as_str()? != KIND {
        return None;
    }
    let revision = v.get("revision")?.as_u64()?;
    Some(serde_json::json!({ "id": "policies", "version": revision.to_string(), "kind": KIND }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_install::test_signing::TestKey;

    fn source(dir: &Path, rules: serde_json::Value) -> String {
        let p = dir.join("rules.json");
        std::fs::write(&p, serde_json::to_vec(&rules).unwrap()).unwrap();
        p.to_string_lossy().into_owned()
    }

    // The signing path: a set raises the revision from the base, keeps `since` of a kept rule,
    // deletes the stale signature; a bad rule writes nothing; only the key's signature verifies.
    #[test]
    fn set_and_verify_a_signed_policy_file() {
        let key = TestKey::default();
        let t = tempfile::tempdir().unwrap();
        let feed = t.path().join("feed").to_string_lossy().into_owned();
        let deny_writes = serde_json::json!([
            { "id": "ats-truckersmp-gameplay", "game": "ats", "when": { "multiplayer": ["truckersmp"] }, "deny": ["input_inject", "game_command"], "reason": "TruckersMP rule." },
            { "id": "msfs-no-writes", "game": "msfs2024", "when": "always", "deny": ["vars_write", "game_memory_write"], "apply": "next_start", "reason": "Asked by the game developer." },
        ]);
        set(&feed, &source(t.path(), deny_writes), 100).unwrap();
        let file = Path::new(&feed).join(FEED);
        let p: Policies = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!((p.revision, p.issued, p.rules[0].since, p.rules[1].since), (1, 100, 0, 100), "the base rule keeps its time");
        std::fs::write(Path::new(&feed).join(FEED_SIG), "old").unwrap();
        set(&feed, &source(t.path(), serde_json::json!([])), 200).unwrap();
        assert!(!Path::new(&feed).join(FEED_SIG).exists(), "the stale signature goes");
        let bytes = std::fs::read(&file).unwrap();
        assert_eq!(serde_json::from_slice::<Policies>(&bytes).unwrap().revision, 2, "an empty set lifts every rule");
        for bad in [
            serde_json::json!([{ "id": "a", "game": "ats", "when": "always", "deny": ["vars_wrte"], "reason": "r" }]),
            serde_json::json!([{ "id": "a", "game": "ats", "when": "always", "reason": " " }]),
            serde_json::json!([{ "id": "a", "game": "ats", "when": "always", "reason": "r" }, { "id": "a", "game": "ats", "when": "always", "reason": "r" }]),
            serde_json::json!([{ "id": "a", "game": "ats", "when": "sometimes", "reason": "r" }]),
        ] {
            assert!(set(&feed, &source(t.path(), bad), 300).is_err());
            assert_eq!(std::fs::read(&file).unwrap(), bytes, "nothing written");
        }
        assert!(verify(&feed, &key.public()).unwrap_err().contains("policies.json.sig"));
        std::fs::write(Path::new(&feed).join(FEED_SIG), key.sign(&bytes, "2")).unwrap();
        verify(&feed, &key.public()).unwrap();
        std::fs::write(Path::new(&feed).join(FEED_SIG), TestKey::default().sign(&bytes, "2")).unwrap();
        assert!(verify(&feed, &key.public()).is_err(), "another key");
        assert_eq!(inspect(&bytes).unwrap(), serde_json::json!({ "id": "policies", "version": "2", "kind": "policies" }));
    }
}
