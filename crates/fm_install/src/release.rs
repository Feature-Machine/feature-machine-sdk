// Release rules shared by features and modules (docs/plans/2026-10-28-release-admin.md, client
// contract item 6). `min_app` is a build property, so it is in the signed package manifest; the
// channel is a publication property (a promotion moves the same bytes), so it is on the feed
// entry only. One check, fits_app, serves catalog read, install, lock build and load.
use serde::{Deserialize, Serialize};

/// The channel of a feed entry. A value that this app does not know is never offered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    #[default]
    Stable,
    Preview,
    #[serde(other)]
    Unknown,
}

impl Channel {
    /// For serde skip_serializing_if: a Stable entry writes no field, so legacy feed files that
    /// fm_pack writes keep their bytes (apps before 0.2 read them).
    pub fn is_stable(&self) -> bool {
        *self == Channel::Stable
    }

    /// A Stable app takes Stable entries only; a Preview app takes both (Stable is a promoted
    /// Preview, plan Q60).
    pub fn offered_to(self, app: Channel) -> bool {
        matches!((self, app), (Channel::Stable, Channel::Stable | Channel::Preview) | (Channel::Preview, Channel::Preview))
    }
}

/// True when an item that needs `min_app` runs on `app_version`. Semver precedence, so a
/// pre-release app is below its release (0.2.0-preview.3 < 0.2.0) and build metadata does not
/// count (Version's Ord compares it). A min_app or app version that does not parse fails closed.
pub fn fits_app(min_app: Option<&str>, app_version: &str) -> bool {
    let Some(min) = min_app else { return true };
    match (semver::Version::parse(min), semver::Version::parse(app_version)) {
        (Ok(min), Ok(app)) => app.cmp_precedence(&min).is_ge(),
        _ => false,
    }
}

/// This app for the release checks: its version and its update channel. The app sets it at
/// start and at each channel change. Default: version 0.0.0 on Stable, so before that every
/// item with a `min_app` fails closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub app: String,
    pub channel: Channel,
}

impl Default for Release {
    fn default() -> Self {
        Release { app: "0.0.0".into(), channel: Channel::Stable }
    }
}

impl Release {
    /// Why this app does not take a feed item of `channel` that needs `min_app`. None: it takes it.
    pub fn refuses(&self, channel: Channel, min_app: Option<&str>) -> Option<String> {
        if !channel.offered_to(self.channel) {
            return Some("it is not on your update channel".into());
        }
        min_app.filter(|m| !fits_app(Some(m), &self.app)).map(needs_app_text)
    }

    /// True when this app can load an installed item that needs `min_app`.
    pub fn runs(&self, min_app: Option<&str>) -> bool {
        fits_app(min_app, &self.app)
    }
}

/// The manifest check of a `min_app` value: a semver version, when present.
pub fn check_min_app(id: &str, min_app: Option<&str>) -> Result<(), String> {
    match min_app {
        Some(v) if semver::Version::parse(v).is_err() => Err(format!("{id}: min_app {v:?} is not a version")),
        _ => Ok(()),
    }
}

/// The card text of an item that needs a newer app.
pub fn needs_app_text(min_app: &str) -> String {
    format!("needs Feature Machine {min_app} or newer")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_app_uses_full_semver_order_and_fails_closed() {
        assert!(fits_app(None, "0.1.0"));
        assert!(fits_app(Some("0.2.0"), "0.2.0"));
        assert!(fits_app(Some("0.2.0"), "0.10.0"));
        assert!(!fits_app(Some("0.2.0"), "0.1.9"));
        assert!(!fits_app(Some("0.2.0"), "0.2.0-preview.3"), "a pre-release is below its release");
        assert!(fits_app(Some("0.2.0-preview.3"), "0.2.0-preview.3"));
        assert!(fits_app(Some("0.2.0-preview.3"), "0.2.0"));
        assert!(!fits_app(Some("0.2.0-preview.4"), "0.2.0-preview.3"));
        assert!(!fits_app(Some("two"), "0.2.0"));
        assert!(!fits_app(Some("0.1.0"), "dev"));
        assert!(fits_app(Some("0.2.0+build.1"), "0.2.0"), "build metadata has no precedence");
        assert!(fits_app(Some("0.2.0"), "0.2.0+build.9"));
        assert!(fits_app(Some("0.2.0-preview.3+b.2"), "0.2.0-preview.3+b.1"));
        assert!(!fits_app(Some("0.2.0-preview.4+b.1"), "0.2.0-preview.3+b.9"));
        assert!(check_min_app("a", Some("0.2.0-preview.1")).is_ok());
        assert!(check_min_app("a", None).is_ok());
        assert!(check_min_app("a", Some("0.2")).is_err());
    }

    #[test]
    fn a_stable_app_takes_stable_only_and_unknown_channels_are_never_offered() {
        assert!(Channel::Stable.offered_to(Channel::Stable));
        assert!(!Channel::Preview.offered_to(Channel::Stable));
        assert!(Channel::Stable.offered_to(Channel::Preview));
        assert!(Channel::Preview.offered_to(Channel::Preview));
        let unknown: Channel = serde_json::from_str(r#""nightly""#).unwrap();
        assert_eq!(unknown, Channel::Unknown);
        assert!(!unknown.offered_to(Channel::Preview) && !unknown.offered_to(Channel::Stable));
        assert_eq!(serde_json::to_string(&Channel::Preview).unwrap(), r#""preview""#);
    }

    // Legacy feed files keep their bytes: a Stable entry writes no channel, and an entry with no
    // channel reads as Stable.
    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Entry {
        id: String,
        #[serde(default, skip_serializing_if = "Channel::is_stable")]
        channel: Channel,
    }

    #[test]
    fn a_stable_entry_writes_no_channel_field() {
        let stable = Entry { id: "a".into(), channel: Channel::Stable };
        assert_eq!(serde_json::to_string(&stable).unwrap(), r#"{"id":"a"}"#);
        assert_eq!(serde_json::from_str::<Entry>(r#"{"id":"a"}"#).unwrap(), stable);
        let preview = Entry { id: "a".into(), channel: Channel::Preview };
        assert_eq!(serde_json::to_string(&preview).unwrap(), r#"{"id":"a","channel":"preview"}"#);
    }
}
