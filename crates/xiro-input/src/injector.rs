//! Client input -> Windows multi-touch, Windows Ink pen, SendInput mouse and scan codes.
//! Pure state machine over an [`InputSink`]; [`crate::handle`] drives the 50 ms touch keepalive.

use crate::geometry::{Rect, ScreenPoint, to_screen_point};
use crate::keymap::scan_code_for;
use crate::pointer_records::{ButtonChange, Contact, PenSample, flag, pen_record, touch_frame};
use crate::protocol::{Phase, Pointer, PointerKind, Wheel};
use crate::records::{mouse_inputs, scan_key_inputs, unicode_inputs, wheel_inputs};
use crate::sink::{DeviceKind, InputSink};

pub(crate) const MAX_TOUCH_CONTACTS: usize = 10;
/// Text is truncated to this many UTF-16 units.
const MAX_TEXT_LENGTH: usize = 256;
/// Only one Apple Pencil exists per iPad.
const PEN_CONTACTS: u32 = 1;

const TOUCH_DOWN: u32 = flag::DOWN | flag::INRANGE | flag::INCONTACT;
const TOUCH_MOVE: u32 = flag::UPDATE | flag::INRANGE | flag::INCONTACT;

/// `PointerEvent.buttons` value for "left button held" in the mouse fallbacks.
const LEFT_BUTTON: u8 = 1;

/// The fields of a pen sample the pen state machine needs (also synthesised by `release_all`).
#[derive(Clone, Copy)]
struct PenInput {
    phase: Phase,
    pressure: f64,
    tilt_x: i32,
    tilt_y: i32,
}

#[derive(Default)]
struct PenState {
    in_contact: bool,
    in_range: bool,
    point: Option<ScreenPoint>,
}

pub(crate) struct Injector<S: InputSink> {
    sink: S,
    cmd_as_ctrl: bool,
    target: Rect,
    vs: Rect,
    mouse_buttons: u8,
    mouse_point: Option<ScreenPoint>,
    /// In press order, so `release_all` lifts keys in that order.
    pressed_keys: Vec<String>,
    /// In insertion order; at most [`MAX_TOUCH_CONTACTS`] entries.
    touches: Vec<(u8, ScreenPoint)>,
    /// `None` until first use, then whether Windows gave us the device.
    touch_device: Option<bool>,
    pen_device: Option<bool>,
    pen: PenState,
    warned: Vec<&'static str>,
    on_warning: Box<dyn Fn(String)>,
}

impl<S: InputSink> Injector<S> {
    pub(crate) fn new(mut sink: S, cmd_as_ctrl: bool, on_warning: Box<dyn Fn(String)>) -> Self {
        let vs = sink.virtual_screen();
        Self {
            sink,
            cmd_as_ctrl,
            target: Rect::default(),
            vs,
            mouse_buttons: 0,
            mouse_point: None,
            pressed_keys: Vec::new(),
            touches: Vec::new(),
            touch_device: None,
            pen_device: None,
            pen: PenState::default(),
            warned: Vec::new(),
            on_warning,
        }
    }

    pub(crate) fn set_target(&mut self, rect: Rect) {
        self.target = rect;
    }

    /// Display layout may change while running (e.g. resolution switch).
    pub(crate) fn refresh_layout(&mut self) {
        self.vs = self.sink.virtual_screen();
    }

    /// Touches are held, so the caller must keep calling [`Self::keepalive`].
    pub(crate) fn is_touching(&self) -> bool {
        !self.touches.is_empty()
    }

    /// Windows cancels touch contacts that go quiet; re-send every held contact unchanged.
    pub(crate) fn keepalive(&mut self) {
        self.inject_touch(None, 0);
    }

    fn screen_point(&mut self, x: f64, y: f64) -> Option<ScreenPoint> {
        if self.target.is_empty() {
            self.warn_once("no-target", "Pointer input ignored: no target monitor has been set.");
            return None;
        }
        Some(to_screen_point(x, y, self.target))
    }

