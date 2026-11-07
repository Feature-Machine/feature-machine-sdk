// Game policies (plan 2026-11-04-game-policies): server rules per game that deny powers, one
// feature or one creator, always or while a multiplayer client runs. One signed file in the
// feed, like the module catalog; a higher revision replaces the whole set, so a rule can be
// lifted. This file is the one rule engine: the app, the hosts and fm_pack share it, so a
// feature is never judged by two copies of the rules.
use fm_platform::ApplicationId;
use serde::{Deserialize, Serialize};

/// The feed paths of the policy file and its signature, relative to the feed root.
pub const FEED: &str = "v2/policies/policies.json";
pub const FEED_SIG: &str = "v2/policies/policies.json.sig";
/// The signed `kind` field: a file signed for one purpose never passes as another (the module
/// catalog is signed with the same key for its own revision).
pub const KIND: &str = "policies";
const SCHEMA: u32 = 1;
/// The multiplayer client name of TruckersMP, as the ATS plugin reports it (lower case).
pub const TRUCKERSMP: &str = "truckersmp";

/// When a rule applies and what it does to a running game: the types of active.json, so the
/// host reads the same names that the feed writes.
pub use fm_feature_sdk::active::{Apply, When};

/// One rule. The scope is the listed features and creators (none listed: every feature of the
/// game); in that scope, `deny` names the powers that may not run (empty: the whole feature).
/// So "no memory writes in this game" is `deny: [vars_write, game_memory_write]` (decision D2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    #[serde(alias = "application")]
    pub game: ApplicationId,
    pub when: When,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub creators: Vec<String>,
    #[serde(default)]
    pub apply: Apply,
    /// One sentence for the card and consent; the notice (`notice`) has the full text.
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    /// Unix seconds when the rule was published.
    pub since: u64,
}

/// The facts of a feature that rules match: its id, its creator (from the signed manifest of a
/// reviewed package; None for a local file) and its powers (capabilities plus module powers).
pub struct Subject<'a> {
    pub id: &'a str,
    pub creator: Option<&'a str>,
    pub powers: &'a [String],
}

/// One rule that matches a feature, and the powers it denies there (empty: the whole feature).
#[derive(Debug, PartialEq)]
pub struct Hit<'a> {
    pub rule: &'a Rule,
    pub powers: Vec<String>,
}

/// The policy file. `skipped`: rules that this build cannot read (a newer rule kind or game);
/// they are left out and counted, so one new rule does not stop the whole set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Policies {
    pub schema: u32,
    pub kind: String,
    pub revision: u64,
    pub issued: u64,
    pub rules: Vec<Rule>,
    #[serde(skip)]
    pub skipped: usize,
}

/// An app with no saved policy file applies the base rules (revision 0).
impl Default for Policies {
    fn default() -> Self {
        Policies::baseline()
    }
}

impl Policies {
    /// Checks the signature of the exact bytes with the feed key (signed for its revision, the
    /// module catalog format), then the kind and schema. Each rule is read alone (see `skipped`).
    pub fn verify(bytes: &[u8], signature: &str, key: &str) -> Result<Policies, String> {
        let (h, raw) = crate::signed_set::read(bytes, signature, key, KIND, SCHEMA, "rules")?;
        let total = raw.len();
        // An open ID preserves saved setup, but only reviewed applications can own a rule.
        // Filter after signature verification so skipped rules retain the signed revision.
        let rules: Vec<Rule> = raw.into_iter().filter_map(|r| serde_json::from_value::<Rule>(r).ok()).filter(|r| r.game.definition().is_some()).collect();
        Ok(Policies { schema: h.schema, kind: h.kind, revision: h.revision, issued: h.issued, skipped: total - rules.len(), rules })
    }

    /// The rules that the app carries (revision 0): the TruckersMP rule that the ATS plugin has
    /// in code today, so an app that never read the feed is not weaker than before. The first
    /// feed file replaces it (the feed carries the same rule, and may lift it later). The rules
    /// are policy_baseline.json: the fm-admin editor starts from the same file when the feed has
    /// no policy set, so a first draft keeps the rule (game policies plan, slice 5).
    pub fn baseline() -> Policies {
        let rules = serde_json::from_str(include_str!("policy_baseline.json")).expect("policy_baseline.json is a rule list (tested)");
        Policies { schema: SCHEMA, kind: KIND.into(), revision: 0, issued: 0, rules, skipped: 0 }
    }

