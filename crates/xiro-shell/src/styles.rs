//! Common Controls v6 for message boxes, borrowed from shell32.dll's manifest (id 124):
//! this build embeds no manifest of its own.

use std::sync::OnceLock;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::ApplicationInstallationAndServicing::{
    ACTCTXW, ActivateActCtx, CreateActCtxW, DeactivateActCtx,
};
use windows::core::PCWSTR;

use crate::wide::to_wide;

/// From WindowsProgramming; not worth the feature for two constants.
const ACTCTX_FLAG_ASSEMBLY_DIRECTORY_VALID: u32 = 0x4;
const ACTCTX_FLAG_RESOURCE_NAME_VALID: u32 = 0x8;
/// Resource id of shell32.dll's Common Controls v6 manifest.
const SHELL32_MANIFEST_ID: usize = 124;

/// The activation context as a raw handle value (0 = unavailable, classic look).
static CONTEXT: OnceLock<isize> = OnceLock::new();

/// System32, where shell32.dll and the manifest's dependencies are resolved from.
fn system_dir() -> std::path::PathBuf {
    let windows = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    std::path::Path::new(&windows).join("System32")
}

fn context() -> Option<HANDLE> {
    let raw = *CONTEXT.get_or_init(|| {
        // Windows only builds the context from a full path and with the assembly directory given.
        let dir = to_wide(&system_dir().display().to_string());
        let shell32 = to_wide(&system_dir().join("shell32.dll").display().to_string());
        let request = ACTCTXW {
            cbSize: size_of::<ACTCTXW>() as u32,
            dwFlags: ACTCTX_FLAG_RESOURCE_NAME_VALID | ACTCTX_FLAG_ASSEMBLY_DIRECTORY_VALID,
            lpSource: PCWSTR(shell32.as_ptr()),
            lpAssemblyDirectory: PCWSTR(dir.as_ptr()),
            lpResourceName: PCWSTR(SHELL32_MANIFEST_ID as *const u16), // MAKEINTRESOURCE
            ..Default::default()
        };
        // SAFETY: `request` is fully initialised; the handle is kept for the process lifetime.
        unsafe { CreateActCtxW(&request) }.map(|h| h.0 as isize).unwrap_or(0)
    });
    (raw != 0).then(|| HANDLE(raw as *mut _))
}

/// Runs `f` with Common Controls v6 active on this thread (or plainly if unavailable).
pub(crate) fn with_visual_styles<T>(f: impl FnOnce() -> T) -> T {
    let mut cookie = 0usize;
    // SAFETY: live context; the cookie is deactivated below on the same thread.
    let active = context().is_some_and(|ctx| unsafe { ActivateActCtx(Some(ctx), &mut cookie) }.is_ok());
    let result = f();
    if active {
        // SAFETY: deactivates exactly the context activated above with its cookie.
        let _ = unsafe { DeactivateActCtx(0, cookie) };
    }
    result
}
