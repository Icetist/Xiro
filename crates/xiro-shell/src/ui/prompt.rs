//! Xiro's dialogs. A button can stay disabled briefly so a stray keypress can't answer,
//! and a countdown can answer when nobody does.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, DispatchMessageW, GetMessageW, IsWindow, MSG, PostQuitMessage, SW_SHOWNORMAL, SetForegroundWindow, ShowWindow,
    TranslateMessage,
};

use super::controls::{Control, Rect, Style};
use super::frame::{self, Options, Reply, View};
use super::gfx::{CENTERED, Gfx, PARAGRAPH};
use super::paint::Painter;
use super::theme::{self, glyph, radius};

const WIDTH: f32 = 460.0;
const PAD: f32 = 32.0;
const BADGE: f32 = 44.0;
const BUTTON_H: f32 = 36.0;
const BUTTON_MIN_W: f32 = 96.0;
const BUTTON_PAD: f32 = 36.0;
const GAP: f32 = 8.0;
/// The countdown line moves in visible steps; nothing else needs a faster clock.
const TICK: Duration = Duration::from_millis(100);

/// The symbol at the top of the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    Device,
    Shield,
    Unlink,
    Remove,
    Done,
}

impl Badge {
    fn glyph(self) -> char {
        match self {
            Self::Device => glyph::DEVICE,
            Self::Shield => glyph::SHIELD,
            Self::Unlink => glyph::UNLINK,
            Self::Remove => glyph::DELETE,
            Self::Done => glyph::CHECK,
        }
    }
}

/// How a button looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// The suggested action.
    Main,
    Plain,
    /// Removes something.
    Destructive,
}

/// A question for the person at the PC.
#[derive(Clone, Debug)]
pub struct Prompt {
    /// The window's title bar.
    pub caption: &'static str,
    pub badge: Badge,
    pub title: String,
    pub body: String,
    /// A short fact shown in a chip, e.g. the device's address.
    pub detail: Option<String>,
    /// Left to right.
    pub buttons: Vec<(String, Choice)>,
    /// Enter, when no button has keyboard focus.
    pub default: usize,
    /// Esc, the close button and the countdown.
    pub cancel: usize,
    /// This button stays disabled for this long.
    pub arm: Option<(usize, Duration)>,
    /// Nobody answering within this time means `cancel`.
    pub timeout: Option<Duration>,
    pub topmost: bool,
}

struct PromptView {
    prompt: Prompt,
    opened: Instant,
    now: Instant,
    answer: Rc<Cell<Option<usize>>>,
    body_h: Cell<f32>,
    title_h: Cell<f32>,
    /// Measured in `size`, which always runs before the controls are needed.
    button_w: RefCell<Vec<f32>>,
}

impl PromptView {
    fn armed(&self, index: usize) -> bool {
        match self.prompt.arm {
            Some((armed, delay)) if armed == index => self.now.duration_since(self.opened) >= delay,
            _ => true,
        }
    }

    fn remaining(&self) -> Option<f32> {
        let timeout = self.prompt.timeout?;
        let left = timeout.saturating_sub(self.now.duration_since(self.opened));
        Some(left.as_secs_f32() / timeout.as_secs_f32().max(0.001))
    }

    fn text_top(&self) -> f32 {
        PAD + BADGE + 22.0
    }

    fn buttons_top(&self) -> f32 {
        let mut y = self.text_top() + self.title_h.get() + 8.0 + self.body_h.get();
        if self.prompt.detail.is_some() {
            y += 16.0 + 32.0;
        }
        y + 30.0
    }

    fn answer(&mut self, index: usize) -> Reply {
        self.answer.set(Some(index));
        Reply::Close
    }
}

impl View for PromptView {
    fn size(&self, gfx: &Gfx) -> (f32, f32) {
        let inner = WIDTH - 2.0 * PAD;
        let title = gfx.text(&self.prompt.title, theme::TITLE, inner, 400.0, PARAGRAPH).map_or(28.0, |t| t.size().1);
        let body = gfx.text(&self.prompt.body, theme::BODY, inner, 1000.0, PARAGRAPH).map_or(60.0, |t| t.size().1);
        self.title_h.set(title.ceil());
        self.body_h.set(body.ceil());
        let widths = self.prompt.buttons.iter().map(|(label, _)| {
            let text = gfx.text(label, theme::STRONG, 1000.0, 100.0, CENTERED).map_or(60.0, |t| t.size().0);
            (text + BUTTON_PAD).max(BUTTON_MIN_W).ceil()
        });
        *self.button_w.borrow_mut() = widths.collect();
        (WIDTH, self.buttons_top() + BUTTON_H + PAD)
    }

    fn controls(&self) -> Vec<Control> {
        let y = self.buttons_top();
        let widths: Vec<f32> = self.button_w.borrow().clone();
        let total: f32 = widths.iter().sum::<f32>() + GAP * (widths.len().saturating_sub(1)) as f32;
        let mut x = WIDTH - PAD - total;
        let mut out = Vec::new();
        for (i, ((label, choice), w)) in self.prompt.buttons.iter().zip(widths).enumerate() {
            let style = match choice {
                Choice::Main => Style::Primary,
                Choice::Plain => Style::Secondary,
                Choice::Destructive => Style::Danger,
            };
            out.push(Control::new(i as u16 + 1, style, Rect::new(x, y, w, BUTTON_H), label).enabled(self.armed(i)));
            x += w + GAP;
        }
        out
    }

