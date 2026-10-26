//! "Copy link": Unicode text onto the clipboard.

use windows::Win32::Foundation::{GlobalFree, HANDLE};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
use windows::core::w;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;

use crate::wide::to_wide;

/// Keeps the copy out of clipboard history and cloud sync: it holds a code or Wi-Fi password.
fn keep_private() {
    for name in [w!("ExcludeClipboardContentFromMonitorProcessing"), w!("CanIncludeInClipboardHistory")] {
        // SAFETY: registering a named format; a zero DWORD means "no" for the second one.
        unsafe {
            let format = RegisterClipboardFormatW(name);
            let Ok(memory) = GlobalAlloc(GMEM_MOVEABLE, size_of::<u32>()) else { continue };
            let target = GlobalLock(memory);
            if !target.is_null() {
                *target.cast::<u32>() = 0;
            }
            let _ = GlobalUnlock(memory);
            if SetClipboardData(format, Some(HANDLE(memory.0))).is_err() {
                let _ = GlobalFree(Some(memory));
            }
        }
    }
}

/// Copies `text` as Unicode; the clipboard owner is the active Xiro window.
pub(crate) fn copy(text: &str) -> Result<(), String> {
    let wide = to_wide(text);
    let bytes = wide.len() * size_of::<u16>();
    // SAFETY: opened and closed here; the clipboard owns the memory on success, we free it on failure.
    unsafe {
        let owner = GetActiveWindow();
        OpenClipboard(Some(owner).filter(|h| !h.is_invalid())).map_err(|e| e.message())?;
        let result = (|| {
            EmptyClipboard().map_err(|e| e.message())?;
            let memory = GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(|e| e.message())?;
            let target = GlobalLock(memory);
            if target.is_null() {
                let _ = GlobalFree(Some(memory));
                return Err("GlobalLock failed".to_string());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), target.cast::<u16>(), wide.len());
            let _ = GlobalUnlock(memory);
            if let Err(e) = SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(memory.0))) {
                let _ = GlobalFree(Some(memory));
                return Err(e.message());
            }
            keep_private();
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}
