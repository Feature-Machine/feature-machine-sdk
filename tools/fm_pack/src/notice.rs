// Notices on a feed (plan 2026-11-04-game-policies, slice 3). The format is fm_install::notice
// (the app reads the same file). `notice-set` replaces the whole notice set from a source file,
// raises the revision and deletes the old signature, like `policy-set`; sign it again
// (app/scripts/sign-feature.mjs) before upload. An image is a feed file: the tool writes its
// SHA-256, so the app shows only those bytes.
use fm_install::notice::{limits, Notice, Notices, FEED, FEED_SIG, KIND};
use fm_install::policy::Policies;
use sha2::Digest;
use std::path::Path;

pub const USAGE: &str = "fm_pack notice-set <feed> <notices.json> | fm_pack notice-verify <feed>";

pub fn command(args: &[&str]) -> Result<(), String> {
    match args {
        ["notice-set", feed, source] => set(feed, source, now()),
        ["notice-verify", feed] => verify(feed, crate::feed::key()),
        _ => Err(format!("usage: {USAGE}")),
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The feed's notice file, or an empty set (revision 0) when the feed has none yet.
fn read(feed: &str) -> Result<Notices, String> {
    let file = Path::new(feed).join(FEED);
    match std::fs::read(&file) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Notices::default()),
        Err(e) => Err(format!("{}: {e}", file.display())),
    }
}

