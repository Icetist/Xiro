//! Main window painting: the second screen is a dashed outline until a device connects,
//! then takes that device's shape and shows its resolution.

use super::controls::Rect;
use super::gfx::{CENTERED, CENTERED_PARAGRAPH, LINE, PARAGRAPH};
use super::hub::{Hub, Page, geo};
use super::paint::Painter;
use super::theme::{self, Color, Font, radius};
use super::words::{self, LiveScreen};
use crate::WifiState;

const QR_INK: Color = Color::hex(0x111112);
const QR_PAPER: Color = Color::hex(0xFFFFFF);
const QR_RADIUS: f32 = 6.0;
const QR_MARGIN: f32 = 14.0;
const DOT: f32 = 3.5;
/// A switch row's words stop short of its switch.
const ROW_TEXT_W: f32 = geo::RIGHT_W - 52.0;

impl Hub {
    pub(super) fn paint_all(&self, p: &Painter, _size: (f32, f32)) {
        match self.page {
            Page::Connect => {
                self.paint_arrangement(p);
                self.paint_settings(p);
            }
            Page::Help => self.paint_help(p),
        }
        for c in super::frame::View::controls(self) {
            p.control(&c);
        }
    }

    /// Where the second screen sits: the connected device's shape, or an upright iPad.
    fn slot(live: Option<LiveScreen>) -> Rect {
        let bottom = geo::MONITOR.bottom();
        let Some(screen) = live.filter(|s| s.width > 0 && s.height > 0) else { return geo::SLOT_WAITING };
        let aspect = screen.width as f32 / screen.height as f32;
        let (w, h) = if aspect >= 1.0 { (geo::SLOT_MAX, geo::SLOT_MAX / aspect) } else { (geo::SLOT_MAX * aspect, geo::SLOT_MAX) };
        Rect::new(geo::SLOT_X, bottom - h, w, h)
    }

    fn paint_arrangement(&self, p: &Painter) {
        if !self.has_steps() {
            p.label(words::HEADING, theme::TITLE, geo::HEADING, LINE, p.p.text);
        }
        let monitor = geo::MONITOR;
        p.t.fill(monitor, radius::SCREEN, p.p.surface);
        p.t.stroke(monitor, radius::SCREEN, p.p.edge, 1.0);
        p.t.fill(geo::QR_TILE, QR_RADIUS, QR_PAPER);
        let code = if self.joining && self.has_steps() { &self.wifi_qr } else { &self.qr };
        p.t.qr(code, geo::QR_TILE.inset(QR_MARGIN), QR_INK);

        let live = self.live_screen();
        // Needing a fix, it keeps its waiting shape: the Fix button sits in it.
        let slot = if self.needs_repair() { geo::SLOT_WAITING } else { Self::slot(live) };
        if self.needs_repair() {
            self.paint_slot_repair(p, slot);
        } else if let Some(screen) = live {
            self.paint_slot_live(p, slot, screen);
        } else {
            p.t.dashed(slot, radius::TABLET, p.p.edge, 1.2);
            let text = Rect::new(slot.x + 20.0, slot.y + slot.h / 2.0 - 22.0, slot.w - 40.0, 48.0);
            p.label(words::SLOT_WAITING, theme::SMALL, text, CENTERED_PARAGRAPH, p.p.tertiary);
        }

        p.label(words::THIS_PC, theme::SMALL, Rect::new(monitor.x, geo::NAMES_Y, monitor.w, 18.0), LINE, p.p.tertiary);
        p.label(words::SECOND_SCREEN, theme::SMALL, Rect::new(slot.x, geo::NAMES_Y, geo::SLOT_MAX, 18.0), LINE, p.p.tertiary);
        p.label(&words::caption(self.joining, &self.info), theme::BODY, geo::CAPTION, PARAGRAPH, p.p.secondary);
        self.paint_link(p);
    }

    fn paint_slot_live(&self, p: &Painter, slot: Rect, screen: LiveScreen) {
        p.t.fill(slot, radius::TABLET, p.p.raised);
        p.t.stroke(slot, radius::TABLET, p.p.edge, 1.0);
        let size = format!("{}\u{2009}\u{d7}\u{2009}{}", screen.width, screen.height);
        let middle = slot.y + slot.h / 2.0;
        p.label(&size, theme::FIGURE, Rect::new(slot.x, middle - 22.0, slot.w, 30.0), CENTERED, p.p.text);
        let rate = screen.fps.map_or_else(String::new, |fps| format!("{fps} fps"));
        p.label(&rate, theme::SMALL, Rect::new(slot.x, middle + 10.0, slot.w, 18.0), CENTERED, p.p.secondary);
        p.t.circle(slot.x + 18.0, slot.y + 18.0, DOT, p.p.success);
    }

