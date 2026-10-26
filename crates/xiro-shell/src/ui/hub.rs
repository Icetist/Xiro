//! The main window: displays with the code on the left, settings on the right, and a
//! removal page. Created on first use, only hidden on close.

use std::time::{Duration, Instant};

use super::controls::{Control, Rect, Style};
use super::frame::{Reply, View};
use super::gfx::Gfx;
use super::paint::Painter;
use super::theme::glyph;
use super::words;
use crate::{DisplayHealth, MenuState, PairingInfo, TrayAction, WifiState, clipboard, mailbox, tray};

pub(super) const WIDTH: f32 = 944.0;
pub(super) const HEIGHT: f32 = 548.0;
const COPIED_FOR: Duration = Duration::from_millis(1600);
const TICK: Duration = Duration::from_millis(250);

pub(super) const ID_COPY: u16 = 1;
pub(super) const ID_STEP_JOIN: u16 = 2;
pub(super) const ID_STEP_OPEN: u16 = 3;
pub(super) const ID_WIFI: u16 = 4;
pub(super) const ID_AUTOSTART: u16 = 5;
pub(super) const ID_UNLINK: u16 = 6;
pub(super) const ID_HELP: u16 = 7;
pub(super) const ID_LOGS: u16 = 8;
pub(super) const ID_UNINSTALL: u16 = 9;
pub(super) const ID_BACK: u16 = 10;
pub(super) const ID_REPAIR: u16 = 11;

/// Layout in DIPs: display arrangement on the left, settings on the right.
pub(super) mod geo {
    use super::Rect;

    pub const LEFT: f32 = 32.0;
    pub const HEADING: Rect = Rect::new(LEFT, 28.0, 600.0, 32.0);
    pub const TAB_JOIN: Rect = Rect::new(LEFT, 28.0, 150.0, 32.0);
    pub const TAB_OPEN: Rect = Rect::new(LEFT + 166.0, 28.0, 100.0, 32.0);
    pub const MONITOR: Rect = Rect::new(LEFT, 84.0, 400.0, 280.0);
    pub const QR_TILE: Rect = Rect::new(LEFT + 80.0, 104.0, 240.0, 240.0);
    /// The second screen's place: bottom-aligned with the monitor, at most this big.
    pub const SLOT_X: f32 = LEFT + 416.0;
    pub const SLOT_MAX: f32 = 228.0;
    /// Its shape until a device connects: an iPad upright.
    pub const SLOT_WAITING: Rect = Rect::new(SLOT_X, 364.0 - 224.0, 168.0, 224.0);
    pub const REPAIR: Rect = Rect::new(SLOT_X + 24.0, SLOT_WAITING.y + 150.0, 120.0, 32.0);
    pub const NAMES_Y: f32 = 376.0;
    pub const CAPTION: Rect = Rect::new(LEFT, 414.0, 612.0, 44.0);
    pub const LINK: Rect = Rect::new(LEFT, 480.0, 520.0, 36.0);
    pub const COPY: Rect = Rect::new(LEFT + 528.0, 480.0, 116.0, 36.0);

    pub const RIGHT_X: f32 = 712.0;
    pub const RIGHT_W: f32 = 200.0;
    pub const WORDMARK: Rect = Rect::new(RIGHT_X, 28.0, RIGHT_W, 32.0);
    pub const STATUS: Rect = Rect::new(RIGHT_X, 60.0, RIGHT_W, 20.0);
    pub const WIFI_ROW: Rect = Rect::new(RIGHT_X, 112.0, RIGHT_W, 64.0);
    pub const AUTO_ROW: Rect = Rect::new(RIGHT_X, 200.0, RIGHT_W, 64.0);
    pub const DEVICES_Y: f32 = 288.0;
    pub const UNLINK: Rect = Rect::new(RIGHT_X, 348.0, RIGHT_W, 32.0);
    pub const HELP: Rect = Rect::new(RIGHT_X, 504.0, 44.0, 28.0);
    pub const LOGS: Rect = Rect::new(RIGHT_X + 56.0, 504.0, 44.0, 28.0);
    pub const UNINSTALL: Rect = Rect::new(RIGHT_X + RIGHT_W - 60.0, 504.0, 60.0, 28.0);
    /// Rules between the settings rows.
    pub const RULES: [f32; 3] = [100.0, 188.0, 276.0];

    pub const BACK: Rect = Rect::new(LEFT, 28.0, 72.0, 32.0);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Page {
    Connect,
    Help,
}

pub(crate) struct Hub {
    pub(super) info: PairingInfo,
    pub(super) state: MenuState,
    pub(super) status: String,
    pub(super) page: Page,
    /// Showing the "join Xiro Wi-Fi" code (step 1) rather than the pairing code.
    pub(super) joining: bool,
    pub(super) qr: Vec<Vec<bool>>,
    pub(super) wifi_qr: Vec<Vec<bool>>,
    copied_until: Option<Instant>,
}

/// The QR code's modules (true = dark), without the quiet zone.
fn modules(text: &str) -> Vec<Vec<bool>> {
    let Ok(qr) = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium) else { return Vec::new() };
    let size = qr.size();
    (0..size).map(|y| (0..size).map(|x| qr.get_module(x, y)).collect()).collect()
}