    /// True when `self` may replace `current`: a strictly higher revision (replay protection).
    pub fn replaces(&self, current: &Policies) -> bool {
        self.revision > current.revision
    }

    /// Every rule of `game` that matches the feature, in file order.
    pub fn hits<'a>(&'a self, game: ApplicationId, f: &Subject) -> Vec<Hit<'a>> {
        self.rules.iter().filter(|r| r.game == game).filter_map(|r| r.hit(f)).collect()
    }
}

impl Rule {
    /// The hit of this rule on a feature, game aside: None when the feature is out of its scope
    /// or has none of its denied powers.
    pub fn hit(&self, f: &Subject) -> Option<Hit<'_>> {
        let scoped = self.features.is_empty() && self.creators.is_empty();
        let in_scope = scoped || self.features.iter().any(|id| id == f.id) || f.creator.is_some_and(|c| self.creators.iter().any(|x| x == c));
        if !in_scope {
            return None;
        }
        if self.deny.is_empty() {
            return Some(Hit { rule: self, powers: vec![] });
        }
        let powers: Vec<String> = f.powers.iter().filter(|p| self.deny.iter().any(|deny| fm_feature_sdk::effective_power(deny) == fm_feature_sdk::effective_power(p))).cloned().collect();
        (!powers.is_empty()).then_some(Hit { rule: self, powers })
    }

    /// True for a rule that applies while one of these clients runs.
    pub fn in_multiplayer(&self) -> bool {
        matches!(self.when, When::Multiplayer(_))
    }
}

