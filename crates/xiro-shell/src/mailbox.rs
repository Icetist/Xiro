//! Requests to the tray thread, which owns the windows: callers queue here and post a wake-up;
//! the tray drains on wake-up and at startup, so early requests are not lost.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use crate::TrayAction;

/// Notifications kept while the tray is not (yet) running; older ones are dropped beyond this.
const MAX_QUEUED_NOTICES: usize = 8;

/// The tray's hidden window, or 0 when no tray is running.
static TRAY_WINDOW: AtomicIsize = AtomicIsize::new(0);
static SHOW_PAIRING: AtomicBool = AtomicBool::new(false);
static QUIT: AtomicBool = AtomicBool::new(false);
static NOTICES: Mutex<VecDeque<Notice>> = Mutex::new(VecDeque::new());
/// Choices made in the main window, run by the tray thread outside the window's own handler.
static ACTIONS: Mutex<VecDeque<TrayAction>> = Mutex::new(VecDeque::new());

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Notice {
    pub title: String,
    pub text: String,
}

/// Raw HWND value of the running tray window, if any.
pub(crate) fn tray_window() -> Option<isize> {
    Some(TRAY_WINDOW.load(Ordering::Acquire)).filter(|&h| h != 0)
}

pub(crate) fn set_tray_window(hwnd: Option<isize>) {
    TRAY_WINDOW.store(hwnd.unwrap_or(0), Ordering::Release);
}

pub(crate) fn request_pairing() {
    SHOW_PAIRING.store(true, Ordering::Release);
}

pub(crate) fn take_pairing_request() -> bool {
    SHOW_PAIRING.swap(false, Ordering::AcqRel)
}

pub(crate) fn request_quit() {
    QUIT.store(true, Ordering::Release);
}

pub(crate) fn take_quit_request() -> bool {
    QUIT.swap(false, Ordering::AcqRel)
}

/// Queues an action; one already waiting is not queued twice (a double click is one wish).
pub(crate) fn push_action(action: TrayAction) {
    let mut queue = ACTIONS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if !queue.contains(&action) {
        queue.push_back(action);
    }
}

pub(crate) fn take_actions() -> Vec<TrayAction> {
    ACTIONS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).drain(..).collect()
}

pub(crate) fn push_notice(notice: Notice) {
    // A poisoned lock only means another thread panicked mid-push; the queue is still valid.
    let mut queue = NOTICES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if queue.len() == MAX_QUEUED_NOTICES {
        queue.pop_front();
    }
    queue.push_back(notice);
}

pub(crate) fn take_notices() -> Vec<Notice> {
    let mut queue = NOTICES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    queue.drain(..).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test touches the process-wide statics so parallel tests cannot interleave.
    #[test]
    fn requests_are_kept_until_taken_and_bounded() {
        request_pairing();
        assert!(take_pairing_request());
        assert!(!take_pairing_request());

        for i in 0..MAX_QUEUED_NOTICES + 3 {
            push_notice(Notice { title: format!("t{i}"), text: String::new() });
        }
        let notices = take_notices();
        assert_eq!(notices.len(), MAX_QUEUED_NOTICES);
        assert_eq!(notices[0].title, "t3", "oldest are dropped first");
        assert!(take_notices().is_empty());

        push_action(TrayAction::UnlinkAll);
        push_action(TrayAction::UnlinkAll);
        push_action(TrayAction::Uninstall);
        assert_eq!(take_actions(), vec![TrayAction::UnlinkAll, TrayAction::Uninstall]);
        assert!(take_actions().is_empty());
        request_quit();
        assert!(take_quit_request());
        assert!(!take_quit_request());

        assert_eq!(tray_window(), None);
        set_tray_window(Some(42));
        assert_eq!(tray_window(), Some(42));
        set_tray_window(None);
        assert_eq!(tray_window(), None);
    }
}
