//! user32 calls: SendInput, synthetic pointer devices, DPI awareness, virtual-screen metrics and
//! the monitor list. Everything unsafe in the crate lives here.

use std::collections::HashMap;

use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    DISPLAY_DEVICEW, EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
    MONITORINFOEXW,
};
use windows::Win32::UI::Controls::{
    CreateSyntheticPointerDevice, DestroySyntheticPointerDevice, HSYNTHETICPOINTERDEVICE, POINTER_FEEDBACK_DEFAULT,
    POINTER_TYPE_INFO,
};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, SendInput};
use windows::Win32::UI::Input::Pointer::InjectSyntheticPointerInput;
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, MONITORINFOF_PRIMARY, PT_PEN, PT_TOUCH, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};
use windows::core::{BOOL, PCWSTR};

use crate::displays::{Monitor, is_virtual_adapter};
use crate::geometry::Rect;
use crate::sink::{DeviceKind, InputSink};

const UNKNOWN_ADAPTER: &str = "Unknown";

/// Physical-pixel coordinates everywhere; must run before any geometry call.
/// Failure means awareness was already set (manifest or earlier call), so it is ignored.
pub fn enable_dpi_awareness() {
    // SAFETY: plain user32 call with a predefined context constant.
    let _already_set = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// Bounds of the whole virtual desktop in physical pixels.
pub(crate) fn virtual_screen() -> Rect {
    // SAFETY: GetSystemMetrics has no preconditions.
    unsafe {
        Rect {
            left: GetSystemMetrics(SM_XVIRTUALSCREEN),
            top: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    }
}

/// UTF-16 fixed buffer up to its first NUL.
fn from_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// GDI device name (lowercased) -> adapter description.
fn adapter_names() -> HashMap<String, String> {
    let mut names = HashMap::new();
    for index in 0.. {
        let mut dd = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
        // SAFETY: `dd` is a properly sized, writable DISPLAY_DEVICEW with `cb` set.
        if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), index, &mut dd, 0) }.as_bool() {
            break;
        }
        names.insert(from_wide(&dd.DeviceName).to_lowercase(), from_wide(&dd.DeviceString));
    }
    names
}

struct EnumState<'a> {
    adapters: &'a HashMap<String, String>,
    monitors: Vec<Monitor>,
}

unsafe extern "system" fn on_monitor(hmonitor: HMONITOR, _hdc: HDC, _clip: *mut RECT, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the `&mut EnumState` passed by `list_monitors`, alive for the whole enumeration.
    let state = unsafe { &mut *(data.0 as *mut EnumState) };
    let mut info = MONITORINFOEXW { monitorInfo: MONITORINFO { cbSize: size_of::<MONITORINFOEXW>() as u32, ..Default::default() }, ..Default::default() };
    // SAFETY: MONITORINFOEXW starts with MONITORINFO and `cbSize` tells user32 the extended size.
    if unsafe { GetMonitorInfoW(hmonitor, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) }.as_bool() {
        let r = info.monitorInfo.rcMonitor;
        let name = from_wide(&info.szDevice);
        let adapter = state.adapters.get(&name.to_lowercase()).cloned().unwrap_or_else(|| UNKNOWN_ADAPTER.to_owned());
        state.monitors.push(Monitor {
            is_primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            is_virtual: is_virtual_adapter(&adapter),
            rect: Rect { left: r.left, top: r.top, width: r.right - r.left, height: r.bottom - r.top },
            name,
            adapter,
        });
    }
    BOOL::from(true)
}

/// Active monitors in EnumDisplayMonitors order (what `MonitorChoice::Index` uses); empty on failure.
pub fn list_monitors() -> Vec<Monitor> {
    let adapters = adapter_names();
    let mut state = EnumState { adapters: &adapters, monitors: Vec::new() };
    // SAFETY: the callback only touches `state`, which outlives this synchronous call.
    let ok = unsafe { EnumDisplayMonitors(None, None, Some(on_monitor), LPARAM(&mut state as *mut EnumState as isize)) };
    if ok.as_bool() { state.monitors } else { Vec::new() }
}