/// The power names of a feature, as rules write them: its capabilities (snake_case, the
/// feature.json names) and the powers of the modules it requires. The app gives the capabilities
/// of a manifest or of a card (the same list).
pub fn powers_of(capabilities: &[crate::feature::Capability], module_powers: &[crate::module::Power]) -> Vec<String> {
    let name = |v: serde_json::Value| v.as_str().map(str::to_string);
    let mut out: Vec<String> = capabilities.iter().filter_map(|c| serde_json::to_value(c).ok().and_then(name)).collect();
    out.extend(module_powers.iter().filter_map(|p| serde_json::to_value(p).ok().and_then(name)));
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_signing::TestKey;

    fn rule(id: &str, when: When, deny: &[&str]) -> Rule {
        Rule { id: id.into(), game: ApplicationId::ATS, when, deny: deny.iter().map(|s| s.to_string()).collect(), features: vec![], creators: vec![], apply: Apply::Now, reason: "r".into(), notice: None, since: 1 }
    }

    fn file(revision: u64, rules: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({ "schema": 1, "kind": KIND, "revision": revision, "issued": 5, "rules": rules })).unwrap()
    }

    fn powers(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| s.to_string()).collect()
    }

    // The server file: only the feed key's signature for its own revision passes, and a file of
    // another kind (the module catalog) never passes as policies.
    #[test]
    fn a_signed_file_passes_and_a_wrong_one_does_not() {
        let k = TestKey::default();
        let bytes = file(3, serde_json::json!([{ "id": "a", "game": "ats", "when": "always", "deny": ["vars_write"], "reason": "r", "since": 1 }]));
        let p = Policies::verify(&bytes, &k.sign(&bytes, "3"), &k.public()).unwrap();
        assert_eq!((p.revision, p.rules.len(), p.skipped), (3, 1, 0));
        assert!(Policies::verify(&bytes, &k.sign(&bytes, "2"), &k.public()).is_err(), "signed for another revision");
        assert!(Policies::verify(&bytes, &TestKey::default().sign(&bytes, "3"), &k.public()).is_err(), "another key");
        let other = serde_json::to_vec(&serde_json::json!({ "schema": 1, "kind": "modules", "revision": 3, "issued": 5, "rules": [] })).unwrap();
        assert!(Policies::verify(&other, &k.sign(&other, "3"), &k.public()).is_err(), "another kind");
    }

    // A rule of a newer kind (unknown game or `when`) is left out and counted; the others apply.
    #[test]
    fn a_rule_that_this_build_cannot_read_is_skipped() {
        let k = TestKey::default();
        let bytes = file(4, serde_json::json!([
            { "id": "a", "game": "ats", "when": "always", "reason": "r", "since": 1 },
            { "id": "b", "game": "new-game", "when": "always", "reason": "r", "since": 1 },
            { "id": "c", "game": "ats", "when": { "on_weekends": true }, "reason": "r", "since": 1 },
        ]));
        let p = Policies::verify(&bytes, &k.sign(&bytes, "4"), &k.public()).unwrap();
        assert_eq!((p.rules.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), p.skipped), (vec!["a"], 2));
    }

    #[test]
    fn only_a_higher_revision_replaces_the_set() {
        let base = Policies::baseline();
        let mut next = base.clone();
        next.revision = 1;
        assert!(next.replaces(&base) && !base.replaces(&next) && !next.replaces(&next));
    }

    // Scope times powers (D2): no scope is every feature; a listed feature or creator narrows
    // it; no deny list blocks the whole feature in scope.
    #[test]
    fn a_rule_matches_by_power_feature_and_creator() {
        let p = powers(&["input_inject", "vars_read"]);
        let f = Subject { id: "cruise", creator: Some("acme"), powers: &p };
        let mut r = rule("x", When::Always, &["input_inject", "vars_write"]);
        assert_eq!(r.hit(&f).unwrap().powers, powers(&["input_inject"]));
        r.deny = powers(&["vars_write"]);
        assert!(r.hit(&f).is_none(), "no denied power");
        r.deny.clear();
        r.features = powers(&["other"]);
        assert!(r.hit(&f).is_none(), "another feature");
        r.creators = powers(&["acme"]);
        assert_eq!(r.hit(&f).unwrap().powers, Vec::<String>::new(), "the creator is in scope: the whole feature");
        let local = Subject { id: "cruise", creator: None, powers: &p };
        assert!(r.hit(&local).is_none(), "a local file has no creator");
    }

    #[test]
    fn hits_keep_to_the_game() {
        let mut p = Policies::baseline();
        p.rules.push(Rule { game: ApplicationId::MSFS2024, ..rule("m", When::Always, &[]) });
        let pw = powers(&["input_inject"]);
        let f = Subject { id: "f", creator: None, powers: &pw };
        assert_eq!(p.hits(ApplicationId::ATS, &f).iter().map(|h| h.rule.id.as_str()).collect::<Vec<_>>(), vec!["ats-truckersmp-gameplay"]);
        assert_eq!(p.hits(ApplicationId::MSFS2024, &f).iter().map(|h| h.rule.id.as_str()).collect::<Vec<_>>(), vec!["m"]);
    }

    // The baseline is the rule of the ATS plugin today: inputs and engine commands are off while
    // TruckersMP runs, for every feature.
    #[test]
    fn the_baseline_is_todays_truckersmp_rule() {
        let p = Policies::baseline();
        let r = &p.rules[0];
        assert_eq!((r.game, r.in_multiplayer(), r.apply), (ApplicationId::ATS, true, Apply::Now));
        assert_eq!(r.when, When::Multiplayer(vec![TRUCKERSMP.into()]));
        assert_eq!((p.rules.len(), r.id.as_str(), r.deny.clone(), r.since), (1, "ats-truckersmp-gameplay", vec!["input_inject".to_string(), "game_command".to_string()], 0));
        let json = serde_json::to_value(r).unwrap();
        assert_eq!(json["when"], serde_json::json!({ "multiplayer": ["truckersmp"] }));
        assert_eq!(serde_json::to_value(rule("a", When::Always, &[])).unwrap()["when"], "always");
    }

    #[test]
    fn powers_use_the_feature_json_names_and_module_powers() {
        let m: crate::feature::Manifest = serde_json::from_value(serde_json::json!({ "id": "f", "name": "F", "version": "1.0.0", "description": "d", "api": 3, "games": ["ats"],
            "capabilities": ["vars_write", "vars_memory", "vars_read"] })).unwrap();
        assert_eq!(powers_of(&m.capabilities, &[crate::module::Power::GameMemoryWrite]), powers(&["game_memory_write", "vars_memory", "vars_read", "vars_write"]));
    }
}
