//! Tray icon and the message loop for all Xiro windows. The hidden window is top-level because
//! only those get WM_ENDSESSION and "TaskbarCreated" (Explorer restart: re-add the icon).

use std::cell::{Cell, RefCell};
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows::Win32::UI::Shell::NIN_SELECT;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetMessageW, GetWindowLongPtrW,
    IsWindow, IsWindowVisible, SW_SHOWNORMAL, ShowWindow, KillTimer, MSG, PostMessageW, PostQuitMessage, RegisterClassExW,
    RegisterWindowMessageW, SM_CXICON, SM_CXSMICON, SetForegroundWindow, SetTimer, SetWindowLongPtrW,
    TranslateMessage, WINDOW_EX_STYLE, WM_APP, WM_CONTEXTMENU, WM_DESTROY, WM_ENDSESSION, WM_LBUTTONDBLCLK, WM_NULL,
    WM_QUERYENDSESSION, WM_TIMER, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
};
use windows::core::{PCWSTR, w};

use crate::icon::{OwnedIcon, create_icon};
use crate::mailbox::{self, Notice};
use crate::menu::{self, Command};
use crate::{MenuState, PairingInfo, Status, TrayAction, notify_icon, ui};

const CLASS_NAME: PCWSTR = w!("XiroTrayWindow");
const WM_TRAY_ICON: u32 = WM_APP + 1;
/// Posted by `show_pairing`/`notify` from other threads: "check the mailbox".
pub(crate) const WM_WAKE: u32 = WM_APP + 2;
/// NIN_KEYSELECT = NIN_SELECT | NINF_KEY: the icon was activated with the keyboard.
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
/// Status poll timer; also retries adding the icon if the taskbar wasn't ready.
const STATUS_TIMER: usize = 1;
const STATUS_POLL_MS: u32 = 1000;

/// Reached via GWLP_USERDATA; Cell/RefCell because nested loops (menu, dialogs) re-enter the wndproc.
struct TrayState<'a> {
    /// Called each time the pairing window opens: codes are one-time and short-lived.
    pairing: &'a dyn Fn() -> PairingInfo,
    status: Arc<Mutex<Status>>,
    menu_state: &'a dyn Fn() -> MenuState,
    on_action: RefCell<&'a mut dyn FnMut(TrayAction)>,
    pairing_window: Cell<Option<HWND>>,
    small_icon: OwnedIcon,
    large_icon: Option<OwnedIcon>,
    icon_added: Cell<bool>,
    tooltip: RefCell<String>,
    taskbar_created: u32,
}

pub fn run_tray(
    pairing: impl Fn() -> PairingInfo,
    status: Arc<Mutex<Status>>,
    menu_state: impl Fn() -> MenuState,
    mut on_action: impl FnMut(TrayAction),
) -> Result<(), String> {
    // SAFETY: GetDpiForSystem/GetSystemMetricsForDpi have no preconditions.
    let (small, large) = unsafe {
        let dpi = GetDpiForSystem();
        (GetSystemMetricsForDpi(SM_CXSMICON, dpi), GetSystemMetricsForDpi(SM_CXICON, dpi))
    };
    let state = TrayState {
        pairing: &pairing,
        status,
        menu_state: &menu_state,
        on_action: RefCell::new(&mut on_action),
        pairing_window: Cell::new(None),
        small_icon: create_icon(small as u32)?,
        large_icon: create_icon(large as u32).ok(),
        icon_added: Cell::new(false),
        tooltip: RefCell::new(String::new()),
        // SAFETY: registering a well-known message name has no preconditions.
        taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
    };
    let hwnd = create_window()?;
    // SAFETY: `state` outlives the window: it is destroyed below before `state` drops.
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, &state as *const TrayState as isize) };
    add_icon(hwnd, &state);
    mailbox::set_tray_window(Some(hwnd.0 as isize));
    drain_mailbox(hwnd, &state); // requests made before the tray existed
    // SAFETY: a timer on our own window.
    unsafe { SetTimer(Some(hwnd), STATUS_TIMER, STATUS_POLL_MS, None) };

    let result = message_loop();
    mailbox::set_tray_window(None);
    // SAFETY: tear down leftovers, then detach the state so no later message can reach it.
    unsafe {
        if IsWindow(Some(hwnd)).as_bool() {
            let _ = DestroyWindow(hwnd);
        }
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
    }
    result
}