    pub(crate) fn pointer(&mut self, p: &Pointer) {
        let Some(point) = self.screen_point(p.x, p.y) else { return };
        match p.kind {
            PointerKind::Mouse => self.mouse(point, p.buttons),
            PointerKind::Pen => self.pen(point, PenInput { phase: p.phase, pressure: p.pressure, tilt_x: p.tilt_x, tilt_y: p.tilt_y }),
            PointerKind::Touch => self.touch(point, p.id, p.phase),
        }
    }

    pub(crate) fn wheel(&mut self, w: &Wheel) {
        let Some(point) = self.screen_point(w.x, w.y) else { return };
        self.sink.send_input(&wheel_inputs(point, w.dx, w.dy, self.vs));
    }

    pub(crate) fn key(&mut self, code: &str, is_down: bool) {
        let Some(sc) = scan_code_for(code, self.cmd_as_ctrl) else { return };
        let held = self.pressed_keys.iter().position(|k| k == code);
        match (is_down, held) {
            (true, None) => self.pressed_keys.push(code.to_owned()),
            (false, Some(i)) => {
                self.pressed_keys.remove(i);
            }
            _ => {}
        }
        self.sink.send_input(&scan_key_inputs(sc, is_down));
    }

    pub(crate) fn text(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.sink.send_input(&unicode_inputs(s.encode_utf16().take(MAX_TEXT_LENGTH)));
    }

    /// Lift everything so a dropped connection never leaves a key or button stuck.
    pub(crate) fn release_all(&mut self) {
        for code in std::mem::take(&mut self.pressed_keys) {
            self.key(&code, false);
        }
        // Release where the pointer is, so an interrupted drag drops in place.
        if let (true, Some(point)) = (self.mouse_buttons != 0, self.mouse_point) {
            self.mouse(point, 0);
        }
        let ids: Vec<u8> = self.touches.iter().map(|(id, _)| *id).collect();
        for id in ids {
            self.touch_end(id, true);
        }
        if let (true, Some(point)) = (self.pen.in_range, self.pen.point) {
            self.pen(point, PenInput { phase: Phase::Cancel, pressure: 0.0, tilt_x: 0, tilt_y: 0 });
        }
    }

    fn warn_once(&mut self, key: &'static str, message: &str) {
        if self.warned.contains(&key) {
            return;
        }
        self.warned.push(key);
        (self.on_warning)(message.to_owned());
    }

    fn mouse(&mut self, point: ScreenPoint, buttons: u8) {
        self.sink.send_input(&mouse_inputs(point, buttons, self.mouse_buttons, self.vs));
        self.mouse_buttons = buttons;
        self.mouse_point = Some(point);
    }

    /// Lazily creates the synthetic device once; a refusal is remembered and warned about once.
    fn device(&mut self, kind: DeviceKind) -> bool {
        let cached = match kind {
            DeviceKind::Touch => self.touch_device,
            DeviceKind::Pen => self.pen_device,
        };
        if let Some(ok) = cached {
            return ok;
        }
        let (max, key, what) = match kind {
            DeviceKind::Touch => (MAX_TOUCH_CONTACTS as u32, "touchDevice", "touch"),
            DeviceKind::Pen => (PEN_CONTACTS, "penDevice", "pen"),
        };
        let ok = self.sink.create_pointer_device(kind, max);
        match kind {
            DeviceKind::Touch => self.touch_device = Some(ok),
            DeviceKind::Pen => self.pen_device = Some(ok),
        }
        if !ok {
            self.warn_once(key, &format!("Synthetic {what} input unavailable; falling back to mouse."));
        }
        ok
    }

    fn touch_index(&self, id: u8) -> Option<usize> {
        self.touches.iter().position(|(t, _)| *t == id)
    }

