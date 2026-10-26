// Elements: the physical inputs a binding can name, with one text form for feature.json defaults,
// the binding store and the UI. Keys are USB HID usages (page 7), so a key is its position, not
// its layout letter: AZERTY and QWERTY members bind the same key (plan, device layer). Right
// modifiers count as their left twin, because members expect "Ctrl" to mean either Ctrl key.
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HatDir { Up, Right, Down, Left }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Wheel { Up, Down, Right, Left }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MoveAxis { X, Y }

/// One physical input. Joystick indexes are SDL's (0-based).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Element {
    Button(u16),
    Axis(u8),
    Hat(u8, HatDir),
    Key(u16),
    MouseButton(u8),
    /// One wheel notch as a press (button controls).
    WheelStep(Wheel),
    /// Wheel travel in notches (delta controls): "mouse:wheel:y" or "mouse:wheel:x".
    WheelAxis(MoveAxis),
    /// Relative mouse movement in counts (delta controls).
    Move(MoveAxis),
}

pub const KEY_LCTRL: u16 = 0xE0;
pub const KEY_LSHIFT: u16 = 0xE1;
pub const KEY_LALT: u16 = 0xE2;
pub const KEY_LGUI: u16 = 0xE3;

impl Element {
    /// The form used for matching: right Ctrl, Shift, Alt and Win become the left ones.
    pub fn canonical(self) -> Element {
        match self {
            Element::Key(u @ 0xE4..=0xE7) => Element::Key(u - 4),
            e => e,
        }
    }
    pub fn is_modifier(self) -> bool {
        matches!(self.canonical(), Element::Key(KEY_LCTRL..=KEY_LGUI))
    }
    /// Elements that give a pressed state (button controls and chord modifiers).
    pub fn is_switch(self) -> bool {
        matches!(self, Element::Button(_) | Element::Hat(..) | Element::Key(_) | Element::MouseButton(_) | Element::WheelStep(_) | Element::Axis(_))
    }
}

/// HID usage names (page 7). Letters and digits are the US position names.
const KEYS: &[(u16, &str)] = &[
    (0x28, "enter"), (0x29, "esc"), (0x2A, "backspace"), (0x2B, "tab"), (0x2C, "space"),
    (0x2D, "minus"), (0x2E, "equal"), (0x2F, "lbracket"), (0x30, "rbracket"), (0x31, "backslash"),
    (0x33, "semicolon"), (0x34, "quote"), (0x35, "grave"), (0x36, "comma"), (0x37, "period"),
    (0x38, "slash"), (0x39, "capslock"), (0x46, "printscreen"), (0x47, "scrolllock"), (0x48, "pause"),
    (0x49, "insert"), (0x4A, "home"), (0x4B, "pageup"), (0x4C, "delete"), (0x4D, "end"),
    (0x4E, "pagedown"), (0x4F, "right"), (0x50, "left"), (0x51, "down"), (0x52, "up"),
    (0x53, "numlock"), (0x54, "kp_divide"), (0x55, "kp_multiply"), (0x56, "kp_minus"), (0x57, "kp_plus"),
    (0x58, "kp_enter"), (0x62, "kp_0"), (0x63, "kp_period"), (0x64, "nonus_backslash"), (0x65, "menu"),
    (KEY_LCTRL, "ctrl"), (KEY_LSHIFT, "shift"), (KEY_LALT, "alt"), (KEY_LGUI, "win"),
];

pub fn key_name(usage: u16) -> String {
    match usage {
        0x04..=0x1D => ((b'a' + (usage - 0x04) as u8) as char).to_string(),
        0x1E..=0x26 => ((b'1' + (usage - 0x1E) as u8) as char).to_string(),
        0x27 => "0".into(),
        0x3A..=0x45 => format!("f{}", usage - 0x3A + 1),
        0x59..=0x61 => format!("kp_{}", usage - 0x59 + 1),
        0x68..=0x73 => format!("f{}", usage - 0x68 + 13),
        0xE4..=0xE7 => key_name(usage - 4),
        _ => KEYS.iter().find(|(u, _)| *u == usage).map(|(_, n)| n.to_string()).unwrap_or_else(|| format!("0x{usage:02x}")),
    }
}

