//! How each control looks in every state; shared by the main window and prompts.

use super::controls::{Channel, Control, Motion, Pointer, Rect, Style, ease};
use super::gfx::{CENTERED, Flow, Gfx, LINE, Target};
use super::theme::{self, Color, Font, Palette, radius};

const ICON_GAP: f32 = 7.0;
const SWITCH_W: f32 = 36.0;
const SWITCH_H: f32 = 20.0;
const KNOB_PAD: f32 = 3.0;
/// A tab's underline when selected.
const TAB_LINE: f32 = 2.0;
const DISABLED: f32 = 0.38;

pub(crate) struct Painter<'a> {
    pub t: &'a Target,
    pub g: &'a Gfx,
    pub p: &'static Palette,
    pub motion: &'a Motion,
    pub pointer: Pointer,
}

impl Painter<'_> {
    /// Text in a box; drawing nothing is the only failure mode.
    pub(crate) fn label(&self, text: &str, font: Font, r: Rect, flow: Flow, c: Color) {
        if let Ok(laid) = self.g.text(text, font, r.w, r.h, flow) {
            self.t.text(&laid, r.x, r.y, c);
        }
    }

    /// The width `text` needs on one line.
    pub(crate) fn measure(&self, text: &str, font: Font) -> f32 {
        self.g.text(text, font, 4000.0, 100.0, LINE).map_or(0.0, |t| t.size().0)
    }

    /// A one-pixel horizontal rule.
    pub(crate) fn rule(&self, x: f32, y: f32, w: f32) {
        self.t.fill(Rect::new(x, y, w, 1.0), 0.0, self.p.hairline);
    }

    pub(crate) fn control(&self, c: &Control) {
        let hover = self.motion.get((c.id, Channel::Hover));
        let press = self.motion.get((c.id, Channel::Press));
        let dim = if c.enabled { 1.0 } else { DISABLED };
        match c.style {
            Style::Primary => self.primary(c, hover, press, dim),
            Style::Secondary => self.secondary(c, hover, press, dim),
            Style::Danger => self.danger(c, hover, dim),
            Style::Quiet => self.content(c, c.rect, self.p.secondary.mix(self.p.text, hover).alpha(dim), theme::SMALL, false),
            Style::QuietDanger => self.content(c, c.rect, self.p.danger.alpha((0.8 + 0.2 * hover) * dim), theme::SMALL, false),
            Style::Switch => self.switch_row(c, hover, dim),
            Style::Segment => self.tab(c, hover, dim),
        }
        if self.pointer.keyboard && self.pointer.focus == Some(c.id) {
            let rounding = if matches!(c.style, Style::Switch | Style::Segment) { radius::CONTROL } else { radius::CONTROL + 2.0 };
            self.t.stroke(c.rect.inset(-3.0), rounding, self.p.text.alpha(0.85), 1.5);
        }
    }

    /// Ink on the text colour: white on dark, black on light. No colour, no light effects.
    fn primary(&self, c: &Control, hover: f32, press: f32, dim: f32) {
        let fill = self.p.text.mix(self.p.secondary, 0.18 * hover + 0.3 * press).alpha(dim);
        self.t.fill(c.rect, radius::CONTROL, fill);
        self.content(c, c.rect, self.p.ink.alpha(dim), theme::STRONG, true);
    }

    fn secondary(&self, c: &Control, hover: f32, press: f32, dim: f32) {
        let fill = self.p.raised.mix(self.p.text, 0.05 * hover + 0.04 * press).alpha(dim);
        self.t.fill(c.rect, radius::CONTROL, fill);
        self.t.stroke(c.rect, radius::CONTROL, self.p.hairline, 1.0);
        self.content(c, c.rect, self.p.text.alpha(dim), theme::STRONG, true);
    }

    fn danger(&self, c: &Control, hover: f32, dim: f32) {
        self.t.fill(c.rect, radius::CONTROL, self.p.danger.alpha((0.12 + 0.08 * hover) * dim));
        self.content(c, c.rect, self.p.danger.alpha(dim), theme::STRONG, true);
    }

    /// Icon and label, centred together in `r` (or from its left edge).
    fn content(&self, c: &Control, r: Rect, color: Color, font: Font, centred: bool) {
        let label_w = self.measure(&c.label, font);
        let icon_w = match (c.icon, c.label.is_empty()) {
            (None, _) => 0.0,
            (Some(_), true) => theme::ICON.size,
            (Some(_), false) => theme::ICON.size + ICON_GAP,
        };
        let start = if centred { r.x + (r.w - label_w - icon_w).max(0.0) / 2.0 } else { r.x };
        if let Some(icon) = c.icon {
            let ir = Rect::new(start, r.y, theme::ICON.size, r.h);
            self.label(&icon.to_string(), theme::ICON, ir, CENTERED, color);
        }
        let lr = Rect::new(start + icon_w, r.y, (r.w - icon_w).min(label_w + 2.0), r.h);
        self.label(&c.label, font, lr, LINE, color);
    }

    /// A settings row: faint wash on hover, switch at the right edge; the view draws its words.
    fn switch_row(&self, c: &Control, hover: f32, dim: f32) {
        if hover > 0.01 {
            self.t.fill(c.rect.inset(-1.0), radius::CONTROL, self.p.text.alpha(0.035 * hover));
        }
        let on = ease(self.motion.get((c.id, Channel::Switch)));
        let track = Rect::new(c.rect.right() - SWITCH_W, c.rect.y + (c.rect.h - SWITCH_H) / 2.0, SWITCH_W, SWITCH_H);
        let off_fill = self.p.text.alpha(if self.p.dark { 0.16 } else { 0.14 });
        self.t.fill(track, radius::PILL, off_fill.mix(self.p.text, on).alpha(dim));
        let knob_r = SWITCH_H / 2.0 - KNOB_PAD;
        let cx = track.x + SWITCH_H / 2.0 + (SWITCH_W - SWITCH_H) * on;
        let knob = self.p.text.mix(self.p.ink, on);
        let knob = if self.p.dark { knob } else { Color::hex(0xFFFFFF) };
        self.t.circle(cx, track.y + SWITCH_H / 2.0, knob_r, knob.alpha(dim));
    }

    /// A tab: its words, and a short rule under the selected one.
    fn tab(&self, c: &Control, hover: f32, dim: f32) {
        let on = self.motion.get((c.id, Channel::Switch));
        let text = self.p.tertiary.mix(self.p.text, on.max(hover * 0.7)).alpha(dim);
        self.content(c, c.rect, text, theme::STRONG, false);
        if on > 0.01 {
            let w = self.measure(&c.label, theme::STRONG);
            self.t.fill(Rect::new(c.rect.x, c.rect.bottom() - TAB_LINE, w * on, TAB_LINE), 1.0, self.p.text.alpha(on * dim));
        }
    }
}
