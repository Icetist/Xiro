//! Xiro's own interface, drawn with Direct2D: the main window and the prompts.

mod controls;
mod frame;
mod gfx;
mod hub;
mod hub_paint;
mod paint;
pub(crate) mod prompt;
mod theme;
mod words;

use windows::Win32::Foundation::HWND;

use crate::{MenuState, PairingInfo};
pub(crate) use frame::Reply;
use hub::Hub;

/// Creates the (hidden) main window.
pub(crate) fn create_hub(info: PairingInfo, state: MenuState, status: String) -> Result<HWND, String> {
    let options = frame::Options { title: words::WINDOW_TITLE, topmost: false, dialog: false };
    frame::create(Box::new(Hub::new(info, state, status)), &options)
}

/// A new pairing code, or Xiro Wi-Fi switched on or off.
pub(crate) fn set_hub_info(hwnd: HWND, info: PairingInfo) {
    frame::update(hwnd, |view| view.as_any().downcast_mut::<Hub>().map_or(Reply::Nothing, |hub| hub.set_info(info)));
}

/// The once-a-second refresh of the status line and switches.
pub(crate) fn set_hub_live(hwnd: HWND, state: MenuState, status: &str) {
    frame::update(hwnd, |view| view.as_any().downcast_mut::<Hub>().map_or(Reply::Nothing, |hub| hub.set_live(state, status)));
}
