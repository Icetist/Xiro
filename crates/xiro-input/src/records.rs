//! Pure builders for SendInput `INPUT` records (no FFI here, so they are unit-testable).

use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL,
    MOUSEINPUT, VIRTUAL_KEY,
};

use crate::geometry::{Rect, ScreenPoint, to_absolute};
use crate::keymap::ScanCode;

/// Every mouse record is absolute over the whole virtual desktop, so multi-monitor layouts map 1:1.
const ABSOLUTE_BASE: MOUSE_EVENT_FLAGS = MOUSE_EVENT_FLAGS(MOUSEEVENTF_ABSOLUTE.0 | MOUSEEVENTF_VIRTUALDESK.0);

/// `PointerEvent.buttons` bit -> (press flag, release flag).
const BUTTONS: [(u8, MOUSE_EVENT_FLAGS, MOUSE_EVENT_FLAGS); 3] = [
    (1, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
    (2, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
    (4, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
];

fn mouse((dx, dy): (i32, i32), flags: MOUSE_EVENT_FLAGS, mouse_data: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: mouse_data, dwFlags: ABSOLUTE_BASE | flags, time: 0, dwExtraInfo: 0 } },
    }
}

fn key(scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

/// Absolute move plus a press/release for every button whose state changed.
pub fn mouse_inputs(point: ScreenPoint, buttons: u8, prev_buttons: u8, vs: Rect) -> Vec<INPUT> {
    let abs = to_absolute(point, vs);
    let mut records = vec![mouse(abs, MOUSEEVENTF_MOVE, 0)];
    for (bit, down, up) in BUTTONS {
        let was = prev_buttons & bit != 0;
        let is = buttons & bit != 0;
        if was != is {
            records.push(mouse(abs, if is { down } else { up }, 0));
        }
    }
    records
}

/// `dx` horizontal (+right), `dy` vertical (+up), in WHEEL_DELTA units. Negative deltas travel as
/// the two's-complement DWORD that `mouseData` expects.
pub fn wheel_inputs(point: ScreenPoint, dx: i32, dy: i32, vs: Rect) -> Vec<INPUT> {
    let abs = to_absolute(point, vs);
    let mut records = vec![mouse(abs, MOUSEEVENTF_MOVE, 0)];
    if dy != 0 {
        records.push(mouse(abs, MOUSEEVENTF_WHEEL, dy as u32));
    }
    if dx != 0 {
        records.push(mouse(abs, MOUSEEVENTF_HWHEEL, dx as u32));
    }
    records
}

/// One scan-code key transition (layout-independent, like a physical key).
pub fn scan_key_inputs(sc: ScanCode, is_down: bool) -> Vec<INPUT> {
    let mut flags = KEYEVENTF_SCANCODE;
    if sc.extended {
        flags = flags | KEYEVENTF_EXTENDEDKEY;
    }
    if !is_down {
        flags = flags | KEYEVENTF_KEYUP;
    }
    vec![key(sc.scan, flags)]
}

/// Types arbitrary text (including emoji, as surrogate pairs): one down/up pair per UTF-16 unit.
pub fn unicode_inputs(units: impl IntoIterator<Item = u16>) -> Vec<INPUT> {
    units
        .into_iter()
        .flat_map(|unit| [key(unit, KEYEVENTF_UNICODE), key(unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP)])
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const VS: Rect = Rect { left: -1920, top: 0, width: 1920 + 2360, height: 1640 };
    const ORIGIN: ScreenPoint = ScreenPoint { x: 0, y: 0 };

    pub(crate) fn mi(r: &INPUT) -> MOUSEINPUT {
        assert_eq!(r.r#type, INPUT_MOUSE);
        // SAFETY: the type tag says the union holds `mi`.
        unsafe { r.Anonymous.mi }
    }

    pub(crate) fn ki(r: &INPUT) -> KEYBDINPUT {
        assert_eq!(r.r#type, INPUT_KEYBOARD);
        // SAFETY: the type tag says the union holds `ki`.
        unsafe { r.Anonymous.ki }
    }

    #[test]
    fn input_record_matches_the_x64_sdk_size() {
        assert_eq!(size_of::<INPUT>(), 40);
    }

    #[test]
    fn mouse_inputs_emit_a_move_plus_only_button_transitions() {
        let press = mouse_inputs(ORIGIN, 1, 0, VS);
        assert_eq!(press.len(), 2);
        assert!(mi(&press[0]).dwFlags.contains(MOUSEEVENTF_MOVE | ABSOLUTE_BASE));
        assert!(mi(&press[1]).dwFlags.contains(MOUSEEVENTF_LEFTDOWN));

        let swap = mouse_inputs(ORIGIN, 2, 1, VS);
        let flags: Vec<_> = swap[1..].iter().map(|r| mi(r).dwFlags).collect();
        assert!(flags.iter().any(|f| f.contains(MOUSEEVENTF_LEFTUP)));
        assert!(flags.iter().any(|f| f.contains(MOUSEEVENTF_RIGHTDOWN)));
        assert_eq!(mouse_inputs(ORIGIN, 0, 0, VS).len(), 1);
    }

    #[test]
    fn middle_button_and_coordinates() {
        let recs = mouse_inputs(ScreenPoint { x: -1920, y: 0 }, 4, 0, VS);
        assert!(mi(&recs[1]).dwFlags.contains(MOUSEEVENTF_MIDDLEDOWN));
        assert_eq!((mi(&recs[0]).dx, mi(&recs[0]).dy), (0, 0));
        let up = mouse_inputs(ORIGIN, 0, 4, VS);
        assert!(mi(&up[1]).dwFlags.contains(MOUSEEVENTF_MIDDLEUP));
    }

    #[test]
    fn wheel_inputs_encode_negative_deltas_as_unsigned_dwords() {
        let recs = wheel_inputs(ORIGIN, 30, -120, VS);
        assert_eq!(recs.len(), 3);
        assert_eq!(mi(&recs[1]).mouseData, (-120i32) as u32);
        assert!(mi(&recs[1]).dwFlags.contains(MOUSEEVENTF_WHEEL));
        assert!(mi(&recs[2]).dwFlags.contains(MOUSEEVENTF_HWHEEL));
        assert_eq!(wheel_inputs(ORIGIN, 0, 0, VS).len(), 1);
    }

    #[test]
    fn keyboard_records_use_scan_codes() {
        let up = scan_key_inputs(ScanCode { scan: 0x4b, extended: true }, false);
        assert_eq!(ki(&up[0]).wScan, 0x4b);
        assert_eq!(ki(&up[0]).dwFlags, KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP);
        let down = scan_key_inputs(ScanCode { scan: 0x1e, extended: false }, true);
        assert_eq!(ki(&down[0]).dwFlags, KEYEVENTF_SCANCODE);
    }

    #[test]
    fn unicode_text_is_one_pair_per_utf16_unit() {
        let text = unicode_inputs("h😀".encode_utf16());
        assert_eq!(text.len(), 6);
        assert_eq!(ki(&text[0]).wScan, u16::from(b'h'));
        assert_eq!(ki(&text[1]).dwFlags, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP);
        assert_eq!(ki(&text[2]).wScan, 0xd83d);
    }
}
