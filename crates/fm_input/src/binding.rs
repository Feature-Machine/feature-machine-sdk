// Controls and bindings. A feature declares controls (id, kind, label, optional default); the
// member binds each to one or more inputs. A binding is a trigger element on a device, held
// modifier elements (a chord, such as Ctrl+Shift+F or a HOTAS shift button plus a hat), and the
// axis tuning. The store is controls.json in the app data folder, versioned (plan, App).
use crate::device::{DeviceRef, Source};
use crate::element::Element;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Down and up.
    Button,
    /// Absolute value: -1..1, or 0..1 with `half`.
    Axis,
    /// Relative: mouse counts or wheel notches since the last event.
    Delta,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tuning {
    #[serde(default)]
    pub deadzone: f32,
    #[serde(default)]
    pub invert: bool,
    /// 1 is linear; above 1 is softer near the center.
    #[serde(default = "one")]
    pub curve: f32,
    /// Use the axis as 0..1 (a trigger or a throttle lever): -1 maps to 0.
    #[serde(default)]
    pub half: bool,
}

fn one() -> f32 { 1.0 }

impl Default for Tuning {
    fn default() -> Self { Tuning { deadzone: 0.0, invert: false, curve: 1.0, half: false } }
}

impl Tuning {
    pub fn apply(&self, v: f32) -> f32 {
        let mut v = if self.invert { -v } else { v };
        if self.half {
            v = (v + 1.0) / 2.0;
        }
        let dz = self.deadzone.clamp(0.0, 0.95);
        let (mag, sign) = (v.abs(), v.signum());
        let m = if mag <= dz { 0.0 } else { ((mag - dz) / (1.0 - dz)).powf(self.curve.clamp(0.2, 5.0)) };
        (sign * m).clamp(-1.0, 1.0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub device: DeviceRef,
    pub trigger: Element,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modifiers: Vec<Element>,
    #[serde(default)]
    pub tuning: Tuning,
}

impl Binding {
    /// A feature default: keyboard and mouse only, "ctrl+shift+r", "mouse:wheel:up",
    /// "alt+mouse:button:4". All but the last part are keys held as modifiers; they become a
    /// sorted set, as learn makes them, so "shift+ctrl+r" and "ctrl+shift+r" are one binding.
    pub fn parse_default(text: &str) -> Result<Binding, String> {
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let elems = parts.iter().map(|p| p.parse::<Element>()).collect::<Result<Vec<_>, _>>()?;
        let (trigger, mods) = elems.split_last().ok_or("empty input")?;
        if mods.iter().any(|m| !matches!(m, Element::Key(_))) {
            return Err(format!("{text:?}: only keys can be held before the last input"));
        }
        let source = match trigger {
            Element::Key(_) => Source::Keyboard,
            Element::MouseButton(_) | Element::WheelStep(_) | Element::WheelAxis(_) | Element::Move(_) => Source::Mouse,
            _ => return Err(format!("{text:?}: a default can name only keyboard and mouse inputs")),
        };
        let mut b = Binding { device: DeviceRef::Any(source), trigger: *trigger, modifiers: vec![], tuning: Tuning::default() };
        b.modifiers = Binding { modifiers: mods.to_vec(), ..b.clone() }.input_key().2;
        Ok(b)
    }

    /// The input with no tuning, right modifiers as left ones and the modifiers as a sorted set:
    /// two bindings with the same key fire together (a hand-edited store can have any order).
    pub fn input_key(&self) -> (DeviceRef, Element, Vec<Element>) {
        let mut m: Vec<Element> = self.modifiers.iter().map(|e| e.canonical()).collect();
        m.sort();
        m.dedup();
        (self.device.clone(), self.trigger.canonical(), m)
    }

    /// Text for the app: "Ctrl + Shift + R", "Button 5 + Hat 1 up on T.16000M".
    pub fn label(&self) -> String { self.label_with(&|_| None) }

    /// `label` with the layout names of character keys (Element::label_with).
    pub fn label_with(&self, layout: &dyn Fn(u16) -> Option<String>) -> String {
        let text = self.modifiers.iter().chain([&self.trigger]).map(|e| e.label_with(layout)).collect::<Vec<_>>().join(" + ");
        match &self.device {
            DeviceRef::Joystick(fp) if !fp.name.is_empty() => format!("{text} on {}", fp.name),
            _ => text,
        }
    }

    /// A key with no modifier: the game and its text fields get the key too (plan, Risks 1, 2).
    pub fn single_key(&self) -> bool {
        matches!(self.trigger, Element::Key(_)) && self.modifiers.is_empty()
    }

    /// Whether this binding can drive a control of this kind.
    pub fn fits(&self, kind: Kind) -> bool {
        match kind {
            Kind::Button => self.trigger.is_switch(),
            Kind::Axis => matches!(self.trigger, Element::Axis(_)),
            Kind::Delta => matches!(self.trigger, Element::Move(_) | Element::WheelAxis(_)),
        }
    }
}

/// One control as the feature declared it (feature.json "controls").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlDecl {
    pub id: String,
    pub kind: Kind,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

impl ControlDecl {
    /// Checks the id (the SDK rule, which the hosts use too) and that the default fits the kind.
    pub fn check(&self) -> Result<(), String> {
        if !fm_feature_sdk::valid_control_id(&self.id) { return Err(format!("bad control id {:?}", self.id)); }
        if self.label.trim().is_empty() || self.label.len() > 80 { return Err(format!("control {}: label must be 1 to 80 bytes", self.id)); }
        if let Some(d) = &self.default {
            let b = Binding::parse_default(d)?;
            if !b.fits(self.kind) { return Err(format!("control {}: default {d:?} does not fit a {:?} control", self.id, self.kind)); }
        }
        Ok(())
    }
}

pub const STORE_VERSION: u32 = 1;

/// The bindings of the features: feature id, then control id, then its bindings.
pub type FeatureBindings = BTreeMap<String, BTreeMap<String, Vec<Binding>>>;

/// controls.json: feature id, then control id, then its bindings. A control with no entry uses
/// its default; an empty list means the member removed all bindings. `installs`: per-install
/// setup (app plan 2026-11-03), the bindings of each game install by install id; `features`
/// then stays the form that an older app reads (it drops `installs` at its next save).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Store {
    pub version: u32,
    #[serde(default)]
    pub features: FeatureBindings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installs: Option<BTreeMap<String, FeatureBindings>>,
}

/// An empty store has the current version, so it saves as a file that `from_json` reads.
impl Default for Store {
    fn default() -> Self { Store { version: STORE_VERSION, features: BTreeMap::new(), installs: None } }
}

impl Store {
    pub fn from_json(text: &str) -> Result<Store, String> {
        let s: Store = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if s.version != STORE_VERSION { return Err(format!("controls.json version {} is not {STORE_VERSION}", s.version)); }
        Ok(s)
    }

