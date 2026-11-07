// Notices (plan 2026-11-04-game-policies, slice 3): server messages to every app, first for
// policy changes (kind "policy"), later news. One signed file in the feed, the same shape as the
// policy file (signed_set.rs), so a network attacker cannot show a message. The body is a
// Markdown subset that the app renders itself (no HTML from the server). Links go to Feature
// Machine hosts only; an image is a feed file with its SHA-256, like catalog art.
use crate::feature::ArtFile;
use fm_platform::ApplicationId;
use serde::{Deserialize, Serialize};

/// The feed paths of the notice file and its signature, relative to the feed root.
pub const FEED: &str = "v2/notices/notices.json";
pub const FEED_SIG: &str = "v2/notices/notices.json.sig";
/// The folder of notice images in the feed: an image path must be in it.
pub const IMAGES: &str = "v2/notices/";
pub const KIND: &str = "notices";
const SCHEMA: u32 = 1;

/// Size limits: a notice is a short article, not a page.
pub mod limits {
    pub const TITLE: usize = 120;
    pub const BODY: usize = 16 * 1024;
    pub const IMAGE: u64 = 1024 * 1024;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    Policy,
    News,
}

/// One notice. `games`: the games it is about (empty: all). `policies`: the policy rule ids that
/// it explains (a rule names its notice in `Rule::notice`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notice {
    pub id: String,
    pub kind: NoticeKind,
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde(alias = "applications")]
    pub games: Vec<ApplicationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ArtFile>,
    /// Unix seconds when the notice was published.
    pub published: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policies: Vec<String>,
}

/// The notice file. `skipped`: notices that this build cannot read or that fail `Notice::check`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notices {
    pub schema: u32,
    pub kind: String,
    pub revision: u64,
    pub issued: u64,
    pub notices: Vec<Notice>,
    #[serde(skip)]
    pub skipped: usize,
}

/// An app that never read the feed has no notices (revision 0).
impl Default for Notices {
    fn default() -> Self {
        Notices { schema: SCHEMA, kind: KIND.into(), revision: 0, issued: 0, notices: vec![], skipped: 0 }
    }
}

impl Notices {
    /// Checks the signature (signed_set.rs), then reads each notice alone: one that this build
    /// cannot read, or that fails its checks, is left out and counted.
    pub fn verify(bytes: &[u8], signature: &str, key: &str) -> Result<Notices, String> {
        let (h, raw) = crate::signed_set::read(bytes, signature, key, KIND, SCHEMA, "notices")?;
        let total = raw.len();
        let notices: Vec<Notice> = raw.into_iter().filter_map(|n| serde_json::from_value::<Notice>(n).ok()).filter(|n| n.check().is_ok()).collect();
        Ok(Notices { schema: h.schema, kind: h.kind, revision: h.revision, issued: h.issued, skipped: total - notices.len(), notices })
    }

    /// True when `self` may replace `current`: a strictly higher revision (replay protection).
    pub fn replaces(&self, current: &Notices) -> bool {
        self.revision > current.revision
    }
}

impl Notice {
    /// The rules of a notice that the app shows: an id that can name a seen record, text within
    /// the limits, a link to a Feature Machine host and an image in the notice folder of the feed.
    pub fn check(&self) -> Result<(), String> {
        // Ignore unsupported targets rather than showing their notices as general messages.
        if self.games.iter().any(|id| id.definition().is_none()) { return Err(format!("notice {}: unknown application", self.id)); }
        crate::feature::check_id(&self.id).map_err(|e| format!("notice {}: {e}", self.id))?;
        if self.title.trim().is_empty() || self.title.chars().count() > limits::TITLE {
            return Err(format!("notice {}: the title must have 1 to {} characters", self.id, limits::TITLE));
        }
        if self.body.trim().is_empty() || self.body.len() > limits::BODY {
            return Err(format!("notice {}: the body must have 1 to {} bytes", self.id, limits::BODY));
        }
        if let Some(l) = self.link.as_deref().filter(|l| !trusted_link(l)) {
            return Err(format!("notice {}: the link {l} is not an https link to a Feature Machine host", self.id));
        }
        if let Some(i) = &self.image {
            if !feed_image(&i.url) || i.sha256.len() != 64 || !i.sha256.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
                return Err(format!("notice {}: the image must be a file in {IMAGES} with its SHA-256", self.id));
            }
        }
        Ok(())
    }
}