pub fn key_usage(name: &str) -> Option<u16> {
    let n = name.to_ascii_lowercase();
    let b = n.as_bytes();
    if b.len() == 1 && b[0].is_ascii_lowercase() { return Some(0x04 + (b[0] - b'a') as u16); }
    if b.len() == 1 && b[0].is_ascii_digit() { return Some(if b[0] == b'0' { 0x27 } else { 0x1E + (b[0] - b'1') as u16 }); }
    if let Some(f) = n.strip_prefix('f').and_then(|d| d.parse::<u16>().ok()) {
        return match f { 1..=12 => Some(0x3A + f - 1), 13..=24 => Some(0x68 + f - 13), _ => None };
    }
    if let Some(k) = n.strip_prefix("kp_").and_then(|d| d.parse::<u16>().ok()) {
        if (1..=9).contains(&k) { return Some(0x59 + k - 1); }
    }
    if let Some(h) = n.strip_prefix("0x") { return u16::from_str_radix(h, 16).ok(); }
    let alias = match n.as_str() { "control" => "ctrl", "escape" => "esc", "return" => "enter", other => other };
    KEYS.iter().find(|(_, k)| *k == alias).map(|(u, _)| *u)
}

fn dir_name<T: fmt::Debug>(d: T) -> String { format!("{d:?}").to_ascii_lowercase() }

impl Element {
    /// Text for the app ("Ctrl", "R", "F5", "Button 4", "Hat 1 up", "Wheel up"). Joystick
    /// indexes count from 1, as members count. Keys use the US position name.
    pub fn label(self) -> String { self.label_with(&|_| None) }

    /// `label`, with the member's layout name of a character key (`layout`: HID usage to the
    /// letter that the key types, from the OS). Named keys (F5, Enter, Keypad 7) keep the English
    /// name, so the text stays one language. Matching still uses the position (plan, device layer).
    pub fn label_with(self, layout: &dyn Fn(u16) -> Option<String>) -> String {
        match self {
            Element::Key(u) if is_character_key(u) => layout(u).unwrap_or_else(|| key_label(&key_name(u))),
            Element::Button(n) => format!("Button {}", n + 1),
            Element::Axis(n) => format!("Axis {}", n + 1),
            Element::Hat(n, d) => format!("Hat {} {}", n + 1, dir_name(d)),
            Element::Key(u) => key_label(&key_name(u)),
            Element::MouseButton(n) => match n {
                1 => "Left mouse button".into(),
                2 => "Right mouse button".into(),
                3 => "Middle mouse button".into(),
                n => format!("Mouse button {n}"),
            },
            Element::WheelStep(w) => format!("Wheel {}", dir_name(w)),
            Element::WheelAxis(MoveAxis::Y) => "Wheel".into(),
            Element::WheelAxis(MoveAxis::X) => "Side wheel".into(),
            Element::Move(a) => format!("Mouse movement {}", dir_name(a).to_ascii_uppercase()),
        }
    }
}

/// Keys that type a character that depends on the layout: letters, digits, punctuation and the
/// key next to left Shift on ISO keyboards (usage 0x64).
pub fn is_character_key(usage: u16) -> bool {
    matches!(usage, 0x04..=0x27 | 0x2D..=0x38 | 0x64)
}

/// "r" -> "R", "kp_7" -> "Keypad 7", "pageup" -> "Pageup", "0x87" stays.
fn key_label(name: &str) -> String {
    if let Some(k) = name.strip_prefix("kp_") { return format!("Keypad {k}"); }
    if name.starts_with("0x") { return name.into(); }
    if name.len() == 1 || (name.starts_with('f') && name[1..].parse::<u8>().is_ok()) {
        return name.to_ascii_uppercase();
    }
    let mut c = name.chars();
    c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
}

impl fmt::Display for Element {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Element::Button(n) => write!(f, "button:{n}"),
            Element::Axis(n) => write!(f, "axis:{n}"),
            Element::Hat(n, d) => write!(f, "hat:{n}:{}", dir_name(d)),
            Element::Key(u) => write!(f, "key:{}", key_name(u)),
            Element::MouseButton(n) => write!(f, "mouse:button:{n}"),
            Element::WheelStep(w) => write!(f, "mouse:wheel:{}", dir_name(w)),
            Element::WheelAxis(a) => write!(f, "mouse:wheel:{}", dir_name(a)),
            Element::Move(a) => write!(f, "mouse:move:{}", dir_name(a)),
        }
    }
}

