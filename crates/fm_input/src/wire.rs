// Messages between the app and the fm_input_reader process: one JSON object per line on the
// child's stdin (to the reader) and stdout (from it); stderr is its log. A child process with
// stdio needs no pipe name or security setup, and it ends when the app closes stdin. Both sides
// use these types, so the format has one definition. Privacy rule (plan): no message carries a
// raw key, except the one binding that learn mode captures.
use crate::binding::{Binding, ControlDecl, Kind, Store};
use crate::device::{Fingerprint, Source};
use crate::engine::{DevId, FeatureRef, Value};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Bump when a message changes in a way that an older app or reader can not read.
/// 2: Learn and Learned carry the learn id. 3: Access. 4: feature versions in Controls and Games.
pub const WIRE_VERSION: u32 = 4;

/// The controls of one feature version. A game runs the version that it started with until it
/// ends, so two versions of a feature can run at the same time (one per game).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeatureControls {
    pub feature: String,
    pub version: String,
    pub controls: Vec<ControlDecl>,
}

/// A game session that is connected, with the feature versions that run in it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GameSession {
    pub pid: u32,
    pub features: Vec<FeatureRef>,
    /// The start of each running feature (feature id: activation, session sync plan 12.2). A new
    /// start with the same version gets its axis values again (Engine::restarted). Empty: an
    /// older host or app.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub starts: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ToReader {
    /// The controls of the feature versions that can run and the binding store (replaces the old set).
    Controls { features: Vec<FeatureControls>, store: Store },
    /// The connected game sessions with running features; the gate opens while one of them is
    /// in front, for its features only.
    Games { sessions: Vec<GameSession> },
    /// `id`: the app's id of this learn; Learned gives it back.
    Learn { kind: Kind, id: u64 },
    CancelLearn,
    /// Live joystick element values for the device list (support view). Never keyboard keys.
    Watch { on: bool },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: DevId,
    pub source: Source,
    /// Joysticks: identity for bindings. Keyboards and mice: name and path only, for the list.
    pub fingerprint: Fingerprint,
    /// Axes that the descriptor rule corrects (plan case 0.7e).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wrapped: Vec<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum FromReader {
    /// First line. `joysticks`: false when SDL is not built in or did not start (the log says
    /// why). `keyboard_mouse`: false on Linux before the udev permission step (Q5).
    Ready { version: u32, pid: u32, elevated: bool, joysticks: bool, keyboard_mouse: bool },
    /// Linux: `keyboard_mouse` of Ready changed (a keyboard or mouse plugged in with no access, or
    /// udev gave access). False while any keyboard or mouse node is denied.
    Access { keyboard_mouse: bool },
    /// The full list after each change.
    Devices { list: Vec<DeviceInfo> },
    /// The game in front (None: no game in front, the gate is closed). `elevated`: that game runs
    /// as admin and the reader does not, so keyboard and mouse events can not arrive (case 0.8).
    Focus { pid: Option<u32>, elevated: bool },
    Control { feature: String, control: String, #[serde(flatten)] value: Value },
    Learned { binding: Binding, id: u64 },
    /// Watch mode: one joystick element changed ("button:3", "axis:0" -1..1).
    Element { dev: DevId, element: String, value: f32 },
    /// A command line that the reader could not read; the reader keeps running.
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_round_trip() {
        let c = FromReader::Control { feature: "rain".into(), control: "toggle".into(), value: Value::Button(true) };
        let line = serde_json::to_string(&c).unwrap();
        assert_eq!(line, r#"{"t":"control","feature":"rain","control":"toggle","kind":"button","value":true}"#);
        assert_eq!(serde_json::from_str::<FromReader>(&line).unwrap(), c);
        let g: ToReader = serde_json::from_str(r#"{"t":"games","sessions":[{"pid":42,"features":[{"id":"rain","version":"1.0.0"}]}]}"#).unwrap();
        assert_eq!(g, ToReader::Games { sessions: vec![GameSession { pid: 42, features: vec![FeatureRef::new("rain", "1.0.0")], starts: Default::default() }] });
        let a: FromReader = serde_json::from_str(r#"{"t":"access","keyboard_mouse":false}"#).unwrap();
        assert_eq!(a, FromReader::Access { keyboard_mouse: false });
        let l: ToReader = serde_json::from_str(r#"{"t":"learn","kind":"axis","id":7}"#).unwrap();
        assert_eq!(l, ToReader::Learn { kind: Kind::Axis, id: 7 });
    }
}
