//! The tray menu as testable data, plus the function that builds the real popup.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, HMENU, MENU_ITEM_FLAGS, MF_CHECKED, MF_GRAYED, MF_SEPARATOR,
    MF_STRING, SetMenuDefaultItem, TPM_BOTTOMALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
};
use windows::core::PCWSTR;

use crate::{DisplayHealth, MenuState, TrayAction};
use crate::wide::to_wide;

pub(crate) const ID_STATUS: u16 = 1;
pub(crate) const ID_OPEN: u16 = 2;
pub(crate) const ID_AUTOSTART: u16 = 3;
pub(crate) const ID_REPAIR: u16 = 4;
pub(crate) const ID_QUIT: u16 = 5;

/// Shown when the app has not reported a status yet.
const IDLE_STATUS: &str = "Xiro is running";
const APP_NAME: &str = "Xiro";

#[derive(Debug, PartialEq)]
pub(crate) enum Entry {
    /// Information only: greyed out, does nothing.
    Label(String),
    Item { id: u16, text: &'static str, is_default: bool },
    Check { id: u16, text: &'static str, checked: bool },
    Separator,
}

/// What a menu command (or icon click) asks the tray to do.
#[derive(Debug, PartialEq)]
pub(crate) enum Command {
    ShowPairing,
    Action(TrayAction),
}

/// The status line as shown, never empty.
pub(crate) fn status_text(status: &str) -> &str {
    let trimmed = status.trim();
    if trimmed.is_empty() { IDLE_STATUS } else { trimmed }
}

/// Menu text treats `&` as a mnemonic marker; status lines are literal text.
fn escape_mnemonics(text: &str) -> String {
    text.replace('&', "&&")
}

/// Short on purpose: everything else lives in Xiro's window.
pub(crate) fn entries(status: &str, state: MenuState) -> Vec<Entry> {
    let mut menu = vec![
        Entry::Label(escape_mnemonics(status_text(status))),
        Entry::Item { id: ID_OPEN, text: "Open Xiro", is_default: true },
    ];
    if state.display == DisplayHealth::NeedsRepair {
        menu.push(Entry::Item { id: ID_REPAIR, text: "Fix the second screen\u{2026}", is_default: false });
    }
    menu.extend([
        Entry::Separator,
        Entry::Check { id: ID_AUTOSTART, text: "Start with Windows", checked: state.autostart },
        Entry::Separator,
        Entry::Item { id: ID_QUIT, text: "Quit Xiro", is_default: false },
    ]);
    menu
}

pub(crate) fn command_for(id: u16) -> Option<Command> {
    match id {
        ID_OPEN => Some(Command::ShowPairing),
        ID_AUTOSTART => Some(Command::Action(TrayAction::ToggleAutostart)),
        ID_REPAIR => Some(Command::Action(TrayAction::RepairDisplay)),
        ID_QUIT => Some(Command::Action(TrayAction::Quit)),
        _ => None,
    }
}

/// Tooltip on hover: "Xiro — <status>".
pub(crate) fn tooltip(status: &str) -> String {
    format!("{APP_NAME} \u{2014} {}", status_text(status))
}

/// A popup menu destroyed on drop.
struct OwnedMenu(HMENU);

impl Drop for OwnedMenu {
    fn drop(&mut self) {
        // SAFETY: created by `CreatePopupMenu` and owned by this value.
        let _ = unsafe { DestroyMenu(self.0) };
    }
}

fn append(menu: HMENU, flags: MENU_ITEM_FLAGS, id: u16, text: &str) -> Result<(), String> {
    let wide = to_wide(text);
    // SAFETY: `menu` is live; `wide` is NUL-terminated and the menu copies it.
    unsafe { AppendMenuW(menu, flags, usize::from(id), PCWSTR(wide.as_ptr())) }.map_err(|e| e.message())
}

fn build(entries: &[Entry]) -> Result<OwnedMenu, String> {
    // SAFETY: no preconditions; ownership goes to `OwnedMenu`.
    let menu = OwnedMenu(unsafe { CreatePopupMenu() }.map_err(|e| e.message())?);
    for entry in entries {
        match entry {
            Entry::Label(text) => append(menu.0, MF_STRING | MF_GRAYED, ID_STATUS, text)?,
            Entry::Item { id, text, is_default } => {
                append(menu.0, MF_STRING, *id, text)?;
                if *is_default {
                    // SAFETY: `menu.0` is live and contains the item just appended.
                    let _ = unsafe { SetMenuDefaultItem(menu.0, u32::from(*id), 0) };
                }
            }
            Entry::Check { id, text, checked } => {
                let state = if *checked { MF_CHECKED } else { MENU_ITEM_FLAGS(0) };
                append(menu.0, MF_STRING | state, *id, text)?;
            }
            Entry::Separator => {
                // SAFETY: separators take no text.
                unsafe { AppendMenuW(menu.0, MF_SEPARATOR, 0, PCWSTR::null()) }.map_err(|e| e.message())?;
            }
        }
    }
    Ok(menu)
}

/// Shows the menu at `(x, y)`; returns the chosen id (0 = dismissed).
/// `owner` must be foreground first or the menu won't close on outside clicks (TrackPopupMenu quirk).
pub(crate) fn pick(owner: HWND, entries: &[Entry], x: i32, y: i32) -> Result<u16, String> {
    let menu = build(entries)?;
    let flags = TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN;
    // SAFETY: `menu.0` is a live popup and `owner` a window of this thread.
    let chosen = unsafe { TrackPopupMenu(menu.0, flags, x, y, None, owner, None) };
    Ok(chosen.0 as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_lists_status_open_autostart_and_quit() {
        let menu = entries("1 device connected · 2360×1640 · 60 fps", MenuState { autostart: true, ..MenuState::default() });
        assert_eq!(menu[0], Entry::Label("1 device connected · 2360×1640 · 60 fps".into()));
        assert_eq!(menu[1], Entry::Item { id: ID_OPEN, text: "Open Xiro", is_default: true });
        assert_eq!(menu[2], Entry::Separator);
        assert_eq!(menu[3], Entry::Check { id: ID_AUTOSTART, text: "Start with Windows", checked: true });
        assert_eq!(menu[4], Entry::Separator);
        assert_eq!(menu[5], Entry::Item { id: ID_QUIT, text: "Quit Xiro", is_default: false });
        assert_eq!(menu.len(), 6);
    }

    #[test]
    fn a_broken_display_adds_a_fix_item() {
        let menu = entries("", MenuState { display: DisplayHealth::NeedsRepair, ..MenuState::default() });
        assert!(matches!(menu[2], Entry::Item { id: ID_REPAIR, .. }));
    }

    #[test]
    fn status_line_is_never_blank_and_ampersands_stay_literal() {
        assert_eq!(entries("  ", MenuState::default())[0], Entry::Label(IDLE_STATUS.into()));
        assert_eq!(entries("Tom & Jerry's iPad", MenuState::default())[0], Entry::Label("Tom && Jerry's iPad".into()));
        assert_eq!(tooltip(""), "Xiro \u{2014} Xiro is running");
        assert_eq!(tooltip("60 fps"), "Xiro \u{2014} 60 fps");
    }

    #[test]
    fn every_item_maps_to_its_command() {
        assert_eq!(command_for(ID_OPEN), Some(Command::ShowPairing));
        assert_eq!(command_for(ID_AUTOSTART), Some(Command::Action(TrayAction::ToggleAutostart)));
        assert_eq!(command_for(ID_REPAIR), Some(Command::Action(TrayAction::RepairDisplay)));
        assert_eq!(command_for(ID_QUIT), Some(Command::Action(TrayAction::Quit)));
        assert_eq!(command_for(ID_STATUS), None);
        assert_eq!(command_for(0), None);
    }
}