    fn paint(&self, p: &Painter, size: (f32, f32)) {
        let accent = match self.prompt.badge {
            Badge::Remove => p.p.danger,
            Badge::Done => p.p.success,
            _ => p.p.text,
        };
        let badge = Rect::new(PAD, PAD, BADGE, BADGE);
        p.t.fill(badge, radius::CONTROL + 3.0, p.p.surface);
        p.t.stroke(badge, radius::CONTROL + 3.0, p.p.hairline, 1.0);
        p.label(&self.prompt.badge.glyph().to_string(), theme::Font { size: 18.0, ..theme::ICON }, badge, CENTERED, accent);
        let inner = WIDTH - 2.0 * PAD;
        let top = self.text_top();
        p.label(&self.prompt.title, theme::TITLE, Rect::new(PAD, top, inner, self.title_h.get() + 4.0), PARAGRAPH, p.p.text);
        let body_top = top + self.title_h.get() + 8.0;
        p.label(&self.prompt.body, theme::BODY, Rect::new(PAD, body_top, inner, self.body_h.get() + 4.0), PARAGRAPH, p.p.secondary);
        if let Some(detail) = &self.prompt.detail {
            let w = p.measure(detail, theme::MONO) + 28.0;
            let chip = Rect::new(PAD, body_top + self.body_h.get() + 16.0, w, 32.0);
            p.t.fill(chip, radius::CONTROL, p.p.surface);
            p.t.stroke(chip, radius::CONTROL, p.p.hairline, 1.0);
            p.label(detail, theme::MONO, chip, CENTERED, p.p.text);
        }
        if let Some(left) = self.remaining() {
            self.paint_countdown(p, size, left);
        }
        for c in self.controls() {
            p.control(&c);
        }
    }

    fn activate(&mut self, id: u16) -> Reply {
        let index = usize::from(id.saturating_sub(1));
        if index < self.prompt.buttons.len() && self.armed(index) { self.answer(index) } else { Reply::Nothing }
    }

    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn dismiss(&mut self) -> Reply {
        let cancel = self.prompt.cancel;
        self.answer(cancel)
    }

    fn default_control(&self) -> Option<u16> {
        Some(self.prompt.default as u16 + 1)
    }

    fn enter_means_default(&self) -> bool {
        self.prompt.arm.is_some()
    }

    fn tick_every(&self) -> Option<Duration> {
        (self.prompt.arm.is_some() || self.prompt.timeout.is_some()).then_some(TICK)
    }

    fn tick(&mut self, now: Instant) -> Reply {
        let was_armed: Vec<bool> = (0..self.prompt.buttons.len()).map(|i| self.armed(i)).collect();
        self.now = now;
        if self.remaining() == Some(0.0) {
            let cancel = self.prompt.cancel;
            return self.answer(cancel);
        }
        let armed_now = (0..self.prompt.buttons.len()).any(|i| self.armed(i) != was_armed[i]);
        if armed_now || self.prompt.timeout.is_some() { Reply::Repaint } else { Reply::Nothing }
    }
}

impl PromptView {
    /// A line along the bottom that drains as time runs out.
    fn paint_countdown(&self, p: &Painter, size: (f32, f32), left: f32) {
        let y = size.1 - 2.0;
        p.t.fill(Rect::new(0.0, y, size.0, 2.0), 0.0, p.p.hairline);
        p.t.fill(Rect::new(0.0, y, size.0 * left, 2.0), 0.0, p.p.text.alpha(0.6));
    }
}

/// Shows `prompt` and returns the chosen button; `None` if it can't be shown (use a message box).
/// Runs its own message loop, so it is callable from any thread.
pub fn ask(prompt: Prompt) -> Option<usize> {
    let answer = Rc::new(Cell::new(None));
    let options = Options { title: prompt.caption, topmost: prompt.topmost, dialog: true };
    let cancel = prompt.cancel;
    let now = Instant::now();
    let view = PromptView { prompt, opened: now, now, answer: answer.clone(), body_h: Cell::new(0.0), title_h: Cell::new(0.0), button_w: RefCell::new(Vec::new()) };
    let hwnd = frame::create(Box::new(view), &options).ok()?;
    // SAFETY: showing and focusing our own new window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(hwnd);
    }
    let mut msg = MSG::default();
    // SAFETY: a standard message pump on this thread, until our window is gone.
    unsafe {
        while IsWindow(Some(hwnd)).as_bool() {
            match GetMessageW(&mut msg, None, 0, 0).0 {
                0 => {
                    PostQuitMessage(msg.wParam.0 as i32); // leave WM_QUIT for the outer loop
                    break;
                }
                -1 => break,
                _ => {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        // Left early (WM_QUIT): the card must not stay on screen with its timers running.
        if IsWindow(Some(hwnd)).as_bool() {
            let _ = DestroyWindow(hwnd);
        }
    }
    Some(answer.get().unwrap_or(cancel))
}