fn message_loop() -> Result<(), String> {
    let mut msg = MSG::default();
    loop {
        // SAFETY: standard message pump on this thread.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) }.0;
        match got {
            0 => return Ok(()),
            -1 => return Err("the tray message loop failed".into()),
            _ => {}
        }
        // SAFETY: dispatching the message we just received.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn create_window() -> Result<HWND, String> {
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    // SAFETY: `class` is fully initialised; a repeated registration fails harmlessly.
    unsafe { RegisterClassExW(&class) };
    // SAFETY: never shown; WS_EX_TOOLWINDOW keeps it out of Alt+Tab.
    unsafe { CreateWindowExW(WS_EX_TOOLWINDOW | WINDOW_EX_STYLE(0), CLASS_NAME, w!("Xiro"), WS_OVERLAPPED, 0, 0, 0, 0, None, None, None, None) }
        .map_err(|e| format!("could not create the tray window: {}", e.message()))
}

fn current_status(state: &TrayState) -> String {
    // A poisoned lock still holds the last status line, which is fine to show.
    let status = state.status.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    status.line.clone()
}

fn add_icon(hwnd: HWND, state: &TrayState) {
    let tip = menu::tooltip(&current_status(state));
    let added = notify_icon::add(hwnd, WM_TRAY_ICON, state.small_icon.0, &tip);
    state.icon_added.set(added);
    if added {
        *state.tooltip.borrow_mut() = tip;
    }
}

fn refresh(hwnd: HWND, state: &TrayState) {
    if let Some(window) = state.pairing_window.get() {
        // SAFETY: plain query on our own window.
        if unsafe { IsWindowVisible(window) }.as_bool() {
            ui::set_hub_live(window, (state.menu_state)(), &current_status(state));
        }
    }
    if !state.icon_added.get() {
        add_icon(hwnd, state);
        return;
    }
    let tip = menu::tooltip(&current_status(state));
    if *state.tooltip.borrow() != tip && notify_icon::set_tip(hwnd, &tip) {
        *state.tooltip.borrow_mut() = tip;
    }
}

fn drain_mailbox(hwnd: HWND, state: &TrayState) {
    if mailbox::take_pairing_request() {
        show_pairing_window(state);
    }
    for Notice { title, text } in mailbox::take_notices() {
        let _ = notify_icon::balloon(hwnd, &title, &text, state.large_icon.as_ref().map(|i| i.0));
    }
    // A running action (in a prompt's loop) is not re-entered; what was queued runs after it.
    if state.on_action.try_borrow_mut().is_ok() {
        for action in mailbox::take_actions() {
            dispatch(state, action);
        }
    }
    if mailbox::take_quit_request() {
        // SAFETY: ends the loop via WM_DESTROY -> PostQuitMessage.
        let _ = unsafe { DestroyWindow(hwnd) };
    }
}

/// Opens the main window (created once, then only hidden) with a fresh one-time code.
fn show_pairing_window(state: &TrayState) {
    let info = (state.pairing)();
    let hwnd = match state.pairing_window.get() {
        Some(hwnd) => {
            ui::set_hub_info(hwnd, info);
            ui::set_hub_live(hwnd, (state.menu_state)(), &current_status(state));
            hwnd
        }
        None => match ui::create_hub(info, (state.menu_state)(), current_status(state)) {
            Ok(hwnd) => {
                state.pairing_window.set(Some(hwnd));
                hwnd
            }
            Err(_) => return, // nothing sensible to show; the tray keeps working
        },
    };
    // SAFETY: showing and focusing our own window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Runs the app's callback, or queues the action if one is already running up the stack.
fn dispatch(state: &TrayState, action: TrayAction) {
    match state.on_action.try_borrow_mut() {
        Ok(mut on_action) => on_action(action),
        Err(_) => mailbox::push_action(action),
    }
}

fn show_menu(hwnd: HWND, state: &TrayState, x: i32, y: i32) {
    let entries = menu::entries(&current_status(state), (state.menu_state)());
    // SAFETY: foreground first so the menu closes on focus loss; WM_NULL after is the documented fix.
    unsafe {
        let _ = SetForegroundWindow(hwnd);
    }
    let chosen = menu::pick(hwnd, &entries, x, y).unwrap_or(0);
    // SAFETY: posting to our own window.
    let _ = unsafe { PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0)) };
    match menu::command_for(chosen) {
        Some(Command::ShowPairing) => show_pairing_window(state),
        Some(Command::Action(TrayAction::Quit)) => {
            dispatch(state, TrayAction::Quit);
            // SAFETY: ends the loop via WM_DESTROY -> PostQuitMessage.
            let _ = unsafe { DestroyWindow(hwnd) };
        }
        Some(Command::Action(action)) => dispatch(state, action),
        None => {}
    }
}

