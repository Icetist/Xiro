//! Removes the "Apps & features" entry written by legacy installs.

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RegDeleteTreeW};
use windows::core::{PCWSTR, w};

const UNINSTALL_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Xiro");

fn check(status: WIN32_ERROR, what: &str) -> Result<(), String> {
    status.ok().map_err(|e| format!("{what}: {}", e.message()))
}

/// Removes the entry; a missing entry is not an error.
pub(crate) fn remove() -> Result<(), String> {
    // SAFETY: deletes only our own key under HKCU.
    let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, UNINSTALL_KEY) };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    check(status, "could not remove the uninstall entry")
}