/// The policy rule ids of the feed (the built-in base when the feed has no policy file): a
/// notice may explain only rules that exist, so a typo does not leave a rule with no notice.
fn rule_ids(feed: &str) -> Result<Vec<String>, String> {
    let file = Path::new(feed).join(fm_install::policy::FEED);
    let p = match std::fs::read(&file) {
        Ok(b) => serde_json::from_slice::<Policies>(&b).map_err(|e| format!("{}: {e}", file.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Policies::baseline(),
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    Ok(p.rules.into_iter().map(|r| r.id).collect())
}

/// An image given as a feed path (a string) becomes the path and the SHA-256 of that file. An
/// image object (url and sha256) must be the image of the same notice in the feed: a kept notice
/// keeps its image file, and a new or changed image needs its file in the feed (Sol r2).
fn image(feed: &str, id: &str, v: &mut serde_json::Value, old: &Notices) -> Result<(), String> {
    if let Some(obj) = v.get("image").filter(|i| !i.is_string()) {
        let kept = old.notices.iter().find(|n| n.id == id).and_then(|n| n.image.as_ref());
        let given = serde_json::from_value::<fm_install::feature::ArtFile>(obj.clone()).ok();
        return if given.is_some() && given.as_ref() == kept { Ok(()) } else { Err(format!("notice {id}: an image object must be the image of this notice in the feed; give a new image as a feed path")) };
    }
    let Some(path) = v.get("image").and_then(|i| i.as_str()).map(str::to_string) else { return Ok(()) };
    let bytes = std::fs::read(Path::new(feed).join(&path)).map_err(|e| format!("notice {id}: image {path}: {e}"))?;
    if bytes.len() as u64 > limits::IMAGE {
        return Err(format!("notice {id}: the image is larger than {} bytes", limits::IMAGE));
    }
    let sha256: String = sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    v["image"] = serde_json::json!({ "url": path, "sha256": sha256 });
    Ok(())
}

/// Replaces the notice set with `source` (a JSON list of notices). A notice with no `published`
/// keeps the time of the notice with its id in the feed, or gets `now`. Nothing is written on
/// an error.
fn set(feed: &str, source: &str, now: u64) -> Result<(), String> {
    let old = read(feed)?;
    let rules = rule_ids(feed)?;
    let text = std::fs::read(source).map_err(|e| format!("{source}: {e}"))?;
    let list: Vec<serde_json::Value> = serde_json::from_slice(&text).map_err(|e| format!("{source}: {e}"))?;
    let mut notices: Vec<Notice> = Vec::new();
    for mut v in list {
        let id = v.get("id").and_then(|i| i.as_str()).unwrap_or_default().to_string();
        if v.get("published").is_none() {
            v["published"] = old.notices.iter().find(|n| n.id == id).map_or(now, |n| n.published).into();
        }
        image(feed, &id, &mut v, &old)?;
        let n = serde_json::from_value::<Notice>(v).map_err(|e| format!("notice {id}: {e}"))?;
        n.check()?;
        if notices.iter().any(|x| x.id == n.id) {
            return Err(format!("notice {id}: the id is used twice"));
        }
        // A rule that a change lifts leaves the feed, but the notice that announced it stays: a
        // kept notice may name the rules that it named before; any other name must exist.
        let before = old.notices.iter().find(|o| o.id == n.id);
        if let Some(r) = n.policies.iter().find(|r| !rules.contains(r) && !before.is_some_and(|o| o.policies.contains(r))) {
            return Err(format!("notice {id}: no policy rule {r} in the feed"));
        }
        notices.push(n);
    }
    let n = Notices { revision: old.revision + 1, issued: now, notices, ..Notices::default() };
    let path = Path::new(feed).join(FEED);
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new(feed))).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(Path::new(feed).join(FEED_SIG));
    std::fs::write(&path, serde_json::to_vec_pretty(&n).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    println!("notices: revision {}, {} notices (sign it)", n.revision, n.notices.len());
    Ok(())
}

/// Checks the feed's notice file with `key` (the app check), before an upload.
fn verify(feed: &str, key: &str) -> Result<(), String> {
    let (file, sig) = (Path::new(feed).join(FEED), Path::new(feed).join(FEED_SIG));
    let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let signature = std::fs::read_to_string(&sig).map_err(|e| format!("{}: {e}", sig.display()))?;
    let n = Notices::verify(&bytes, signature.trim(), key)?;
    if n.skipped > 0 {
        return Err(format!("notices: {} notices that this tool cannot read", n.skipped));
    }
    println!("notices: revision {} signed by the feed key", n.revision);
    Ok(())
}

/// For `fm_pack inspect` (sign-feature.mjs): a notice file signs for its revision.
pub fn inspect(bytes: &[u8]) -> Option<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    if v.get("kind")?.as_str()? != KIND {
        return None;
    }
    let revision = v.get("revision")?.as_u64()?;
    Some(serde_json::json!({ "id": "notices", "version": revision.to_string(), "kind": KIND }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_install::test_signing::TestKey;

    fn source(dir: &Path, notices: serde_json::Value) -> String {
        let p = dir.join("notices.json");
        std::fs::write(&p, serde_json::to_vec(&notices).unwrap()).unwrap();
        p.to_string_lossy().into_owned()
    }

    // The signing path of a notice set: revision up, `published` kept, image digest written,
    // stale signature deleted; a bad notice writes nothing; only the key's signature verifies.
    #[test]
    fn set_and_verify_a_signed_notice_file() {
        let key = TestKey::default();
        let t = tempfile::tempdir().unwrap();
        let feed = t.path().join("feed");
        std::fs::create_dir_all(feed.join("v2/notices")).unwrap();
        std::fs::write(feed.join("v2/notices/tmp.png"), b"png").unwrap();
        let feed = feed.to_string_lossy().into_owned();
        let one = serde_json::json!([{ "id": "tmp-rules", "kind": "policy", "title": "TruckersMP rules", "body": "Inputs are off in **TruckersMP**.", "games": ["ats"], "image": "v2/notices/tmp.png", "policies": ["ats-truckersmp-gameplay"] }]);
        set(&feed, &source(t.path(), one.clone()), 100).unwrap();
        let file = Path::new(&feed).join(FEED);
        let n: Notices = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!((n.revision, n.notices[0].published), (1, 100));
        assert_eq!(n.notices[0].image.as_ref().unwrap().sha256, format!("{:x}", sha2::Sha256::digest(b"png")));
        std::fs::write(Path::new(&feed).join(FEED_SIG), "old").unwrap();
        set(&feed, &source(t.path(), one), 200).unwrap();
        assert!(!Path::new(&feed).join(FEED_SIG).exists(), "the stale signature goes");
        let bytes = std::fs::read(&file).unwrap();
        let n: Notices = serde_json::from_slice(&bytes).unwrap();
        assert_eq!((n.revision, n.notices[0].published), (2, 100), "a kept notice keeps its time");
        for bad in [
            serde_json::json!([{ "id": "a-1", "kind": "policy", "title": "T", "body": "B", "policies": ["no-such-rule"] }]),
            serde_json::json!([{ "id": "a-1", "kind": "policy", "title": "T", "body": "B", "link": "https://evil.com" }]),
            serde_json::json!([{ "id": "a-1", "kind": "policy", "title": "T", "body": "B", "image": "v2/notices/none.png" }]),
            serde_json::json!([{ "id": "a-1", "kind": "policy", "title": "T", "body": "B" }, { "id": "a-1", "kind": "news", "title": "T", "body": "B" }]),
        ] {
            assert!(set(&feed, &source(t.path(), bad), 300).is_err());
            assert_eq!(std::fs::read(&file).unwrap(), bytes, "nothing written");
        }
        assert!(verify(&feed, &key.public()).unwrap_err().contains("notices.json.sig"));
        std::fs::write(Path::new(&feed).join(FEED_SIG), key.sign(&bytes, "2")).unwrap();
        verify(&feed, &key.public()).unwrap();
        std::fs::write(Path::new(&feed).join(FEED_SIG), TestKey::default().sign(&bytes, "2")).unwrap();
        assert!(verify(&feed, &key.public()).is_err(), "another key");
        assert_eq!(inspect(&bytes).unwrap(), serde_json::json!({ "id": "notices", "version": "2", "kind": "notices" }));
    }

    // A later set from a draft (Sol r2): a kept notice keeps its image object and the rules that it
    // named, also when a rule was lifted; a changed image object or a new name of a missing rule
    // is refused.
    #[test]
    fn a_kept_notice_keeps_its_image_and_its_lifted_rules() {
        let t = tempfile::tempdir().unwrap();
        let feed = t.path().join("feed");
        std::fs::create_dir_all(feed.join("v2/notices")).unwrap();
        std::fs::write(feed.join("v2/notices/tmp.png"), b"png").unwrap();
        let feed = feed.to_string_lossy().into_owned();
        let first = serde_json::json!([{ "id": "tmp-rules", "kind": "policy", "title": "T", "body": "B", "image": "v2/notices/tmp.png", "policies": ["ats-truckersmp-gameplay"] }]);
        set(&feed, &source(t.path(), first), 100).unwrap();
        let file = Path::new(&feed).join(FEED);
        let live: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        let kept = live["notices"][0].clone();
        // The rule is lifted: the feed now has no rule with that id.
        std::fs::create_dir_all(Path::new(&feed).join("v2/policies")).unwrap();
        std::fs::write(Path::new(&feed).join(fm_install::policy::FEED), serde_json::to_vec(&serde_json::json!({ "schema": 1, "kind": "policies", "revision": 1, "issued": 1, "rules": [] })).unwrap()).unwrap();
        let lift = serde_json::json!({ "id": "tmp-lifted", "kind": "policy", "title": "Inputs are on again", "body": "B" });
        set(&feed, &source(t.path(), serde_json::json!([kept.clone(), lift.clone()])), 200).unwrap();
        let n: Notices = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!((n.revision, n.notices.len(), n.notices[0].published, n.notices[0].policies.clone()), (2, 2, 100, vec!["ats-truckersmp-gameplay".to_string()]));
        let mut changed = kept.clone();
        changed["image"]["sha256"] = serde_json::json!("0".repeat(64));
        assert!(set(&feed, &source(t.path(), serde_json::json!([changed])), 300).unwrap_err().contains("image object"));
        let other = serde_json::json!({ "id": "tmp-other", "kind": "policy", "title": "T", "body": "B", "image": kept["image"].clone() });
        assert!(set(&feed, &source(t.path(), serde_json::json!([other])), 300).unwrap_err().contains("image object"), "another notice cannot take the image");
        let renamed = serde_json::json!({ "id": "tmp-new", "kind": "policy", "title": "T", "body": "B", "policies": ["ats-truckersmp-gameplay"] });
        assert!(set(&feed, &source(t.path(), serde_json::json!([renamed])), 300).unwrap_err().contains("no policy rule"));
    }
}
