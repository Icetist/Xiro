//! Runs one step (the firewall rule) in a short-lived elevated copy of this exe.

use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, HANDLE};
use std::os::windows::fs::OpenOptionsExt;

use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::Foundation::WAIT_OBJECT_0;
use windows::Win32::System::Threading::{GetCurrentProcess, GetExitCodeProcess, OpenProcessToken};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW, PostQuitMessage, QS_ALLINPUT, TranslateMessage,
    WM_QUIT,
};

/// A driver install can be slow; past this the step is stuck.
const ELEVATED_STEP_LIMIT: std::time::Duration = std::time::Duration::from_secs(600);
use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{HRESULT, PCWSTR, w};

use super::cmdline::join_args;
use crate::wide::to_wide;

/// A process or token handle closed on drop.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle is owned exclusively by this value.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

pub(crate) fn is_elevated() -> bool {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo handle of the current process needs no closing; the token does.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    let token = OwnedHandle(token);
    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0u32;
    // SAFETY: `elevation` is a writable TOKEN_ELEVATION of the size passed.
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    }
    .is_ok();
    ok && elevation.TokenIsElevated != 0
}

/// Runs this exe elevated. The exe stays open (read-share only) until the copy exits, so nobody
/// can swap it in a user-writable folder before the UAC prompt.
pub(crate) fn run_elevated(args: &[&str]) -> Result<u32, String> {
    let exe = std::env::current_exe().map_err(|e| format!("could not find this program: {e}"))?;
    let _locked = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(&exe)
        .map_err(|e| format!("could not lock {} for the administrator step: {e}", exe.display()))?;
    run_elevated_exe(&exe, args)
}

fn run_elevated_exe(exe: &std::path::Path, args: &[&str]) -> Result<u32, String> {
    let file = to_wide(&exe.display().to_string());
    let parameters = to_wide(&join_args(args));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    // SAFETY: `info` is initialised and its strings outlive the call (UAC prompt included).
    if let Err(e) = unsafe { ShellExecuteExW(&mut info) } {
        if e.code() == HRESULT::from_win32(ERROR_CANCELLED.0) {
            return Err("the administrator permission prompt was declined".into());
        }
        return Err(format!("could not start the administrator step: {}", e.message()));
    }
    if info.hProcess.is_invalid() {
        return Err("the administrator step started without a process handle".into());
    }
    wait_for_exit(OwnedHandle(info.hProcess))
}

/// Waits while pumping messages: the tray and Xiro's window run on this thread.
fn wait_for_exit(process: OwnedHandle) -> Result<u32, String> {
    let deadline = std::time::Instant::now() + ELEVATED_STEP_LIMIT;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err("the administrator step did not finish".into());
        }
        let handles = [process.0];
        // SAFETY: a live process handle; waits for it or for a message to this thread.
        let woke = unsafe { MsgWaitForMultipleObjects(Some(&handles), false, left.as_millis() as u32, QS_ALLINPUT) };
        if woke == WAIT_OBJECT_0 {
            break;
        }
        pump_messages();
    }
    let mut code = 0u32;
    // SAFETY: the process has exited; reading its exit code.
    unsafe { GetExitCodeProcess(process.0, &mut code) }.map_err(|e| format!("could not read the exit code: {}", e.message()))?;
    Ok(code)
}

/// Dispatches what is waiting for this thread; a WM_QUIT is put back for the outer loop.
fn pump_messages() {
    let mut msg = MSG::default();
    // SAFETY: a standard non-blocking message pump on this thread.
    unsafe {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            if msg.message == WM_QUIT {
                PostQuitMessage(msg.wParam.0 as i32);
                return;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elevation_check_is_read_only_and_never_panics() {
        let _ = is_elevated();
    }
}
