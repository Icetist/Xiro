//! Controls as plain data: layout, looks, pointer and keyboard focus, animation springs.
//! No drawing or Win32 here, so it is unit-tested.

/// A rectangle in DIPs (1/96 inch).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub(crate) const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub(crate) fn right(&self) -> f32 {
        self.x + self.w
    }

    pub(crate) fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub(crate) fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// Shrunk by `d` on every side (grown when negative).
    pub(crate) fn inset(&self, d: f32) -> Self {
        Self { x: self.x + d, y: self.y + d, w: (self.w - 2.0 * d).max(0.0), h: (self.h - 2.0 * d).max(0.0) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Style {
    /// Solid: the one thing to do next.
    Primary,
    Secondary,
    /// Destructive: red text on a quiet fill.
    Danger,
    /// Text only, for footers.
    Quiet,
    /// Text only, red: a destructive footer action.
    QuietDanger,
    /// A settings row that toggles; the switch sits at its right edge, `Control::on` is its state.
    Switch,
    /// One tab of a row of tabs; `on` marks the selected one.
    Segment,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Control {
    pub id: u16,
    pub style: Style,
    pub rect: Rect,
    pub label: String,
    pub icon: Option<char>,
    pub enabled: bool,
    pub on: bool,
}

impl Control {
    pub(crate) fn new(id: u16, style: Style, rect: Rect, label: &str) -> Self {
        Self { id, style, rect, label: label.to_owned(), icon: None, enabled: true, on: false }
    }

    pub(crate) fn icon(mut self, icon: char) -> Self {
        self.icon = Some(icon);
        self
    }

    pub(crate) fn on(mut self, on: bool) -> Self {
        self.on = on;
        self
    }

    pub(crate) fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// The enabled control under `(x, y)`; later controls sit on top.
pub(crate) fn hit(controls: &[Control], x: f32, y: f32) -> Option<u16> {
    controls.iter().rev().find(|c| c.enabled && c.rect.contains(x, y)).map(|c| c.id)
}

/// The next enabled control in tab order after `current` (wrapping), or before it with `back`.
pub(crate) fn next_focus(controls: &[Control], current: Option<u16>, back: bool) -> Option<u16> {
    let enabled: Vec<u16> = controls.iter().filter(|c| c.enabled).map(|c| c.id).collect();
    if enabled.is_empty() {
        return None;
    }
    let n = enabled.len();
    let at = current.and_then(|id| enabled.iter().position(|&e| e == id));
    let next = match (at, back) {
        (None, false) => 0,
        (None, true) => n - 1,
        (Some(i), false) => (i + 1) % n,
        (Some(i), true) => (i + n - 1) % n,
    };
    Some(enabled[next])
}

/// What the pointer and keyboard are doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pointer {
    pub hover: Option<u16>,
    /// Pressed with the mouse; a click lands when released over the same control.
    pub pressed: Option<u16>,
    pub focus: Option<u16>,
    /// The focus ring shows only after keyboard use, as elsewhere in Windows.
    pub keyboard: bool,
}

/// Which animated value of a control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Channel {
    Hover,
    Switch,
    Press,
}

/// Values that glide towards their targets, like a critically damped spring.
#[derive(Debug, Default)]
pub(crate) struct Motion {
    tracks: Vec<((u16, Channel), f32, f32)>,
}

/// Time constant: ~95% of the way in three of these.
const TAU_SECONDS: f32 = 0.055;
const SETTLED: f32 = 0.002;

impl Motion {
    /// Moves `key` towards `target`; a new key starts at its target so nothing flashes in.
    pub(crate) fn set(&mut self, key: (u16, Channel), target: f32) {
        match self.tracks.iter_mut().find(|t| t.0 == key) {
            Some(track) => track.2 = target,
            None => self.tracks.push((key, target, target)),
        }
    }

    pub(crate) fn get(&self, key: (u16, Channel)) -> f32 {
        self.tracks.iter().find(|t| t.0 == key).map_or(0.0, |t| t.1)
    }

    /// Advances by `dt` seconds; true while anything still moves.
    pub(crate) fn step(&mut self, dt: f32) -> bool {
        let k = 1.0 - (-dt.max(0.0) / TAU_SECONDS).exp();
        let mut moving = false;
        for track in &mut self.tracks {
            track.1 += (track.2 - track.1) * k;
            if (track.2 - track.1).abs() < SETTLED {
                track.1 = track.2;
            } else {
                moving = true;
            }
        }
        moving
    }
}

/// Ease for drawing: smooth start and end.
pub(crate) fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn three() -> Vec<Control> {
        vec![
            Control::new(1, Style::Primary, Rect::new(0.0, 0.0, 10.0, 10.0), "a"),
            Control::new(2, Style::Secondary, Rect::new(20.0, 0.0, 10.0, 10.0), "b").enabled(false),
            Control::new(3, Style::Quiet, Rect::new(5.0, 5.0, 10.0, 10.0), "c"),
        ]
    }

    #[test]
    fn rects_contain_their_inside_only() {
        let r = Rect::new(10.0, 20.0, 30.0, 40.0);
        assert!(r.contains(10.0, 20.0) && r.contains(39.9, 59.9));
        assert!(!r.contains(40.0, 30.0) && !r.contains(9.9, 30.0));
        assert_eq!(r.inset(5.0), Rect::new(15.0, 25.0, 20.0, 30.0));
        assert_eq!(r.inset(100.0).w, 0.0);
    }

    #[test]
    fn hits_skip_disabled_controls_and_prefer_the_topmost() {
        let c = three();
        assert_eq!(hit(&c, 1.0, 1.0), Some(1));
        assert_eq!(hit(&c, 7.0, 7.0), Some(3), "drawn last, on top");
        assert_eq!(hit(&c, 25.0, 5.0), None, "disabled");
        assert_eq!(hit(&c, 100.0, 100.0), None);
    }

    #[test]
    fn tab_order_wraps_and_skips_disabled_controls() {
        let c = three();
        assert_eq!(next_focus(&c, None, false), Some(1));
        assert_eq!(next_focus(&c, Some(1), false), Some(3));
        assert_eq!(next_focus(&c, Some(3), false), Some(1));
        assert_eq!(next_focus(&c, Some(1), true), Some(3));
        assert_eq!(next_focus(&c, None, true), Some(3));
        assert_eq!(next_focus(&[], None, false), None);
    }

    #[test]
    fn motion_starts_settled_then_glides_to_new_targets() {
        let key = (7, Channel::Switch);
        let mut m = Motion::default();
        m.set(key, 1.0);
        assert_eq!(m.get(key), 1.0, "new values start settled");
        assert!(!m.step(0.016));
        m.set(key, 0.0);
        assert!(m.step(0.016));
        let mid = m.get(key);
        assert!(mid > 0.0 && mid < 1.0);
        for _ in 0..60 {
            m.step(0.016);
        }
        assert_eq!(m.get(key), 0.0);
        assert_eq!(m.get((8, Channel::Hover)), 0.0, "unknown values are 0");
    }

    #[test]
    fn ease_is_clamped_and_symmetric() {
        assert_eq!(ease(-1.0), 0.0);
        assert_eq!(ease(2.0), 1.0);
        assert!((ease(0.5) - 0.5).abs() < 1e-6);
    }
}