/// True for an https URL on featuremachine.app or one of its subdomains, with no user name and
/// no port: the only links that a notice can open (the body links too, in the app).
pub fn trusted_link(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let ok_chars = !host.is_empty() && host.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-');
    ok_chars && (host == "featuremachine.app" || host.ends_with(".featuremachine.app")) && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// True for a path of a file in the notice folder of the feed: letters, digits, "-", "_" and "."
/// in each segment, never "." or "..". No "%": a URL parser reads "%2e%2e" as ".." (Sol r1). No
/// "?", "#" or "\": the path names the same file for the app and the feed.
fn feed_image(path: &str) -> bool {
    let segment = |p: &str| !p.is_empty() && p != "." && p != ".." && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.');
    path.strip_prefix(IMAGES).is_some_and(|name| name.split('/').all(segment))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_signing::TestKey;

    fn notice(id: &str) -> serde_json::Value {
        serde_json::json!({ "id": id, "kind": "policy", "title": "No memory writes in MSFS 2024", "body": "## Why\n\nThe game developer asked.", "games": ["msfs2024"], "published": 7, "policies": ["msfs-no-writes"] })
    }

    fn file(revision: u64, notices: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({ "schema": 1, "kind": KIND, "revision": revision, "issued": 5, "notices": notices })).unwrap()
    }

    // Unsupported targets are counted after verifying the bytes, without hiding other notices.
    #[test]
    fn unknown_application_notices_are_skipped_individually() {
        let k = TestKey::default();
        let mut unknown = notice("unknown");
        unknown["games"] = serde_json::json!(["unreviewed-app"]);
        let bytes = file(9, serde_json::json!([unknown, notice("known")]));
        let n = Notices::verify(&bytes, &k.sign(&bytes, "9"), &k.public()).unwrap();
        assert_eq!((n.revision, n.skipped, n.notices[0].id.as_str()), (9, 1, "known"));
    }

    // The signature rules of the policy file: the feed key for the file's own revision and kind.
    #[test]
    fn a_signed_notice_file_passes_and_a_wrong_one_does_not() {
        let k = TestKey::default();
        let bytes = file(2, serde_json::json!([notice("first-one")]));
        let n = Notices::verify(&bytes, &k.sign(&bytes, "2"), &k.public()).unwrap();
        assert_eq!((n.revision, n.notices.len(), n.notices[0].games.clone()), (2, 1, vec![ApplicationId::MSFS2024]));
        assert!(Notices::verify(&bytes, &k.sign(&bytes, "1"), &k.public()).is_err(), "signed for another revision");
        assert!(Notices::verify(&bytes, &TestKey::default().sign(&bytes, "2"), &k.public()).is_err(), "another key");
        let policies = serde_json::to_vec(&serde_json::json!({ "schema": 1, "kind": "policies", "revision": 2, "issued": 5, "rules": [] })).unwrap();
        assert!(Notices::verify(&policies, &k.sign(&policies, "2"), &k.public()).is_err(), "a policy file is not a notice file");
        assert!(Notices::verify(&bytes, &k.sign(&bytes, "2"), &k.public()).unwrap().replaces(&Notices::default()));
    }

    // A notice of a newer kind, or one that breaks the link, image or text rules, is left out.
    #[test]
    fn a_notice_that_breaks_a_rule_is_skipped() {
        let k = TestKey::default();
        let mut bad = vec![];
        for (field, value) in [
            ("kind", serde_json::json!("survey")),
            ("link", serde_json::json!("http://featuremachine.app/blog")),
            ("link", serde_json::json!("https://featuremachine.app.evil.com/")),
            ("link", serde_json::json!("https://user@featuremachine.app/")),
            ("link", serde_json::json!("https://featuremachine.app:8443/")),
            ("image", serde_json::json!({ "url": "https://evil.com/a.png", "sha256": "0".repeat(64) })),
            ("image", serde_json::json!({ "url": "v2/notices/../catalog.json", "sha256": "0".repeat(64) })),
            ("image", serde_json::json!({ "url": "v2/notices/a.png", "sha256": "short" })),
            ("image", serde_json::json!({ "url": "v2/notices/%2e%2e/%2e%2e/catalog.json", "sha256": "0".repeat(64) })),
            ("image", serde_json::json!({ "url": "v2/notices/a.png?x=1", "sha256": "0".repeat(64) })),
            ("image", serde_json::json!({ "url": "v2/notices/a.png#x", "sha256": "0".repeat(64) })),
            ("title", serde_json::json!("x".repeat(limits::TITLE + 1))),
            ("body", serde_json::json!(" ")),
            ("id", serde_json::json!("Bad Id")),
        ] {
            // A valid id: each case breaks only its own field.
            let mut n = notice("bad-one");
            n[field] = value;
            bad.push(n);
        }
        let mut good = notice("good");
        good["link"] = "https://blog.featuremachine.app/policies/msfs".into();
        good["image"] = serde_json::json!({ "url": "v2/notices/msfs.png", "sha256": "a".repeat(64) });
        bad.push(good);
        let bytes = file(3, serde_json::Value::Array(bad));
        let n = Notices::verify(&bytes, &k.sign(&bytes, "3"), &k.public()).unwrap();
        assert_eq!((n.notices.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), n.skipped), (vec!["good"], 14));
    }
}
