//! Shell_NotifyIconW wrappers. On Windows 10/11 a balloon shows as a toast, no app registration needed.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_LARGE_ICON, NIIF_USER, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NIM_SETVERSION, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::HICON;

use crate::wide::copy_truncated;

/// Our only icon's id within the tray window.
const ICON_ID: u32 = 1;

fn base(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: ICON_ID, ..Default::default() }
}

fn call(message: windows::Win32::UI::Shell::NOTIFY_ICON_MESSAGE, data: &NOTIFYICONDATAW) -> bool {
    // SAFETY: `data` is a fully initialised NOTIFYICONDATAW with `cbSize` set.
    unsafe { Shell_NotifyIconW(message, data) }.as_bool()
}

/// Adds the icon with version 4 callbacks; fails until the taskbar is up (e.g. right after sign-in).
pub(crate) fn add(hwnd: HWND, callback_message: u32, icon: HICON, tip: &str) -> bool {
    let mut data = base(hwnd);
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    data.uCallbackMessage = callback_message;
    data.hIcon = icon;
    copy_truncated(&mut data.szTip, tip);
    if !call(NIM_ADD, &data) {
        return false;
    }
    data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    call(NIM_SETVERSION, &data)
}

pub(crate) fn set_tip(hwnd: HWND, tip: &str) -> bool {
    let mut data = base(hwnd);
    data.uFlags = NIF_TIP | NIF_SHOWTIP;
    copy_truncated(&mut data.szTip, tip);
    call(NIM_MODIFY, &data)
}

/// A balloon/toast with our logo as its large icon.
pub(crate) fn balloon(hwnd: HWND, title: &str, text: &str, large_icon: Option<HICON>) -> bool {
    let mut data = base(hwnd);
    data.uFlags = NIF_INFO;
    copy_truncated(&mut data.szInfoTitle, title);
    // An empty szInfo means "remove the balloon", so a title-only notice needs some text.
    copy_truncated(&mut data.szInfo, if text.trim().is_empty() { title } else { text });
    if let Some(icon) = large_icon {
        data.dwInfoFlags = NIIF_USER | NIIF_LARGE_ICON;
        data.hBalloonIcon = icon;
    }
    call(NIM_MODIFY, &data)
}

pub(crate) fn remove(hwnd: HWND) {
    let _ = call(NIM_DELETE, &base(hwnd));
}