/// Hands an action to the app, which runs it on the tray thread.
fn post(action: TrayAction) {
    mailbox::push_action(action);
    tray::wake();
}

impl Hub {
    pub(crate) fn new(info: PairingInfo, state: MenuState, status: String) -> Self {
        let mut hub = Self { info: PairingInfo::default(), state, status, page: Page::Connect, joining: false, qr: Vec::new(), wifi_qr: Vec::new(), copied_until: None };
        hub.set_info(info);
        hub
    }

    /// New pairing code (they are one-time) or Xiro Wi-Fi switched on or off.
    pub(crate) fn set_info(&mut self, info: PairingInfo) -> Reply {
        let wifi_appeared = info.wifi.is_some() && self.info.wifi.is_none();
        self.qr = modules(&info.setup_url);
        self.wifi_qr = info.wifi.as_ref().map_or_else(Vec::new, |w| modules(&w.qr_text));
        self.joining = info.wifi.is_some() && (self.joining || wifi_appeared);
        self.info = info;
        Reply::Repaint
    }

    /// The status line and switches, polled every second; repaints only on a change.
    pub(crate) fn set_live(&mut self, state: MenuState, status: &str) -> Reply {
        if self.state == state && self.status == status {
            return Reply::Nothing;
        }
        self.state = state;
        status.clone_into(&mut self.status);
        Reply::Repaint
    }

    pub(super) fn has_steps(&self) -> bool {
        self.info.wifi.is_some()
    }

    pub(super) fn copied(&self) -> bool {
        self.copied_until.is_some()
    }

    /// What the link row shows and copies: the network password while joining, else the link.
    pub(super) fn link_text(&self) -> String {
        match (&self.info.wifi, self.joining) {
            (Some(wifi), true) => wifi.passphrase.clone(),
            _ => self.info.setup_url.clone(),
        }
    }

    fn connect_controls(&self) -> Vec<Control> {
        let mut out = Vec::new();
        if self.has_steps() {
            out.push(Control::new(ID_STEP_JOIN, Style::Segment, geo::TAB_JOIN, words::STEP_JOIN).on(self.joining));
            out.push(Control::new(ID_STEP_OPEN, Style::Segment, geo::TAB_OPEN, words::STEP_OPEN).on(!self.joining));
        }
        if self.needs_repair() {
            out.push(Control::new(ID_REPAIR, Style::Primary, geo::REPAIR, words::REPAIR));
        }
        let (label, icon) = if self.copied() { (words::COPIED, glyph::CHECK) } else { (words::COPY, glyph::COPY) };
        out.push(Control::new(ID_COPY, Style::Secondary, geo::COPY, label).icon(icon));
        let wifi_on = self.state.xiro_wifi != WifiState::Off;
        out.push(Control::new(ID_WIFI, Style::Switch, geo::WIFI_ROW, "").on(wifi_on).enabled(self.state.xiro_wifi != WifiState::Starting));
        out.push(Control::new(ID_AUTOSTART, Style::Switch, geo::AUTO_ROW, "").on(self.state.autostart));
        out.push(Control::new(ID_UNLINK, Style::Secondary, geo::UNLINK, words::UNLINK));
        out.push(Control::new(ID_HELP, Style::Quiet, geo::HELP, words::HELP));
        out.push(Control::new(ID_LOGS, Style::Quiet, geo::LOGS, words::LOGS));
        out.push(Control::new(ID_UNINSTALL, Style::QuietDanger, geo::UNINSTALL, words::UNINSTALL));
        out
    }

    /// The second screen can't work until the driver is fixed (a device may still mirror).
    pub(super) fn needs_repair(&self) -> bool {
        self.state.display == DisplayHealth::NeedsRepair
    }

    /// The connected device's screen, read from the status line.
    pub(super) fn live_screen(&self) -> Option<words::LiveScreen> {
        words::live_screen(&self.status)
    }
}

impl View for Hub {
    fn size(&self, _gfx: &Gfx) -> (f32, f32) {
        (WIDTH, HEIGHT)
    }

    fn controls(&self) -> Vec<Control> {
        match self.page {
            Page::Connect => self.connect_controls(),
            Page::Help => vec![Control::new(ID_BACK, Style::Quiet, geo::BACK, words::BACK).icon(glyph::BACK)],
        }
    }

    fn paint(&self, painter: &Painter, size: (f32, f32)) {
        self.paint_all(painter, size);
    }