impl std::str::FromStr for Element {
    type Err = String;
    /// Parses the Display form; a bare key name ("r", "ctrl", "f5") is a key too.
    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || format!("unknown input {s:?}");
        let p: Vec<&str> = s.split(':').collect();
        let num = |t: &str| t.parse::<u16>().map_err(|_| bad());
        let hat = |t: &str| match t { "up" => Ok(HatDir::Up), "right" => Ok(HatDir::Right), "down" => Ok(HatDir::Down), "left" => Ok(HatDir::Left), _ => Err(bad()) };
        let axis = |t: &str| match t { "x" => Ok(MoveAxis::X), "y" => Ok(MoveAxis::Y), _ => Err(bad()) };
        match p.as_slice() {
            ["button", n] => Ok(Element::Button(num(n)?)),
            ["axis", n] => Ok(Element::Axis(u8::try_from(num(n)?).map_err(|_| bad())?)),
            ["hat", n, d] => Ok(Element::Hat(u8::try_from(num(n)?).map_err(|_| bad())?, hat(d)?)),
            ["key", k] => key_usage(k).map(Element::Key).ok_or_else(bad),
            ["mouse", "button", n] => Ok(Element::MouseButton(u8::try_from(num(n)?).ok().filter(|b| (1..=5).contains(b)).ok_or_else(bad)?)),
            ["mouse", "wheel", w] => match *w {
                "up" => Ok(Element::WheelStep(Wheel::Up)), "down" => Ok(Element::WheelStep(Wheel::Down)),
                "right" => Ok(Element::WheelStep(Wheel::Right)), "left" => Ok(Element::WheelStep(Wheel::Left)),
                a => axis(a).map(Element::WheelAxis),
            },
            ["mouse", "move", a] => axis(a).map(Element::Move),
            [k] => key_usage(k).map(Element::Key).ok_or_else(bad),
            _ => Err(bad()),
        }
    }
}

impl Serialize for Element {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> { s.collect_str(self) }
}

impl<'de> Deserialize<'de> for Element {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?.parse().map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trips() {
        for s in ["button:12", "axis:3", "hat:0:left", "key:r", "key:f24", "key:kp_7", "key:ctrl", "key:0x87", "mouse:button:4", "mouse:wheel:up", "mouse:wheel:y", "mouse:move:x"] {
            let e: Element = s.parse().unwrap();
            assert_eq!(e.to_string(), s);
        }
    }

    #[test]
    fn keys_are_positions_and_right_modifiers_match_left() {
        assert_eq!("R".parse::<Element>().unwrap(), Element::Key(0x15));
        assert_eq!("key:0".parse::<Element>().unwrap(), Element::Key(0x27));
        assert_eq!("F13".parse::<Element>().unwrap(), Element::Key(0x68));
        assert_eq!(Element::Key(0xE4).canonical(), Element::Key(KEY_LCTRL));
        assert!(Element::Key(0xE6).is_modifier());
        assert_eq!(Element::Key(0xE5).to_string(), "key:shift");
    }

    #[test]
    fn labels_are_for_members() {
        let l = |s: &str| s.parse::<Element>().unwrap().label();
        assert_eq!([l("key:r"), l("key:ctrl"), l("key:f5"), l("key:kp_7"), l("key:pageup"), l("key:0x87")], ["R", "Ctrl", "F5", "Keypad 7", "Pageup", "0x87"]);
        assert_eq!([l("button:0"), l("axis:2"), l("hat:0:up")], ["Button 1", "Axis 3", "Hat 1 up"]);
        assert_eq!([l("mouse:button:1"), l("mouse:button:4"), l("mouse:wheel:down"), l("mouse:wheel:y"), l("mouse:move:x")], ["Left mouse button", "Mouse button 4", "Wheel down", "Wheel", "Mouse movement X"]);
    }

    #[test]
    fn bad_text_is_refused() {
        for s in ["button:x", "hat:0:north", "mouse:button:9", "mouse:wheel:z", "key:nokey", "f25", ""] {
            assert!(s.parse::<Element>().is_err(), "{s}");
        }
    }
}