    fn paint_slot_repair(&self, p: &Painter, slot: Rect) {
        p.t.dashed(slot, radius::TABLET, p.p.danger.alpha(0.7), 1.2);
        let text = Rect::new(slot.x + 18.0, slot.y + 46.0, slot.w - 36.0, 96.0);
        p.label(words::SLOT_REPAIR, theme::SMALL, text, CENTERED_PARAGRAPH, p.p.secondary);
    }

    /// The link (or Xiro Wi-Fi's password) in a field, for typing it in by hand.
    fn paint_link(&self, p: &Painter) {
        let field = geo::LINK;
        p.t.fill(field, radius::CONTROL, p.p.surface);
        p.t.stroke(field, radius::CONTROL, p.p.hairline, 1.0);
        let mut x = field.x + 14.0;
        if self.joining && self.has_steps() {
            p.label(words::PASSWORD, theme::SMALL, Rect::new(x, field.y, 80.0, field.h), LINE, p.p.tertiary);
            x += p.measure(words::PASSWORD, theme::SMALL) + 12.0;
        }
        let text = Rect::new(x, field.y, field.right() - 14.0 - x, field.h);
        p.label(&self.link_text(), theme::MONO, text, LINE, p.p.secondary);
    }

    fn paint_settings(&self, p: &Painter) {
        p.label("Xiro", theme::WORDMARK, geo::WORDMARK, LINE, p.p.text);
        let live = words::is_live(&self.status);
        let status = match self.status.trim() {
            _ if live => "Connected",
            "" => "Starting\u{2026}",
            other => other,
        };
        let s = geo::STATUS;
        p.t.circle(s.x + DOT, s.y + s.h / 2.0, DOT, if live { p.p.success } else { p.p.tertiary });
        p.label(status, theme::SMALL, Rect::new(s.x + 14.0, s.y, s.w - 14.0, s.h), LINE, p.p.secondary);
        for y in geo::RULES {
            p.rule(geo::RIGHT_X, y, geo::RIGHT_W);
        }
        let wifi = words::wifi_detail(self.state.xiro_wifi, &self.info);
        self.paint_row_text(p, geo::WIFI_ROW, words::WIFI_TITLE, &wifi, self.state.xiro_wifi == WifiState::On);
        self.paint_row_text(p, geo::AUTO_ROW, words::AUTOSTART_TITLE, words::AUTOSTART_DETAIL, false);
        let (x, w, y) = (geo::RIGHT_X, geo::RIGHT_W, geo::DEVICES_Y);
        p.label(words::DEVICES_TITLE, theme::STRONG, Rect::new(x, y, w, 20.0), LINE, p.p.text);
        p.label(words::DEVICES_DETAIL, theme::SMALL, Rect::new(x, y + 22.0, w, 34.0), PARAGRAPH, p.p.secondary);
    }

    /// A switch row's title and line (the row and its switch are the control).
    fn paint_row_text(&self, p: &Painter, row: Rect, title: &str, detail: &str, lit: bool) {
        p.label(title, theme::STRONG, Rect::new(row.x, row.y + 8.0, ROW_TEXT_W, 20.0), LINE, p.p.text);
        let colour = if lit { p.p.text } else { p.p.secondary };
        p.label(detail, theme::SMALL, Rect::new(row.x, row.y + 30.0, ROW_TEXT_W, 34.0), PARAGRAPH, colour);
    }

    fn paint_help(&self, p: &Painter) {
        let (x, w) = (geo::LEFT, 600.0);
        p.label(words::HELP_TITLE, theme::TITLE, Rect::new(x, 80.0, w, 32.0), LINE, p.p.text);
        let mut y = 132.0;
        for (heading, body) in words::HELP_SECTIONS {
            y = Self::paint_section(p, (x, w), y, heading, body, theme::BODY);
        }
        let side = (geo::RIGHT_X - 40.0, geo::RIGHT_W + 40.0);
        let print = words::fingerprint(&self.info);
        let next = Self::paint_section(p, side, 132.0, words::FINGERPRINT_LABEL, &print, theme::MONO);
        let addresses = words::addresses(&self.info);
        p.label(&addresses, theme::SMALL, Rect::new(side.0, next, side.1, 200.0), PARAGRAPH, p.p.tertiary);
    }

    /// A heading and its text in the column `(x, w)`; returns where the next section starts.
    fn paint_section(p: &Painter, (x, w): (f32, f32), y: f32, heading: &str, body: &str, font: Font) -> f32 {
        p.label(heading, theme::HEADING, Rect::new(x, y, w, 20.0), LINE, p.p.text);
        let height = p.g.text(body, font, w, 400.0, PARAGRAPH).map_or(40.0, |t| t.size().1);
        p.label(body, font, Rect::new(x, y + 26.0, w, height + 4.0), PARAGRAPH, p.p.secondary);
        y + 26.0 + height + 28.0
    }
}
