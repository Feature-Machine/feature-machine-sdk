// Validated IDs keep unknown applications in saved data without approving their execution.
// Inline ASCII storage keeps identity cheap to copy across the existing session and state keys.
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{cmp::Ordering, fmt, str::FromStr, sync::OnceLock};

pub const MAX_ID_BYTES: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ApplicationId { bytes: [u8; MAX_ID_BYTES], len: u8 }

impl ApplicationId {
    pub const MSFS2024: Self = Self::fixed("msfs2024");
    pub const ATS: Self = Self::fixed("ats");
    pub const DBOX_HAPTISYNC_CENTER: Self = Self::fixed("dbox-haptisync-center");
    pub const ALL: [Self; 3] = [Self::MSFS2024, Self::ATS, Self::DBOX_HAPTISYNC_CENTER];

    // Source aliases retain the old public enum spelling for existing SDK and tool callers.
    #[allow(non_upper_case_globals)]
    pub const Ats: Self = Self::ATS;
    #[allow(non_upper_case_globals)]
    pub const Msfs2024: Self = Self::MSFS2024;

    const fn fixed(text: &str) -> Self {
        let input = text.as_bytes();
        assert!(!input.is_empty() && input.len() <= MAX_ID_BYTES);
        let mut bytes = [0; MAX_ID_BYTES];
        let mut i = 0;
        while i < input.len() { bytes[i] = input[i]; i += 1; }
        Self { bytes, len: input.len() as u8 }
    }

    pub fn as_str(&self) -> &str {
        // Only fixed reviewed IDs and the validating parser can construct this ASCII value.
        std::str::from_utf8(&self.bytes[..self.len as usize]).expect("validated ASCII ID")
    }
    pub fn id(&self) -> &str { self.as_str() }
    pub fn definition(&self) -> Option<&'static ApplicationDefinition> { definitions().iter().find(|d| d.id == *self) }
    pub fn display_name(&self) -> &'static str { self.definition().map_or("Unsupported application", |d| d.name.as_str()) }

    // Preserve the existing MSFS/ATS ordering in member setup and session lists.
    fn rank(&self) -> u8 { if *self == Self::MSFS2024 { 0 } else if *self == Self::ATS { 1 } else { 2 } }
}

impl FromStr for ApplicationId {
    type Err = String;
    fn from_str(text: &str) -> Result<Self, String> {
        let valid = !text.is_empty() && text.len() <= MAX_ID_BYTES
            && text.as_bytes()[0].is_ascii_lowercase()
            && text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'));
        valid.then(|| Self::fixed(text)).ok_or_else(|| format!("not an application ID: {text:?}"))
    }
}
impl TryFrom<String> for ApplicationId {
    type Error = String;
    fn try_from(text: String) -> Result<Self, String> { text.parse() }
}
impl fmt::Display for ApplicationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
}
impl fmt::Debug for ApplicationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::Debug::fmt(self.as_str(), f) }
}
impl Ord for ApplicationId {
    fn cmp(&self, other: &Self) -> Ordering { self.rank().cmp(&other.rank()).then_with(|| self.as_str().cmp(other.as_str())) }
}
impl PartialOrd for ApplicationId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}
impl Serialize for ApplicationId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> { s.serialize_str(self.as_str()) }
}
impl<'de> Deserialize<'de> for ApplicationId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?.parse().map_err(serde::de::Error::custom)
    }
}

// This plain catalog is also the UI metadata source. Integrations own OS code and operations;
// a catalog entry alone never grants a session, power, file root, or native-code load.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationDefinition {
    pub id: ApplicationId,
    pub name: String,
    pub icon: String,
    pub art: String,
    pub platforms: Vec<String>,
    pub discovery: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<ProcessDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows_uninstall: Option<WindowsUninstallDefinition>,
    pub connections: Vec<String>,
    pub root_resolvers: Vec<String>,
    pub follows_features: bool,
    pub renders_view: bool,
    pub multiplayer: bool,
    /// Clients that the integration can detect for policy rules. Multiplayer play alone
    /// does not mean that the host can identify an active multiplayer client.
    pub multiplayer_clients: Vec<String>,
    pub data_description: String,
    pub file_description: Option<String>,
}

// Provider data has no OS handles or executable behavior. A matching descriptor still needs
// the compiled integration, platform validation and runtime admission before it can run.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessDefinition {
    pub steam_app_id: u32,
    pub store_family: Option<String>,
    pub executables: std::collections::BTreeMap<String, String>,
    pub tool_args: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowsUninstallDefinition {
    pub display_name: String,
    pub publisher: String,
    pub executables: Vec<String>,
    pub service: ServiceDefinition,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceDefinition { pub name: String, pub display_name: String, pub executable: String }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDefinition { pub id: String, pub name: String, pub icon: String }

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog { applications: Vec<ApplicationDefinition>, sources: Vec<SourceDefinition> }

pub const METADATA: &str = include_str!("../applications.json");
fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| serde_json::from_str(METADATA).expect("compiled application definitions"))
}
pub fn definitions() -> &'static [ApplicationDefinition] { &catalog().applications }
pub fn sources() -> &'static [SourceDefinition] { &catalog().sources }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_validate_and_unknown_ids_round_trip_without_support() {
        for text in ["ats", "msfs2024", "another_application-1", &"a".repeat(32)] {
            let id: ApplicationId = text.parse().unwrap();
            assert_eq!(serde_json::from_str::<ApplicationId>(&serde_json::to_string(&id).unwrap()).unwrap(), id);
            assert_eq!(id.as_str(), text);
        }
        for text in ["", "1app", "App", "../app", "a/b", "a.b", "a b", &"a".repeat(33), "café"] {
            assert!(text.parse::<ApplicationId>().is_err(), "{text:?}");
        }
        assert!("unreviewed-app".parse::<ApplicationId>().unwrap().definition().is_none());
    }

    #[test]
    fn descriptors_have_unique_ids_stable_order_and_known_sources() {
        let definitions = definitions();
        assert_eq!(definitions.iter().map(|d| d.id).collect::<Vec<_>>(), ApplicationId::ALL);
        for (index, d) in definitions.iter().enumerate() {
            assert!(!definitions[..index].iter().any(|other| other.id == d.id));
            assert!(!d.name.is_empty() && !d.icon.contains('/') && !d.art.contains('/'));
            assert!(!d.platforms.is_empty() && d.platforms.iter().all(|p| ["windows", "linux", "macos"].contains(&p.as_str())));
            assert!(!d.connections.is_empty() && d.connections.iter().all(|p| ["in_process", "external_service"].contains(&p.as_str())));
            assert!(d.discovery.iter().all(|s| sources().iter().any(|source| source.id == *s)));
            assert!(d.multiplayer || d.multiplayer_clients.is_empty());
            if let Some(process) = &d.process {
                assert_ne!(process.steam_app_id, 0);
                assert!(process.executables.keys().all(|os| d.platforms.contains(os)));
            }
            if let Some(rule) = &d.windows_uninstall {
                assert!(d.discovery.iter().any(|s| s == "windows_uninstall"));
                assert!(!rule.display_name.is_empty() && !rule.publisher.is_empty());
                assert!(!rule.executables.is_empty());
                assert!(rule.executables.iter().all(|p| !p.starts_with('/') && !p.contains(':') && !p.split('/').any(|c| c == "..")));
                assert!(!rule.service.name.is_empty() && !rule.service.executable.contains(['/', '\\']));
            }
        }
        assert_eq!(ApplicationId::ATS.as_str(), "ats");
        assert_eq!(ApplicationId::MSFS2024.as_str(), "msfs2024");
    }
}
