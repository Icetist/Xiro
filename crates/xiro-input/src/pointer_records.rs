//! Pure builders for `POINTER_TYPE_INFO` records fed to `InjectSyntheticPointerInput`.

use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Controls::{POINTER_TYPE_INFO, POINTER_TYPE_INFO_0};
use windows::Win32::UI::Input::Pointer::{
    POINTER_BUTTON_CHANGE_TYPE, POINTER_CHANGE_FIRSTBUTTON_DOWN, POINTER_CHANGE_FIRSTBUTTON_UP, POINTER_CHANGE_NONE,
    POINTER_FLAG_CANCELED, POINTER_FLAG_DOWN, POINTER_FLAG_FIRSTBUTTON, POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE,
    POINTER_FLAG_PRIMARY, POINTER_FLAG_UP, POINTER_FLAG_UPDATE, POINTER_FLAGS, POINTER_INFO, POINTER_PEN_INFO,
    POINTER_TOUCH_INFO,
};
use windows::Win32::UI::WindowsAndMessaging::{
    PEN_MASK_PRESSURE, PEN_MASK_TILT_X, PEN_MASK_TILT_Y, POINTER_INPUT_TYPE, PT_PEN, PT_TOUCH,
};

use crate::geometry::{ScreenPoint, js_round};

/// Plain-`u32` pointer flags for simple bit arithmetic in the injector.
pub mod flag {
    use super::*;
    pub const INRANGE: u32 = POINTER_FLAG_INRANGE.0;
    pub const INCONTACT: u32 = POINTER_FLAG_INCONTACT.0;
    pub const FIRSTBUTTON: u32 = POINTER_FLAG_FIRSTBUTTON.0;
    pub const PRIMARY: u32 = POINTER_FLAG_PRIMARY.0;
    pub const CANCELED: u32 = POINTER_FLAG_CANCELED.0;
    pub const DOWN: u32 = POINTER_FLAG_DOWN.0;
    pub const UPDATE: u32 = POINTER_FLAG_UPDATE.0;
    pub const UP: u32 = POINTER_FLAG_UP.0;
}

/// Pen barrel-button transition carried by a pen record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonChange {
    None,
    FirstDown,
    FirstUp,
}

impl ButtonChange {
    fn to_win32(self) -> POINTER_BUTTON_CHANGE_TYPE {
        match self {
            Self::None => POINTER_CHANGE_NONE,
            Self::FirstDown => POINTER_CHANGE_FIRSTBUTTON_DOWN,
            Self::FirstUp => POINTER_CHANGE_FIRSTBUTTON_UP,
        }
    }
}

/// One contact in a touch frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Contact {
    pub id: u8,
    pub point: ScreenPoint,
    pub flags: u32,
}

/// One pen sample, already resolved to flags by the injector.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PenSample {
    pub point: ScreenPoint,
    pub flags: u32,
    /// Normalised `[0, 1]`; clamped here.
    pub pressure: f64,
    pub tilt_x: i32,
    pub tilt_y: i32,
    pub button_change: ButtonChange,
}

/// Windows Ink pressure range is 0..=1024.
const PEN_PRESSURE_MAX: f64 = 1024.0;

fn pointer_info(pointer_type: POINTER_INPUT_TYPE, id: u8, flags: u32, point: ScreenPoint, button_change: ButtonChange) -> POINTER_INFO {
    let px = POINT { x: point.x, y: point.y };
    POINTER_INFO {
        pointerType: pointer_type,
        pointerId: u32::from(id),
        pointerFlags: POINTER_FLAGS(flags),
        ptPixelLocation: px,
        ptPixelLocationRaw: px,
        ButtonChangeType: button_change.to_win32(),
        ..Default::default()
    }
}

/// One touch frame: Windows requires every active contact in every frame.
pub fn touch_frame(contacts: &[Contact]) -> Vec<POINTER_TYPE_INFO> {
    contacts
        .iter()
        .map(|c| POINTER_TYPE_INFO {
            r#type: PT_TOUCH,
            Anonymous: POINTER_TYPE_INFO_0 {
                touchInfo: POINTER_TOUCH_INFO { pointerInfo: pointer_info(PT_TOUCH, c.id, c.flags, c.point, ButtonChange::None), ..Default::default() },
            },
        })
        .collect()
}

