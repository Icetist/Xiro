//! Injector state machine, driven through the recording fake sink (never the real desktop).

use std::sync::{Arc, Mutex};

use windows::Win32::UI::Input::KeyboardAndMouse::{
    KEYEVENTF_KEYUP, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_WHEEL,
};
use windows::Win32::UI::Input::Pointer::{POINTER_CHANGE_FIRSTBUTTON_DOWN, POINTER_CHANGE_FIRSTBUTTON_UP, POINTER_CHANGE_NONE};

use super::*;
use crate::pointer_records::tests::{pen, touch};
use crate::records::tests::{ki, mi};
use crate::sink::fake::FakeSink;

const TARGET: Rect = Rect { left: 0, top: 0, width: 1001, height: 1001 };
const VS: Rect = Rect { left: 0, top: 0, width: 1001, height: 1001 };

struct Rig {
    inj: Injector<FakeSink>,
    sink: FakeSink,
    warnings: Arc<Mutex<Vec<String>>>,
}

fn rig_with(configure: impl FnOnce(&mut crate::sink::fake::State)) -> Rig {
    let sink = FakeSink::default();
    sink.state().vs = VS;
    configure(&mut sink.state());
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let w = Arc::clone(&warnings);
    let mut inj = Injector::new(sink.clone(), false, Box::new(move |m| w.lock().expect("warnings").push(m)));
    inj.set_target(TARGET);
    Rig { inj, sink, warnings }
}

fn rig() -> Rig {
    rig_with(|_| {})
}

impl Rig {
    fn ptr(&mut self, kind: PointerKind, phase: Phase, id: u8, x: f64) {
        self.inj.pointer(&Pointer { kind, phase, id, x, y: 0.0, pressure: 0.5, tilt_x: 10, tilt_y: -10, buttons: 0 });
    }

    fn mouse(&mut self, buttons: u8) {
        self.inj.pointer(&Pointer { kind: PointerKind::Mouse, phase: Phase::Move, id: 0, x: 0.5, y: 0.5, pressure: 0.0, tilt_x: 0, tilt_y: 0, buttons });
    }

    fn warnings(&self) -> Vec<String> {
        self.warnings.lock().expect("warnings").clone()
    }

    /// (id, flags, x) per contact of every touch frame.
    fn touch_frames(&self) -> Vec<Vec<(u32, u32, i32)>> {
        self.sink
            .frames(DeviceKind::Touch)
            .iter()
            .map(|f| f.iter().map(|r| touch(r).pointerInfo).map(|i| (i.pointerId, i.pointerFlags.0, i.ptPixelLocation.x)).collect())
            .collect()
    }
}

#[test]
fn mouse_tracks_buttons_and_maps_to_target() {
    let mut r = rig();
    r.mouse(1);
    r.mouse(0);
    let sent = r.sink.sent();
    assert_eq!(sent.len(), 4);
    assert_eq!((mi(&sent[0]).dx, mi(&sent[0]).dy), (32768, 32768));
    assert!(mi(&sent[1]).dwFlags.contains(MOUSEEVENTF_LEFTDOWN));
    assert!(mi(&sent[3]).dwFlags.contains(MOUSEEVENTF_LEFTUP));
}

#[test]
fn wheel_sends_move_and_both_axes() {
    let mut r = rig();
    r.inj.wheel(&Wheel { x: 0.0, y: 0.0, dx: 120, dy: -120 });
    let sent = r.sink.sent();
    assert_eq!(sent.len(), 3);
    assert!(mi(&sent[1]).dwFlags.contains(MOUSEEVENTF_WHEEL));
    assert!(mi(&sent[2]).dwFlags.contains(MOUSEEVENTF_HWHEEL));
}

#[test]
fn pointer_and_wheel_are_dropped_until_a_target_is_set() {
    let mut r = rig();
    r.inj.set_target(Rect::default());
    r.mouse(1);
    r.inj.wheel(&Wheel { x: 0.0, y: 0.0, dx: 0, dy: 120 });
    assert!(r.sink.sent().is_empty());
    assert_eq!(r.warnings().len(), 1, "warned once");
}

#[test]
fn keys_map_through_keymap_and_unknown_codes_are_ignored() {
    let mut r = rig();
    r.inj.key("KeyA", true);
    r.inj.key("Nope", true);
    r.inj.key("KeyA", false);
    let sent = r.sink.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(ki(&sent[0]).wScan, 0x1e);
    assert!(ki(&sent[1]).dwFlags.contains(KEYEVENTF_KEYUP));
}

#[test]
fn cmd_as_ctrl_is_applied() {
    let sink = FakeSink::default();
    let mut inj = Injector::new(sink.clone(), true, Box::new(|_| {}));
    inj.key("MetaLeft", true);
    assert_eq!(ki(&sink.sent()[0]).wScan, 0x1d);
}

#[test]
fn text_is_truncated_to_256_utf16_units_and_empty_is_ignored() {
    let mut r = rig();
    r.inj.text("");
    assert!(r.sink.sent().is_empty());
    r.inj.text(&"x".repeat(300));
    assert_eq!(r.sink.sent().len(), 2 * MAX_TEXT_LENGTH);
}

