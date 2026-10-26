//! Display modes via classic GDI: CCD switches monitors on and off, but mode and
//! position are simplest to set per monitor here.

use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{
    CDS_NORESET, CDS_TYPE, CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DEVMODE_FIELD_FLAGS, DEVMODEW, DM_BITSPERPEL,
    DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH, DM_POSITION, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_FLAGS,
    EnumDisplaySettingsExW, GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
};
use windows::core::{HSTRING, PCWSTR};

use crate::Mode;

/// A monitor's mode and where its top-left corner sits on the desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CurrentMode {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
    pub x: i32,
    pub y: i32,
}

/// A desktop position (top-left corner).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Position {
    pub x: i32,
    pub y: i32,
}

/// The primary monitor's desktop rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bounds {
    pub left: i32,
    pub top: i32,
    pub width: i32,
}

pub(crate) const DISP_CHANGE_SUCCESSFUL: i32 = 0;
/// DISP_CHANGE_BADMODE: the monitor does not offer this mode.
pub(crate) const DISP_CHANGE_BADMODE: i32 = -2;
const BITS_PER_PIXEL: u32 = 32;

fn devmode() -> DEVMODEW {
    DEVMODEW {
        // DEVMODEW is 220 bytes; the field is a u16 by definition.
        dmSize: u16::try_from(size_of::<DEVMODEW>()).unwrap_or(u16::MAX),
        ..Default::default()
    }
}

pub(crate) fn current_mode(gdi_name: &str) -> Option<CurrentMode> {
    let name = HSTRING::from(gdi_name);
    let mut dm = devmode();
    // SAFETY: `dm` is a properly sized DEVMODEW that outlives the call.
    let ok = unsafe { EnumDisplaySettingsExW(&name, ENUM_CURRENT_SETTINGS, &mut dm, ENUM_DISPLAY_SETTINGS_FLAGS(0)) };
    if !ok.as_bool() {
        return None;
    }
    // SAFETY: for displays the union holds the position variant.
    let position = unsafe { dm.Anonymous1.Anonymous2.dmPosition };
    Some(CurrentMode { width: dm.dmPelsWidth, height: dm.dmPelsHeight, hz: dm.dmDisplayFrequency, x: position.x, y: position.y })
}

/// Staged in the registry, then applied in one go so the position sticks with the mode.
/// Returns the DISP_CHANGE_* code.
pub(crate) fn set_mode(gdi_name: &str, mode: Mode, position: Option<Position>) -> i32 {
    let name = HSTRING::from(gdi_name);
    let mut dm = devmode();
    let mut fields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY | DM_BITSPERPEL;
    if let Some(p) = position {
        fields |= DM_POSITION;
        dm.Anonymous1.Anonymous2.dmPosition.x = p.x;
        dm.Anonymous1.Anonymous2.dmPosition.y = p.y;
    }
    dm.dmFields = DEVMODE_FIELD_FLAGS(fields.0);
    dm.dmPelsWidth = mode.width;
    dm.dmPelsHeight = mode.height;
    dm.dmDisplayFrequency = mode.hz;
    dm.dmBitsPerPel = BITS_PER_PIXEL;
    // SAFETY: `dm` and `name` outlive the call.
    let staged = unsafe { ChangeDisplaySettingsExW(&name, Some(&dm), None, CDS_UPDATEREGISTRY | CDS_NORESET, None) };
    if staged.0 != DISP_CHANGE_SUCCESSFUL {
        return staged.0;
    }
    // SAFETY: a null device and mode apply everything staged above.
    unsafe { ChangeDisplaySettingsExW(PCWSTR::null(), None, None, CDS_TYPE(0), None) }.0
}

/// The primary monitor's bounds (it always contains the desktop origin).
pub(crate) fn primary_bounds() -> Option<Bounds> {
    // SAFETY: plain value arguments.
    let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    let mut info = MONITORINFO { cbSize: u32::try_from(size_of::<MONITORINFO>()).unwrap_or(u32::MAX), ..Default::default() };
    // SAFETY: `info` is sized via cbSize and outlives the call.
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return None;
    }
    let r = info.rcMonitor;
    Some(Bounds { left: r.left, top: r.top, width: r.right - r.left })
}