/// Signed 16-bit halves of a version-4 callback's wParam (the icon's anchor point).
fn anchor(wparam: WPARAM) -> (i32, i32) {
    (i32::from(wparam.0 as u16 as i16), i32::from((wparam.0 >> 16) as u16 as i16))
}

fn on_icon_event(hwnd: HWND, state: &TrayState, wparam: WPARAM, lparam: LPARAM) {
    match (lparam.0 & 0xFFFF) as u32 {
        WM_CONTEXTMENU => {
            let (x, y) = anchor(wparam);
            show_menu(hwnd, state, x, y);
        }
        NIN_SELECT | NIN_KEYSELECT | WM_LBUTTONDBLCLK => show_pairing_window(state),
        _ => {}
    }
}

fn handle(hwnd: HWND, state: &TrayState, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_TRAY_ICON => on_icon_event(hwnd, state, wparam, lparam),
        WM_WAKE => drain_mailbox(hwnd, state),
        WM_TIMER if wparam.0 == STATUS_TIMER => {
            refresh(hwnd, state);
            drain_mailbox(hwnd, state); // in case a wake-up message was lost
        }
        // Never veto sign-out or shutdown; the app has nothing unsaved.
        WM_QUERYENDSESSION => return Some(LRESULT(1)),
        WM_ENDSESSION if wparam.0 != 0 => {
            // SAFETY: the session is ending; the process may be killed after this.
            let _ = unsafe { DestroyWindow(hwnd) };
        }
        WM_DESTROY => {
            notify_icon::remove(hwnd);
            if let Some(pairing) = state.pairing_window.take() {
                // SAFETY: our own window on this thread.
                let _ = unsafe { DestroyWindow(pairing) };
            }
            // SAFETY: stops our timer and ends the message loop.
            unsafe {
                let _ = KillTimer(Some(hwnd), STATUS_TIMER);
                PostQuitMessage(0);
            }
        }
        m if m == state.taskbar_created && m != 0 => add_icon(hwnd, state), // Explorer restarted
        _ => return None,
    }
    Some(LRESULT(0))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: USERDATA is 0 or `run_tray`'s live TrayState; the fake 'static never escapes this call.
    let state = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TrayState<'static>).as_ref() };
    if let Some(result) = state.and_then(|state| handle(hwnd, state, msg, wparam, lparam)) {
        return result;
    }
    // SAFETY: default processing for everything we do not handle.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Wakes the tray thread, if one is running, to drain the mailbox.
pub(crate) fn wake() {
    if let Some(raw) = mailbox::tray_window() {
        // SAFETY: posting to a window of another thread is allowed; a stale handle just fails.
        let _ = unsafe { PostMessageW(Some(HWND(raw as *mut _)), WM_WAKE, WPARAM(0), LPARAM(0)) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_coordinates_are_signed() {
        // x = -10 (a monitor left of the primary), y = 1040.
        let packed = ((1040u32 << 16) | (-10i16 as u16 as u32)) as usize;
        assert_eq!(anchor(WPARAM(packed)), (-10, 1040));
    }
}