#[test]
fn refresh_layout_requeries_the_virtual_screen() {
    let mut r = rig();
    let before = r.sink.state().vs_queries;
    r.inj.refresh_layout();
    assert_eq!(r.sink.state().vs_queries, before + 1);
}

#[test]
fn release_all_lifts_keys_buttons_touches_and_pen() {
    let mut r = rig();
    r.inj.key("ShiftLeft", true);
    r.inj.key("KeyA", true);
    r.mouse(1);
    r.ptr(PointerKind::Touch, Phase::Down, 3, 0.0);
    r.ptr(PointerKind::Pen, Phase::Hover, 0, 0.0);
    r.sink.clear();

    r.inj.release_all();
    let sent = r.sink.sent();
    let scans: Vec<_> = sent.iter().take(2).map(|k| (ki(k).wScan, ki(k).dwFlags.contains(KEYEVENTF_KEYUP))).collect();
    assert_eq!(scans, [(0x2a, true), (0x1e, true)], "keys lifted in press order");
    assert!(mi(&sent[3]).dwFlags.contains(MOUSEEVENTF_LEFTUP));
    assert_eq!(r.touch_frames(), [vec![(3, flag::UP | flag::CANCELED, 0)]]);
    assert!(!r.inj.is_touching());
    let pens = r.sink.frames(DeviceKind::Pen);
    assert_eq!(pens.len(), 1);
    assert_eq!(pen(&pens[0][0]).pointerInfo.pointerFlags.0, flag::UPDATE | flag::PRIMARY);

    r.sink.clear();
    r.inj.release_all();
    assert!(r.sink.state().events.is_empty(), "second release is a no-op");
}

#[test]
fn touch_frames_carry_every_contact_with_per_contact_flags() {
    let mut r = rig();
    r.ptr(PointerKind::Touch, Phase::Down, 0, 0.0);
    r.ptr(PointerKind::Touch, Phase::Down, 1, 0.5);
    r.ptr(PointerKind::Touch, Phase::Move, 0, 0.1);
    r.ptr(PointerKind::Touch, Phase::Up, 1, 0.6);
    assert_eq!(
        r.touch_frames(),
        [
            vec![(0, TOUCH_DOWN, 0)],
            vec![(0, TOUCH_MOVE, 0), (1, TOUCH_DOWN, 500)],
            vec![(0, TOUCH_MOVE, 100), (1, TOUCH_MOVE, 500)],
            vec![(0, TOUCH_MOVE, 100), (1, flag::UP, 600)],
        ]
    );
    assert_eq!(r.sink.creates(), [(DeviceKind::Touch, MAX_TOUCH_CONTACTS as u32)], "device created once");
    assert!(r.inj.is_touching());
    r.ptr(PointerKind::Touch, Phase::Cancel, 0, 0.1);
    assert_eq!(r.touch_frames().last(), Some(&vec![(0, flag::UP | flag::CANCELED, 100)]));
    assert!(!r.inj.is_touching());
}

#[test]
fn touch_ignores_unknown_moves_ends_and_hover() {
    let mut r = rig();
    r.ptr(PointerKind::Touch, Phase::Move, 5, 0.0);
    r.ptr(PointerKind::Touch, Phase::Up, 5, 0.0);
    r.ptr(PointerKind::Touch, Phase::Hover, 5, 0.0);
    assert!(r.touch_frames().is_empty());
}

#[test]
fn touch_caps_contacts_at_ten() {
    let mut r = rig();
    for id in 0..=MAX_TOUCH_CONTACTS as u8 {
        r.ptr(PointerKind::Touch, Phase::Down, id, 0.0);
    }
    assert_eq!(r.touch_frames().len(), MAX_TOUCH_CONTACTS);
    r.ptr(PointerKind::Touch, Phase::Down, 0, 0.2);
    assert_eq!(r.touch_frames().last().map(Vec::len), Some(MAX_TOUCH_CONTACTS), "re-down of a held id is allowed");
}

#[test]
fn keepalive_resends_held_contacts_as_updates() {
    let mut r = rig();
    r.inj.keepalive();
    assert!(r.touch_frames().is_empty(), "nothing held, nothing sent");
    r.ptr(PointerKind::Touch, Phase::Down, 2, 0.3);
    r.inj.keepalive();
    assert_eq!(r.touch_frames().last(), Some(&vec![(2, TOUCH_MOVE, 300)]));
}

