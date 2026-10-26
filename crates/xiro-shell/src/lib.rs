//! Windows shell integration for Xiro: tray, Direct2D windows and dialogs, notifications, setup.

mod approval;
mod clipboard;
mod icon;
pub mod install;
mod mailbox;
mod menu;
mod notify_icon;
mod styles;
mod tray;
mod ui;
mod wide;

pub use approval::{ANSWER_TIMEOUT, ask_to_pair};
pub use tray::run_tray;
pub use ui::prompt::{Badge, Choice, Prompt};

/// Everything the main window shows about pairing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PairingInfo {
    /// Encoded in the QR code: the plain-HTTP setup page.
    pub setup_url: String,
    /// The secure viewer, for devices that are already set up.
    pub app_url: String,
    /// Certificate fingerprint shown on the iPad's setup page, for comparison.
    pub fingerprint: String,
    /// Other ways to reach this PC (other adapters' IPs, the mDNS name).
    pub addresses: Vec<String>,
    /// Set while Xiro Wi-Fi is on: the device joins it first, with its own QR code.
    pub wifi: Option<WifiInfo>,
}

/// The PC's own Wi-Fi network, as the main window shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WifiInfo {
    pub ssid: String,
    pub passphrase: String,
    /// `WIFI:` text: the camera offers "Join network" when it sees this QR code.
    pub qr_text: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WifiState {
    #[default]
    Off,
    Starting,
    On,
}

/// Whether the virtual second screen can work right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DisplayHealth {
    #[default]
    Ready,
    /// The display driver is missing, stopped or misconfigured: offer to fix it.
    NeedsRepair,
}

/// The switches and warnings the window and the tray menu show, asked every second.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MenuState {
    pub autostart: bool,
    pub xiro_wifi: WifiState,
    pub display: DisplayHealth,
}

/// Something asked for in the window or the tray menu; run on the tray thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    ToggleAutostart,
    ToggleXiroWifi,
    /// Sign every paired device out.
    UnlinkAll,
    /// Remove Xiro from this PC (asks first).
    Uninstall,
    /// Reinstall or restart the display driver (asks Windows for permission).
    RepairDisplay,
    OpenLogFolder,
    /// The tray loop ends right after this callback returns.
    Quit,
}

/// Live status for the window, tooltip and menu, e.g. "1 device connected · 2360×1640 · 60 fps".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub line: String,
}

/// Shows the main window (with a fresh pairing code) from any thread.
pub fn show_pairing() {
    mailbox::request_pairing();
    tray::wake();
}

/// A notification from the tray icon, from any thread; queued until the tray runs.
pub fn notify(title: &str, text: &str) {
    mailbox::push_notice(mailbox::Notice { title: title.to_owned(), text: text.to_owned() });
    tray::wake();
}

/// Runs a tray action from any thread, as if chosen in the window.
pub fn request(action: TrayAction) {
    mailbox::push_action(action);
    tray::wake();
}

/// Ends the tray (and so the app's main loop) from any thread, e.g. after uninstalling.
pub fn quit() {
    mailbox::request_quit();
    tray::wake();
}

/// Shows a Xiro dialog and returns the chosen button (a message box if it can't be drawn).
pub fn ask(prompt: Prompt) -> usize {
    let fallback = prompt.clone();
    ui::prompt::ask(prompt).unwrap_or_else(|| {
        if fallback.buttons.len() <= 1 {
            install::message(fallback.caption, &format!("{}\n\n{}", fallback.title, fallback.body), false);
            return 0;
        }
        let yes = (0..fallback.buttons.len()).find(|i| *i != fallback.cancel).unwrap_or(fallback.cancel);
        if install::confirm(fallback.caption, &format!("{}\n\n{}", fallback.title, fallback.body)) { yes } else { fallback.cancel }
    })
}
