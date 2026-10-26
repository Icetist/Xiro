//! Owner-less modal message boxes for install-time questions, with the modern control look.

use windows::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND,
    MB_TOPMOST, MB_YESNO,
    MESSAGEBOX_STYLE, MessageBoxW,
};
use windows::core::PCWSTR;

use crate::styles::with_visual_styles;
use crate::wide::to_wide;

fn show(title: &str, text: &str, style: MESSAGEBOX_STYLE) -> windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_RESULT {
    let (title, text) = (to_wide(title), to_wide(text));
    // Foreground + topmost so the box doesn't open behind Explorer or the browser.
    let style = style | MB_SETFOREGROUND | MB_TOPMOST;
    with_visual_styles(|| {
        // SAFETY: both strings are NUL-terminated and outlive the modal call.
        unsafe { MessageBoxW(None, PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), style) }
    })
}

pub(crate) fn ask(title: &str, text: &str) -> bool {
    show(title, text, MB_YESNO | MB_ICONQUESTION) == IDYES
}

/// Like `ask`, but No is the default so a reflexive Enter doesn't say yes.
pub(crate) fn confirm(title: &str, text: &str) -> bool {
    show(title, text, MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) == IDYES
}

pub(crate) fn message(title: &str, text: &str, is_error: bool) {
    let icon = if is_error { MB_ICONERROR } else { MB_ICONINFORMATION };
    show(title, text, MB_OK | icon);
}
