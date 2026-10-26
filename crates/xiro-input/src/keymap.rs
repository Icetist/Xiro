//! `KeyboardEvent.code` -> PC/AT set-1 scan code. Scan codes are layout-independent,
//! so the PC applies its own keyboard layout exactly like a physical keyboard.

/// Extended-key marker in the table (the `E0` prefix on real hardware).
const E: u16 = 0x100;
const SCAN_MASK: u16 = 0xff;

/// A scan code ready for `KEYEVENTF_SCANCODE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanCode {
    pub scan: u16,
    pub extended: bool,
}

#[rustfmt::skip]
const SCANCODES: &[(&str, u16)] = &[
    ("Escape", 0x01), ("Digit1", 0x02), ("Digit2", 0x03), ("Digit3", 0x04), ("Digit4", 0x05),
    ("Digit5", 0x06), ("Digit6", 0x07), ("Digit7", 0x08), ("Digit8", 0x09), ("Digit9", 0x0a),
    ("Digit0", 0x0b), ("Minus", 0x0c), ("Equal", 0x0d), ("Backspace", 0x0e), ("Tab", 0x0f),
    ("KeyQ", 0x10), ("KeyW", 0x11), ("KeyE", 0x12), ("KeyR", 0x13), ("KeyT", 0x14), ("KeyY", 0x15),
    ("KeyU", 0x16), ("KeyI", 0x17), ("KeyO", 0x18), ("KeyP", 0x19), ("BracketLeft", 0x1a),
    ("BracketRight", 0x1b), ("Enter", 0x1c), ("ControlLeft", 0x1d), ("KeyA", 0x1e), ("KeyS", 0x1f),
    ("KeyD", 0x20), ("KeyF", 0x21), ("KeyG", 0x22), ("KeyH", 0x23), ("KeyJ", 0x24), ("KeyK", 0x25),
    ("KeyL", 0x26), ("Semicolon", 0x27), ("Quote", 0x28), ("Backquote", 0x29), ("ShiftLeft", 0x2a),
    ("Backslash", 0x2b), ("KeyZ", 0x2c), ("KeyX", 0x2d), ("KeyC", 0x2e), ("KeyV", 0x2f),
    ("KeyB", 0x30), ("KeyN", 0x31), ("KeyM", 0x32), ("Comma", 0x33), ("Period", 0x34),
    ("Slash", 0x35), ("ShiftRight", 0x36), ("NumpadMultiply", 0x37), ("AltLeft", 0x38),
    ("Space", 0x39), ("CapsLock", 0x3a), ("F1", 0x3b), ("F2", 0x3c), ("F3", 0x3d), ("F4", 0x3e),
    ("F5", 0x3f), ("F6", 0x40), ("F7", 0x41), ("F8", 0x42), ("F9", 0x43), ("F10", 0x44),
    ("NumLock", E | 0x45), ("ScrollLock", 0x46), ("Numpad7", 0x47), ("Numpad8", 0x48),
    ("Numpad9", 0x49), ("NumpadSubtract", 0x4a), ("Numpad4", 0x4b), ("Numpad5", 0x4c),
    ("Numpad6", 0x4d), ("NumpadAdd", 0x4e), ("Numpad1", 0x4f), ("Numpad2", 0x50), ("Numpad3", 0x51),
    ("Numpad0", 0x52), ("NumpadDecimal", 0x53), ("IntlBackslash", 0x56), ("F11", 0x57), ("F12", 0x58),
    ("NumpadEnter", E | 0x1c), ("ControlRight", E | 0x1d), ("NumpadDivide", E | 0x35),
    ("PrintScreen", E | 0x37), ("AltRight", E | 0x38), ("Home", E | 0x47), ("ArrowUp", E | 0x48),
    ("PageUp", E | 0x49), ("ArrowLeft", E | 0x4b), ("ArrowRight", E | 0x4d), ("End", E | 0x4f),
    ("ArrowDown", E | 0x50), ("PageDown", E | 0x51), ("Insert", E | 0x52), ("Delete", E | 0x53),
    ("MetaLeft", E | 0x5b), ("MetaRight", E | 0x5c), ("ContextMenu", E | 0x5d),
];

/// With `cmd_as_ctrl`, the iPad's Cmd key acts as Ctrl so Mac muscle memory (Cmd+C) works.
const CMD_AS_CTRL: &[(&str, &str)] = &[("MetaLeft", "ControlLeft"), ("MetaRight", "ControlRight")];

/// Looks up the scan code for a `KeyboardEvent.code`; `None` for keys Xiro does not map.
pub fn scan_code_for(code: &str, cmd_as_ctrl: bool) -> Option<ScanCode> {
    let mapped = if cmd_as_ctrl {
        CMD_AS_CTRL.iter().find(|(from, _)| *from == code).map_or(code, |(_, to)| *to)
    } else {
        code
    };
    let (_, value) = SCANCODES.iter().find(|(name, _)| *name == mapped)?;
    Some(ScanCode { scan: value & SCAN_MASK, extended: value & E != 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_codes_to_scan_codes_with_extended_flag() {
        assert_eq!(scan_code_for("KeyA", false), Some(ScanCode { scan: 0x1e, extended: false }));
        assert_eq!(scan_code_for("ArrowLeft", false), Some(ScanCode { scan: 0x4b, extended: true }));
        assert_eq!(scan_code_for("MetaLeft", false), Some(ScanCode { scan: 0x5b, extended: true }));
        assert_eq!(scan_code_for("Unknown", false), None);
    }

    #[test]
    fn cmd_as_ctrl_remaps_only_meta_keys() {
        assert_eq!(scan_code_for("MetaLeft", true), Some(ScanCode { scan: 0x1d, extended: false }));
        assert_eq!(scan_code_for("MetaRight", true), Some(ScanCode { scan: 0x1d, extended: true }));
        assert_eq!(scan_code_for("KeyC", true), scan_code_for("KeyC", false));
    }

    #[test]
    fn table_has_no_duplicate_codes() {
        let mut names: Vec<_> = SCANCODES.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), SCANCODES.len());
    }
}