    fn touch(&mut self, point: ScreenPoint, id: u8, phase: Phase) {
        if !self.device(DeviceKind::Touch) {
            // Mouse fallback for the first finger only.
            if id == 0 {
                self.mouse(point, if phase.is_end() { 0 } else { LEFT_BUTTON });
            }
            return;
        }
        let held = self.touch_index(id);
        match phase {
            Phase::Down => {
                match held {
                    Some(i) => self.touches[i].1 = point,
                    None if self.touches.len() >= MAX_TOUCH_CONTACTS => return,
                    None => self.touches.push((id, point)),
                }
                self.inject_touch(Some(id), TOUCH_DOWN);
            }
            Phase::Move => {
                let Some(i) = held else { return };
                self.touches[i].1 = point;
                self.inject_touch(None, 0);
            }
            Phase::Up | Phase::Cancel => {
                if let Some(i) = held {
                    self.touches[i].1 = point;
                }
                self.touch_end(id, phase == Phase::Cancel);
            }
            Phase::Hover => {}
        }
    }

    fn touch_end(&mut self, id: u8, canceled: bool) {
        let Some(i) = self.touch_index(id) else { return };
        self.inject_touch(Some(id), flag::UP | if canceled { flag::CANCELED } else { 0 });
        self.touches.remove(i);
    }

    /// Inject a frame with every contact; `changed` gets `changed_flags`, the rest are updates.
    fn inject_touch(&mut self, changed: Option<u8>, changed_flags: u32) {
        if self.touches.is_empty() {
            return;
        }
        let contacts: Vec<Contact> = self
            .touches
            .iter()
            .map(|&(id, point)| Contact { id, point, flags: if Some(id) == changed { changed_flags } else { TOUCH_MOVE } })
            .collect();
        if !self.sink.inject_pointer(DeviceKind::Touch, &touch_frame(&contacts)) {
            self.warn_once("touch-inject", "Touch injection was rejected by Windows (is the target window elevated?)");
        }
    }

    fn pen(&mut self, point: ScreenPoint, p: PenInput) {
        if !self.device(DeviceKind::Pen) {
            let is_contact = p.phase == Phase::Down || (p.phase == Phase::Move && self.pen.in_contact);
            self.pen.in_contact = is_contact;
            self.mouse(point, if is_contact { LEFT_BUTTON } else { 0 });
            return;
        }
        let Some((flags, button_change)) = self.pen_transition(p.phase) else { return };
        self.pen.in_range = flags & flag::INRANGE != 0;
        self.pen.point = Some(point);
        let pressure = if self.pen.in_contact { p.pressure } else { 0.0 };
        let record = pen_record(&PenSample { point, flags, pressure, tilt_x: p.tilt_x, tilt_y: p.tilt_y, button_change });
        if !self.sink.inject_pointer(DeviceKind::Pen, &[record]) {
            self.warn_once("pen-inject", "Pen injection was rejected by Windows.");
        }
    }

    /// Pointer flags for the next pen frame, updating `in_contact`; `None` means send nothing.
    fn pen_transition(&mut self, phase: Phase) -> Option<(u32, ButtonChange)> {
        let in_contact = self.pen.in_contact;
        let contact_flags = if in_contact { flag::INCONTACT | flag::FIRSTBUTTON } else { 0 };
        match phase {
            Phase::Down => {
                self.pen.in_contact = true;
                Some((flag::DOWN | flag::INRANGE | flag::INCONTACT | flag::FIRSTBUTTON, ButtonChange::FirstDown))
            }
            Phase::Move => Some((flag::UPDATE | flag::INRANGE | contact_flags, ButtonChange::None)),
            // A hover report while touching is stale ordering from the client; ignore it.
            Phase::Hover if in_contact => None,
            Phase::Hover => Some((flag::UPDATE | flag::INRANGE, ButtonChange::None)),
            Phase::Up | Phase::Cancel => {
                if !in_contact && !self.pen.in_range {
                    return None;
                }
                let mut flags = if in_contact { flag::UP | flag::INRANGE } else { flag::UPDATE };
                if phase == Phase::Cancel {
                    flags = (flags & !flag::INRANGE) | if in_contact { flag::CANCELED } else { 0 };
                }
                self.pen.in_contact = false;
                Some((flags, if in_contact { ButtonChange::FirstUp } else { ButtonChange::None }))
            }
        }
    }
}

#[cfg(test)]
#[path = "injector_tests.rs"]
mod tests;