    fn activate(&mut self, id: u16) -> Reply {
        match id {
            ID_COPY => {
                if clipboard::copy(&self.link_text()).is_ok() {
                    self.copied_until = Some(Instant::now() + COPIED_FOR);
                }
            }
            ID_STEP_JOIN | ID_STEP_OPEN => self.joining = id == ID_STEP_JOIN,
            ID_WIFI => {
                // Shown at once; the app confirms (or undoes) it within a second.
                self.state.xiro_wifi = if self.state.xiro_wifi == WifiState::Off { WifiState::Starting } else { WifiState::Off };
                post(TrayAction::ToggleXiroWifi);
            }
            ID_AUTOSTART => {
                self.state.autostart = !self.state.autostart;
                post(TrayAction::ToggleAutostart);
            }
            ID_UNLINK => post(TrayAction::UnlinkAll),
            ID_UNINSTALL => post(TrayAction::Uninstall),
            ID_REPAIR => post(TrayAction::RepairDisplay),
            ID_LOGS => post(TrayAction::OpenLogFolder),
            ID_HELP => self.page = Page::Help,
            ID_BACK => self.page = Page::Connect,
            _ => return Reply::Nothing,
        }
        Reply::Repaint
    }

    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn dismiss(&mut self) -> Reply {
        if self.page == Page::Help {
            self.page = Page::Connect;
            return Reply::Repaint;
        }
        Reply::Hide
    }

    fn tick_every(&self) -> Option<Duration> {
        Some(TICK)
    }

    fn tick(&mut self, now: Instant) -> Reply {
        match self.copied_until {
            Some(until) if now >= until => {
                self.copied_until = None;
                Reply::Repaint
            }
            _ => Reply::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WifiInfo;

    fn info(wifi: bool) -> PairingInfo {
        PairingInfo {
            setup_url: "http://192.168.1.5:8080/#c=abc".into(),
            app_url: String::new(),
            fingerprint: "AB:CD".into(),
            addresses: Vec::new(),
            wifi: wifi.then(|| WifiInfo { ssid: "Xiro-PC".into(), passphrase: "secret".into(), qr_text: "WIFI:T:WPA;S:Xiro-PC;P:secret;;".into() }),
        }
    }

    fn ids(hub: &Hub) -> Vec<u16> {
        hub.controls().iter().map(|c| c.id).collect()
    }

    #[test]
    fn steps_appear_with_xiro_wifi_and_start_on_joining() {
        let mut hub = Hub::new(info(false), MenuState::default(), String::new());
        assert!(!ids(&hub).contains(&ID_STEP_JOIN));
        assert_eq!(hub.link_text(), "http://192.168.1.5:8080/#c=abc");
        hub.set_info(info(true));
        assert!(hub.joining && ids(&hub).contains(&ID_STEP_JOIN));
        assert_eq!(hub.link_text(), "secret", "the password, while joining");
        assert!(!hub.wifi_qr.is_empty() && !hub.qr.is_empty());
        hub.activate(ID_STEP_OPEN);
        assert!(!hub.joining);
        hub.set_info(info(true));
        assert!(!hub.joining, "a new code keeps the chosen step");
    }

    #[test]
    fn the_fix_button_shows_only_when_the_display_needs_it() {
        let mut hub = Hub::new(info(false), MenuState::default(), String::new());
        assert!(!ids(&hub).contains(&ID_REPAIR));
        let broken = MenuState { display: DisplayHealth::NeedsRepair, ..MenuState::default() };
        assert_eq!(hub.set_live(broken, ""), Reply::Repaint);
        assert!(ids(&hub).contains(&ID_REPAIR));
        assert_eq!(hub.set_live(broken, ""), Reply::Nothing, "unchanged: no repaint");
    }

    #[test]
    fn help_is_a_page_that_esc_leaves() {
        let mut hub = Hub::new(info(false), MenuState::default(), String::new());
        hub.activate(ID_HELP);
        assert_eq!(ids(&hub), vec![ID_BACK]);
        assert_eq!(hub.dismiss(), Reply::Repaint);
        assert_eq!(hub.page, Page::Connect);
        assert_eq!(hub.dismiss(), Reply::Hide, "closing only hides the window");
    }

    #[test]
    fn controls_stay_inside_the_window_without_overlapping() {
        let mut hub = Hub::new(info(true), MenuState { display: DisplayHealth::NeedsRepair, ..MenuState::default() }, String::new());
        hub.joining = true;
        let all = hub.controls();
        for c in &all {
            assert!(c.rect.x >= 0.0 && c.rect.right() <= WIDTH && c.rect.bottom() <= HEIGHT, "{}", c.id);
        }
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                let apart = a.rect.right() <= b.rect.x || b.rect.right() <= a.rect.x || a.rect.bottom() <= b.rect.y || b.rect.bottom() <= a.rect.y;
                assert!(apart, "{} overlaps {}", a.id, b.id);
            }
        }
    }
}
