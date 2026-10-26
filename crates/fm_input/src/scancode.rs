// Key positions from the OS to USB HID usages (page 7), the one key code of this crate. Windows
// raw input gives the set 1 make code with an E0 or E1 prefix flag; Linux evdev gives KEY_*
// codes, which equal the set 1 make codes up to 88 (the Linux keyboard driver kept them). Pure
// tables, so both device layers share one tested mapping instead of each OS its own.

/// Set 1 make codes 0x00 to 0x58 without a prefix. 0 means no key.
const SET1: [u16; 0x59] = [
    0, 0x29, 0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x2D, 0x2E, 0x2A, 0x2B, // 00
    0x14, 0x1A, 0x08, 0x15, 0x17, 0x1C, 0x18, 0x0C, 0x12, 0x13, 0x2F, 0x30, 0x28, 0xE0, 0x04, 0x16, // 10
    0x07, 0x09, 0x0A, 0x0B, 0x0D, 0x0E, 0x0F, 0x33, 0x34, 0x35, 0xE1, 0x31, 0x1D, 0x1B, 0x06, 0x19, // 20
    0x05, 0x11, 0x10, 0x36, 0x37, 0x38, 0xE5, 0x55, 0xE2, 0x2C, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, // 30
    0x3F, 0x40, 0x41, 0x42, 0x43, 0x53, 0x47, 0x5F, 0x60, 0x61, 0x56, 0x5C, 0x5D, 0x5E, 0x57, 0x59, // 40
    0x5A, 0x5B, 0x62, 0x63, 0, 0, 0x64, 0x44, 0x45,                                               // 50
];

/// Windows raw input: `make` is RAWKEYBOARD::MakeCode, `e0` and `e1` its RI_KEY_E0 and RI_KEY_E1
/// flags. Pause comes as E1 1D. None for codes with no key (fake shifts: E0 2A and E0 36).
pub fn windows(make: u16, e0: bool, e1: bool) -> Option<u16> {
    if e1 {
        return (make == 0x1D).then_some(0x48);
    }
    let u = if e0 {
        match make {
            0x1C => 0x58, 0x1D => 0xE4, 0x35 => 0x54, 0x37 => 0x46, 0x38 => 0xE6,
            0x47 => 0x4A, 0x48 => 0x52, 0x49 => 0x4B, 0x4B => 0x50, 0x4D => 0x4F,
            0x4F => 0x4D, 0x50 => 0x51, 0x51 => 0x4E, 0x52 => 0x49, 0x53 => 0x4C,
            0x5B => 0xE3, 0x5C => 0xE7, 0x5D => 0x65,
            _ => 0,
        }
    } else {
        match make {
            0..=0x58 => SET1[make as usize],
            0x64..=0x6E => 0x68 + (make - 0x64),
            0x76 => 0x73,
            _ => 0,
        }
    };
    (u != 0).then_some(u)
}

/// The Windows set 1 make code and E0 flag of a HID usage (the inverse of `windows`), for the
/// OS layout lookup of a key name. Pause (E1) has none.
pub fn windows_make(usage: u16) -> Option<(u16, bool)> {
    [false, true].into_iter().find_map(|e0| (0..0x80u16).find(|&m| windows(m, e0, false) == Some(usage)).map(|m| (m, e0)))
}

/// Linux evdev KEY_* code.
pub fn linux(code: u16) -> Option<u16> {
    let u = match code {
        0..=88 => SET1[code as usize],
        96 => 0x58, 97 => 0xE4, 98 => 0x54, 99 => 0x46, 100 => 0xE6, 102 => 0x4A, 103 => 0x52,
        104 => 0x4B, 105 => 0x50, 106 => 0x4F, 107 => 0x4D, 108 => 0x51, 109 => 0x4E, 110 => 0x49,
        111 => 0x4C, 119 => 0x48, 125 => 0xE3, 126 => 0xE7, 127 => 0x65,
        183..=194 => 0x68 + (code - 183),
        _ => 0,
    };
    (u != 0).then_some(u)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::key_name;

    #[test]
    fn windows_codes() {
        let name = |m, e0| windows(m, e0, false).map(key_name);
        assert_eq!(name(0x13, false).as_deref(), Some("r"));
        assert_eq!(name(0x76, false).as_deref(), Some("f24")); // live spike value
        assert_eq!(name(0x1D, true).as_deref(), Some("ctrl")); // right Ctrl names as Ctrl
        assert_eq!(windows(0x1D, true, false), Some(0xE4));
        assert_eq!(name(0x48, false).as_deref(), Some("kp_8"));
        assert_eq!(name(0x48, true).as_deref(), Some("up"));
        assert_eq!(name(0x57, false).as_deref(), Some("f11"));
        assert_eq!(windows(0x1D, false, true), Some(0x48)); // Pause
        assert_eq!(windows(0x2A, true, false), None); // fake shift
    }

    #[test]
    fn windows_make_is_the_inverse() {
        assert_eq!(windows_make(0x15), Some((0x13, false))); // r
        assert_eq!(windows_make(0x52), Some((0x48, true))); // up
        assert_eq!(windows_make(0x48), None); // Pause is E1 only
        // 0x32 (ISO "#") has the scan code of 0x31 on Windows and Linux, so no key gives it.
        for u in (0x04..=0x38).chain([0x64]).filter(|&u| crate::element::is_character_key(u) && u != 0x32) {
            let (m, e0) = windows_make(u).unwrap_or_else(|| panic!("usage {u:#x}"));
            assert_eq!(windows(m, e0, false), Some(u));
        }
    }

    #[test]
    fn linux_codes_match_windows() {
        for code in 1..=88u16 {
            assert_eq!(linux(code), windows(code, false, false), "{code}");
        }
        assert_eq!(linux(103).map(key_name).as_deref(), Some("up"));
        assert_eq!(linux(194).map(key_name).as_deref(), Some("f24"));
        assert_eq!(linux(97), windows(0x1D, true, false));
    }
}