/// The single Apple Pencil contact, always the primary pointer with pressure and tilt valid.
pub fn pen_record(s: &PenSample) -> POINTER_TYPE_INFO {
    POINTER_TYPE_INFO {
        r#type: PT_PEN,
        Anonymous: POINTER_TYPE_INFO_0 {
            penInfo: POINTER_PEN_INFO {
                pointerInfo: pointer_info(PT_PEN, 0, s.flags | flag::PRIMARY, s.point, s.button_change),
                penFlags: 0,
                penMask: PEN_MASK_PRESSURE | PEN_MASK_TILT_X | PEN_MASK_TILT_Y,
                pressure: js_round(s.pressure.clamp(0.0, 1.0) * PEN_PRESSURE_MAX) as u32,
                rotation: 0,
                tiltX: s.tilt_x,
                tiltY: s.tilt_y,
            },
        },
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn touch(r: &POINTER_TYPE_INFO) -> POINTER_TOUCH_INFO {
        assert_eq!(r.r#type, PT_TOUCH);
        // SAFETY: the type tag says the union holds `touchInfo`.
        unsafe { r.Anonymous.touchInfo }
    }

    pub(crate) fn pen(r: &POINTER_TYPE_INFO) -> POINTER_PEN_INFO {
        assert_eq!(r.r#type, PT_PEN);
        // SAFETY: the type tag says the union holds `penInfo`.
        unsafe { r.Anonymous.penInfo }
    }

    #[test]
    fn pointer_type_info_matches_the_x64_sdk_size() {
        assert_eq!(size_of::<POINTER_TYPE_INFO>(), 152);
        assert_eq!(size_of::<POINTER_INFO>(), 96);
    }

    #[test]
    fn touch_frames_include_every_contact() {
        let recs = touch_frame(&[
            Contact { id: 0, point: ScreenPoint { x: 1, y: 2 }, flags: flag::DOWN },
            Contact { id: 1, point: ScreenPoint { x: 3, y: 4 }, flags: flag::UPDATE },
        ]);
        assert_eq!(recs.len(), 2);
        let first = touch(&recs[0]).pointerInfo;
        assert_eq!(touch(&recs[1]).pointerInfo.pointerId, 1);
        assert_eq!(first.ptPixelLocation, POINT { x: 1, y: 2 });
        assert_eq!(first.ptPixelLocationRaw, POINT { x: 1, y: 2 });
        assert_eq!(first.pointerType, PT_TOUCH);
        assert_eq!(first.pointerFlags, POINTER_FLAG_DOWN);
    }

    #[test]
    fn pen_records_scale_pressure_and_mark_primary() {
        let s = PenSample { point: ScreenPoint { x: 5, y: 6 }, flags: flag::INRANGE, pressure: 0.5, tilt_x: 10, tilt_y: -20, button_change: ButtonChange::None };
        let p = pen(&pen_record(&s));
        assert_eq!(p.pressure, 512);
        assert_eq!((p.tiltX, p.tiltY), (10, -20));
        assert!(p.pointerInfo.pointerFlags.contains(POINTER_FLAG_PRIMARY | POINTER_FLAG_INRANGE));
        assert_eq!(p.pointerInfo.pointerType, PT_PEN);
        assert_eq!(p.penMask, PEN_MASK_PRESSURE | PEN_MASK_TILT_X | PEN_MASK_TILT_Y);
    }

    #[test]
    fn pen_pressure_is_clamped_and_button_change_carried() {
        let mut s = PenSample { point: ScreenPoint::default(), flags: 0, pressure: 7.0, tilt_x: 0, tilt_y: 0, button_change: ButtonChange::FirstDown };
        let p = pen(&pen_record(&s));
        assert_eq!(p.pressure, 1024);
        assert_eq!(p.pointerInfo.ButtonChangeType, POINTER_CHANGE_FIRSTBUTTON_DOWN);
        s.pressure = -1.0;
        s.button_change = ButtonChange::FirstUp;
        let p = pen(&pen_record(&s));
        assert_eq!(p.pressure, 0);
        assert_eq!(p.pointerInfo.ButtonChangeType, POINTER_CHANGE_FIRSTBUTTON_UP);
    }
}