#[test]
fn touch_falls_back_to_mouse_for_the_first_finger() {
    let mut r = rig_with(|s| s.refuse_devices = true);
    r.ptr(PointerKind::Touch, Phase::Down, 1, 0.0);
    assert!(r.sink.sent().is_empty(), "second finger ignored");
    r.ptr(PointerKind::Touch, Phase::Down, 0, 0.0);
    r.ptr(PointerKind::Touch, Phase::Move, 0, 0.1);
    r.ptr(PointerKind::Touch, Phase::Up, 0, 0.1);
    let flags: Vec<_> = r.sink.sent().iter().map(|i| mi(i).dwFlags).collect();
    assert!(flags[1].contains(MOUSEEVENTF_LEFTDOWN));
    assert!(flags.last().is_some_and(|f| f.contains(MOUSEEVENTF_LEFTUP)));
    assert_eq!(r.sink.creates().len(), 1, "refusal is cached");
    assert_eq!(r.warnings(), ["Synthetic touch input unavailable; falling back to mouse."]);
}

#[test]
fn rejected_injection_warns_once() {
    let mut r = rig_with(|s| s.reject_injection = true);
    r.ptr(PointerKind::Touch, Phase::Down, 0, 0.0);
    r.ptr(PointerKind::Touch, Phase::Move, 0, 0.0);
    r.ptr(PointerKind::Pen, Phase::Down, 0, 0.0);
    r.ptr(PointerKind::Pen, Phase::Up, 0, 0.0);
    assert_eq!(r.warnings().len(), 2, "{:?}", r.warnings());
    assert!(r.warnings()[0].contains("Touch injection was rejected"));
    assert!(r.warnings()[1].contains("Pen injection was rejected"));
}

/// (flags without PRIMARY, pressure, button change) of every pen frame.
fn pen_frames(r: &Rig) -> Vec<(u32, u32, i32)> {
    r.sink
        .frames(DeviceKind::Pen)
        .iter()
        .map(|f| pen(&f[0]))
        .map(|p| (p.pointerInfo.pointerFlags.0 & !flag::PRIMARY, p.pressure, p.pointerInfo.ButtonChangeType.0))
        .collect()
}

#[test]
fn pen_stroke_hover_down_move_up() {
    let mut r = rig();
    for phase in [Phase::Hover, Phase::Down, Phase::Move, Phase::Hover, Phase::Up, Phase::Move] {
        r.ptr(PointerKind::Pen, phase, 0, 0.0);
    }
    let (none, down, up) = (POINTER_CHANGE_NONE.0, POINTER_CHANGE_FIRSTBUTTON_DOWN.0, POINTER_CHANGE_FIRSTBUTTON_UP.0);
    assert_eq!(
        pen_frames(&r),
        [
            (flag::UPDATE | flag::INRANGE, 0, none),
            (flag::DOWN | flag::INRANGE | flag::INCONTACT | flag::FIRSTBUTTON, 512, down),
            (flag::UPDATE | flag::INRANGE | flag::INCONTACT | flag::FIRSTBUTTON, 512, none),
            // hover while in contact is dropped
            (flag::UP | flag::INRANGE, 0, up),
            (flag::UPDATE | flag::INRANGE, 0, none),
        ]
    );
    assert_eq!(r.sink.creates(), [(DeviceKind::Pen, PEN_CONTACTS)]);
    let tilt = pen(&r.sink.frames(DeviceKind::Pen)[1][0]);
    assert_eq!((tilt.tiltX, tilt.tiltY), (10, -10));
}

#[test]
fn pen_cancel_in_contact_and_out_of_contact() {
    let mut r = rig();
    r.ptr(PointerKind::Pen, Phase::Down, 0, 0.0);
    r.ptr(PointerKind::Pen, Phase::Cancel, 0, 0.0);
    r.ptr(PointerKind::Pen, Phase::Cancel, 0, 0.0);
    r.ptr(PointerKind::Pen, Phase::Hover, 0, 0.0);
    r.ptr(PointerKind::Pen, Phase::Cancel, 0, 0.0);
    let frames = pen_frames(&r);
    assert_eq!(frames.len(), 4, "cancel while out of range is ignored");
    assert_eq!(frames[1], (flag::UP | flag::CANCELED, 0, POINTER_CHANGE_FIRSTBUTTON_UP.0));
    assert_eq!(frames[3], (flag::UPDATE, 0, POINTER_CHANGE_NONE.0));
}

#[test]
fn pen_up_without_range_is_ignored() {
    let mut r = rig();
    r.ptr(PointerKind::Pen, Phase::Up, 0, 0.0);
    assert!(pen_frames(&r).is_empty());
}

#[test]
fn pen_falls_back_to_mouse_drag() {
    let mut r = rig_with(|s| s.refuse_devices = true);
    for phase in [Phase::Hover, Phase::Down, Phase::Move, Phase::Up, Phase::Move] {
        r.ptr(PointerKind::Pen, phase, 0, 0.0);
    }
    let sent = r.sink.sent();
    let downs = sent.iter().filter(|i| mi(i).dwFlags.contains(MOUSEEVENTF_LEFTDOWN)).count();
    let ups = sent.iter().filter(|i| mi(i).dwFlags.contains(MOUSEEVENTF_LEFTUP)).count();
    assert_eq!((downs, ups), (1, 1));
    assert_eq!(r.warnings(), ["Synthetic pen input unavailable; falling back to mouse."]);
    r.inj.release_all();
    assert!(r.sink.frames(DeviceKind::Pen).is_empty());
}