    /// The store that a game of `install` uses: the bindings of that install after the
    /// per-install migration (an install with no entry: the defaults), else the feature bindings.
    pub fn for_install(&self, install: Option<&str>) -> Store {
        let features = match (&self.installs, install) {
            (Some(by), Some(i)) => by.get(i).cloned().unwrap_or_default(),
            _ => self.features.clone(),
        };
        Store { version: self.version, features, installs: None }
    }

    /// The bindings of one control: the member's, else the default.
    pub fn bindings(&self, feature: &str, decl: &ControlDecl) -> Vec<Binding> {
        match self.features.get(feature).and_then(|c| c.get(&decl.id)) {
            Some(list) => list.iter().filter(|b| b.fits(decl.kind)).cloned().collect(),
            None => decl.default.as_deref().and_then(|d| Binding::parse_default(d).ok()).into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse() {
        let b = Binding::parse_default("ctrl+shift+R").unwrap();
        assert_eq!(b.device, DeviceRef::Any(Source::Keyboard));
        assert_eq!(b.trigger.to_string(), "key:r");
        assert_eq!(b.modifiers.len(), 2);
        assert_eq!(Binding::parse_default("alt+mouse:button:4").unwrap().device, DeviceRef::Any(Source::Mouse));
        assert!(Binding::parse_default("button:3").is_err());
        assert!(Binding::parse_default("mouse:button:4+r").is_err());
    }

    // Slice 4 review round 1: the same chord in another order is the same input.
    #[test]
    fn modifier_order_does_not_count() {
        let a = Binding::parse_default("shift+ctrl+r").unwrap();
        let b = Binding::parse_default("ctrl+shift+r").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.label(), "Ctrl + Shift + R");
        let mut c = b.clone();
        c.modifiers.reverse();
        c.tuning.deadzone = 0.2;
        assert_ne!(c, b);
        assert_eq!(c.input_key(), b.input_key());
    }

    #[test]
    fn labels_and_single_keys() {
        let b = Binding::parse_default("ctrl+shift+r").unwrap();
        assert_eq!(b.label(), "Ctrl + Shift + R");
        assert!(!b.single_key());
        assert!(Binding::parse_default("r").unwrap().single_key());
        assert!(!Binding::parse_default("mouse:button:4").unwrap().single_key());
        let fp = crate::Fingerprint { vid: 1, pid: 2, name: "T.16000M".into(), guid: String::new(), serial: None, path: None, axes: 4, buttons: 16, hats: 1 };
        let j = Binding { device: DeviceRef::Joystick(fp), trigger: Element::Hat(0, crate::element::HatDir::Up), modifiers: vec![Element::Button(4)], tuning: Tuning::default() };
        assert_eq!(j.label(), "Button 5 + Hat 1 up on T.16000M");
        // Slice 4 review round 1: an AZERTY member sees the letter that the key types. The key
        // at the US Q position (usage 0x14) types A; named keys keep their English names.
        let azerty = |u: u16| match u { 0x14 => Some("A".to_string()), 0x04 => Some("Q".to_string()), _ => None };
        assert_eq!(Binding::parse_default("ctrl+q").unwrap().label_with(&azerty), "Ctrl + A");
        assert_eq!(Binding::parse_default("a").unwrap().label_with(&azerty), "Q");
        let all = |_: u16| Some("X".to_string());
        assert_eq!(Binding::parse_default("shift+f5").unwrap().label_with(&all), "Shift + F5");
        assert_eq!(Binding::parse_default("kp_7").unwrap().label_with(&all), "Keypad 7");
    }

    #[test]
    fn declarations_are_checked() {
        let d = |id: &str, kind, def: Option<&str>| ControlDecl { id: id.into(), kind, label: "Zoom".into(), default: def.map(Into::into) };
        assert!(d("zoom", Kind::Delta, Some("mouse:wheel:y")).check().is_ok());
        assert!(d("toggle", Kind::Button, Some("ctrl+r")).check().is_ok());
        assert!(d("Bad Id", Kind::Button, None).check().is_err());
        assert!(d("zoom", Kind::Axis, Some("ctrl+r")).check().is_err());
    }

    #[test]
    fn tuning() {
        let t = Tuning { deadzone: 0.1, ..Default::default() };
        assert_eq!(t.apply(0.05), 0.0);
        assert!((t.apply(0.55) - 0.5).abs() < 1e-6);
        assert_eq!(t.apply(-1.0), -1.0);
        let half = Tuning { half: true, invert: true, ..Default::default() };
        assert_eq!(half.apply(1.0), 0.0);
        assert_eq!(half.apply(-1.0), 1.0);
    }

    #[test]
    fn store_round_trip_and_default() {
        let decl = ControlDecl { id: "toggle".into(), kind: Kind::Button, label: "Toggle".into(), default: Some("ctrl+r".into()) };
        assert_eq!(Store::from_json(&serde_json::to_string(&Store::default()).unwrap()).unwrap(), Store::default());
        let mut s = Store::default();
        assert_eq!(s.bindings("f", &decl).len(), 1);
        s.features.entry("f".into()).or_default().insert("toggle".into(), vec![]);
        assert!(s.bindings("f", &decl).is_empty());
        let text = serde_json::to_string(&s).unwrap();
        assert_eq!(Store::from_json(&text).unwrap(), s);
        assert!(Store::from_json(r#"{"version":9}"#).is_err());
    }
}