/// The real desktop. Owns the synthetic pointer devices and destroys them on drop, so Windows
/// lifts any contact still down if the input thread ends.
#[derive(Default)]
pub(crate) struct Win32Sink {
    touch: Option<HSYNTHETICPOINTERDEVICE>,
    pen: Option<HSYNTHETICPOINTERDEVICE>,
}

impl Win32Sink {
    fn slot(&mut self, kind: DeviceKind) -> &mut Option<HSYNTHETICPOINTERDEVICE> {
        match kind {
            DeviceKind::Touch => &mut self.touch,
            DeviceKind::Pen => &mut self.pen,
        }
    }
}

impl InputSink for Win32Sink {
    fn virtual_screen(&mut self) -> Rect {
        virtual_screen()
    }

    fn send_input(&mut self, inputs: &[INPUT]) {
        if inputs.is_empty() {
            return;
        }
        // SAFETY: `inputs` are fully initialised INPUT records and the size matches the SDK struct.
        // A short count means UIPI blocked us (elevated foreground window); nothing to retry.
        let _sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    }

    fn create_pointer_device(&mut self, kind: DeviceKind, max_contacts: u32) -> bool {
        let pointer_type = match kind {
            DeviceKind::Touch => PT_TOUCH,
            DeviceKind::Pen => PT_PEN,
        };
        // SAFETY: plain user32 call; the handle is owned by `self` and destroyed in Drop.
        match unsafe { CreateSyntheticPointerDevice(pointer_type, max_contacts, POINTER_FEEDBACK_DEFAULT) } {
            Ok(device) => {
                *self.slot(kind) = Some(device);
                true
            }
            Err(_) => false,
        }
    }

    fn inject_pointer(&mut self, kind: DeviceKind, records: &[POINTER_TYPE_INFO]) -> bool {
        let Some(device) = *self.slot(kind) else { return false };
        // SAFETY: `device` is a live handle from CreateSyntheticPointerDevice; records are initialised.
        unsafe { InjectSyntheticPointerInput(device, records) }.is_ok()
    }
}

impl Drop for Win32Sink {
    fn drop(&mut self) {
        for device in [self.touch.take(), self.pen.take()].into_iter().flatten() {
            // SAFETY: each handle came from CreateSyntheticPointerDevice and is destroyed once.
            unsafe { DestroySyntheticPointerDevice(device) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Read-only OS calls only: nothing here injects input into the live desktop.

    #[test]
    fn lists_at_least_one_monitor_with_a_real_rect() {
        let monitors = list_monitors();
        assert!(!monitors.is_empty());
        assert!(monitors.iter().all(|m| !m.rect.is_empty() && !m.name.is_empty()));
        assert_eq!(monitors.iter().filter(|m| m.is_primary).count(), 1);
    }

    #[test]
    fn virtual_screen_covers_every_monitor() {
        enable_dpi_awareness();
        let vs = virtual_screen();
        assert!(!vs.is_empty());
        for m in list_monitors() {
            assert!(m.rect.left >= vs.left && m.rect.top >= vs.top);
            assert!(m.rect.left + m.rect.width <= vs.left + vs.width);
            assert!(m.rect.top + m.rect.height <= vs.top + vs.height);
        }
    }

    #[test]
    fn from_wide_stops_at_nul() {
        let buf: Vec<u16> = "ab\0cd".encode_utf16().collect();
        assert_eq!(from_wide(&buf), "ab");
        assert_eq!(from_wide(&[0x61, 0x62]), "ab");
    }

    #[test]
    fn sink_without_device_rejects_injection_without_calling_windows() {
        let mut sink = Win32Sink::default();
        assert!(!sink.inject_pointer(DeviceKind::Touch, &[]));
        assert!(!sink.inject_pointer(DeviceKind::Pen, &[]));
    }
}
