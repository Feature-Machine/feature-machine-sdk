// Device identity. A binding stores the fingerprint of its device; a live device matches it by
// the strongest tier both have: serial, then path, then VID, PID and counts, then name and counts.
// Slice 0 proved why: serials are often missing, identical devices share GUID and name, and the
// name can be a stale Windows cache, while the path is distinct per device and stays after a
// replug. Never the SDL instance id or the list order (plan, model).
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source { Joystick, Keyboard, Mouse }

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub vid: u16,
    pub pid: u16,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub guid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub axes: u8,
    pub buttons: u16,
    pub hats: u8,
}

/// Match strength; higher wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier { Name = 1, Model = 2, Path = 3, Serial = 4 }

impl Fingerprint {
    fn counts(&self, o: &Self) -> bool { self.axes == o.axes && self.buttons == o.buttons && self.hats == o.hats }

    /// How well a live device matches this saved one, or None. A serial or path on both sides
    /// that differs is a different device even when the model is the same (a twin).
    pub fn matches(&self, live: &Fingerprint) -> Option<Tier> {
        fn some(s: &Option<String>) -> Option<&str> { s.as_deref().filter(|v| !v.is_empty()) }
        match (some(&self.serial), some(&live.serial)) {
            (Some(a), Some(b)) if a == b && self.vid == live.vid && self.pid == live.pid => return Some(Tier::Serial),
            (Some(_), Some(_)) => return None,
            _ => {}
        }
        match (some(&self.path), some(&live.path)) {
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => return Some(Tier::Path),
            _ => {}
        }
        if self.vid == live.vid && self.pid == live.pid && self.vid != 0 && self.counts(live) {
            return Some(Tier::Model);
        }
        (!self.name.is_empty() && self.name == live.name && self.counts(live)).then_some(Tier::Name)
    }
}

/// The device side of a binding: any keyboard or mouse, or one joystick.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceRef {
    Any(Source),
    Joystick(Fingerprint),
}

/// Fixes an axis value from a device whose HID descriptor has Logical Maximum not above Logical
/// Minimum (HIDMaestro, cheap devices): DirectInput then gives the raw unsigned bits as signed
/// (0.5 reads -32768). The device layer sets `wrapped` from the HidP value caps (plan case 0.7e).
pub fn axis_value(raw: i16, wrapped: bool) -> f32 {
    let v = if wrapped { (raw as u16) as i32 - 32768 } else { raw as i32 };
    (v as f32 / 32767.0).clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn twin(path: &str) -> Fingerprint {
        Fingerprint { vid: 0x1209, pid: 0x000A, name: "FM Spike Twin".into(), guid: "0300f162".into(), serial: None, path: Some(path.into()), axes: 2, buttons: 8, hats: 0 }
    }

    #[test]
    fn twins_match_by_path_only() {
        let (a, b) = (twin(r"\\?\HID#HIDCLASS#1&39A06D2A"), twin(r"\\?\HID#HIDCLASS#1&4A35C771"));
        assert_eq!(a.matches(&a), Some(Tier::Path));
        assert_eq!(a.matches(&b), Some(Tier::Model)); // only a fallback; Path wins when present
    }

    #[test]
    fn serial_wins_and_separates() {
        let mut a = twin("p1");
        a.serial = Some("S1".into());
        let mut moved = twin("other-port");
        moved.serial = Some("S1".into());
        assert_eq!(a.matches(&moved), Some(Tier::Serial));
        moved.serial = Some("S2".into());
        assert_eq!(a.matches(&moved), None);
    }

    #[test]
    fn name_needs_counts() {
        let a = Fingerprint { name: "Stick".into(), axes: 3, ..Default::default() };
        assert_eq!(a.matches(&Fingerprint { name: "Stick".into(), axes: 3, ..Default::default() }), Some(Tier::Name));
        assert_eq!(a.matches(&Fingerprint { name: "Stick".into(), axes: 4, ..Default::default() }), None);
    }

    #[test]
    fn wrapped_axes_are_corrected() {
        // Live HIDMaestro samples before the fix (plan case 0.7e): set 0, 0.5, 1 read 0, -32768, -1.
        assert_eq!(axis_value(0, true), -1.0);
        assert!(axis_value(-32768, true).abs() < 1e-4);
        assert_eq!(axis_value(-1, true), 1.0);
        assert_eq!(axis_value(-32768, false), -1.0);
        assert_eq!(axis_value(16383, false), 16383.0 / 32767.0);
    }
}
