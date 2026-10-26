// The character that a key types on the member's keyboard layout, for key names in the app
// (control input plan: keys bind by position, the UI shows the layout name). Windows asks the
// layout of the window in front (the member's active layout); other systems give None and the
// app shows the US position name.

/// The unshifted character of a Windows set 1 scan code (`make`, `e0`), in upper case. None for
/// keys that type no visible character (Enter, Space, Tab) or with no layout entry.
#[cfg(windows)]
pub fn scan_char(make: u16, e0: bool) -> Option<String> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardLayout, MapVirtualKeyExW, MAPVK_VK_TO_CHAR, MAPVK_VSC_TO_VK_EX};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    // SAFETY: plain Win32 calls with no pointers kept; a null window gives thread 0 (this thread).
    let (vk, ch) = unsafe {
        let w = GetForegroundWindow();
        let tid = if w.is_null() { 0 } else { GetWindowThreadProcessId(w, std::ptr::null_mut()) };
        let hkl = GetKeyboardLayout(tid);
        let vk = MapVirtualKeyExW(u32::from(make) | if e0 { 0xE000 } else { 0 }, MAPVK_VSC_TO_VK_EX, hkl);
        (vk, if vk == 0 { 0 } else { MapVirtualKeyExW(vk, MAPVK_VK_TO_CHAR, hkl) })
    };
    // The high bit marks a dead key (^ on French layouts); the low word is its character.
    let c = char::from_u32(ch & 0xFFFF).filter(|c| vk != 0 && !c.is_control() && !c.is_whitespace())?;
    Some(c.to_uppercase().collect())
}

#[cfg(not(windows))]
pub fn scan_char(_make: u16, _e0: bool) -> Option<String> {
    None
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    // Any layout: the A position types a letter, and Enter, Space and the arrows type nothing.
    #[test]
    fn letter_keys_have_a_character_and_named_keys_do_not() {
        let a = scan_char(0x1E, false).expect("the A position types a character");
        assert_eq!(a.chars().count(), 1);
        assert!(a.chars().all(char::is_alphabetic), "{a}");
        for (make, e0) in [(0x1C, false), (0x39, false), (0x48, true), (0x0F, false)] {
            assert_eq!(scan_char(make, e0), None, "{make:#x}");
        }
    }
}
