//! Coordinate mapping: normalised client position -> monitor pixel -> SendInput absolute units.

/// A desktop rectangle in physical pixels (also used for the virtual-screen bounds).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    /// A zero-area rect cannot host a pointer (nothing has been targeted yet).
    pub fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }
}

/// A pixel on the virtual desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ScreenPoint {
    pub x: i32,
    pub y: i32,
}

/// SendInput absolute coordinates span 0..=65535 across the virtual desktop.
const ABS_MAX: f64 = 65535.0;

/// Rounds halves toward +infinity, like JavaScript `Math.round`.
pub(crate) fn js_round(v: f64) -> f64 {
    (v + 0.5).floor()
}

/// Normalised `[0,1]` position -> physical pixel on the target monitor.
pub fn to_screen_point(nx: f64, ny: f64, rect: Rect) -> ScreenPoint {
    ScreenPoint {
        x: rect.left + js_round(nx * f64::from(rect.width - 1)) as i32,
        y: rect.top + js_round(ny * f64::from(rect.height - 1)) as i32,
    }
}

/// Pixel -> SendInput absolute units spanning the whole virtual desktop (`MOUSEEVENTF_VIRTUALDESK`).
pub fn to_absolute(point: ScreenPoint, vs: Rect) -> (i32, i32) {
    let scale = |offset: i32, extent: i32| js_round(f64::from(offset) * ABS_MAX / f64::from((extent - 1).max(1))) as i32;
    (scale(point.x - vs.left, vs.width), scale(point.y - vs.top, vs.height))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VS: Rect = Rect { left: -1920, top: 0, width: 1920 + 2360, height: 1640 };

    #[test]
    fn maps_normalised_points_onto_the_monitor_and_virtual_desktop() {
        let rect = Rect { left: 0, top: 0, width: 2360, height: 1640 };
        assert_eq!(to_screen_point(0.0, 0.0, rect), ScreenPoint { x: 0, y: 0 });
        assert_eq!(to_screen_point(1.0, 1.0, rect), ScreenPoint { x: 2359, y: 1639 });
        assert_eq!(to_absolute(ScreenPoint { x: -1920, y: 0 }, VS), (0, 0));
        assert_eq!(to_absolute(ScreenPoint { x: 2359, y: 1639 }, VS), (65535, 65535));
    }

    #[test]
    fn offsets_by_monitor_origin() {
        let rect = Rect { left: -1920, top: 100, width: 1920, height: 1080 };
        assert_eq!(to_screen_point(0.5, 0.0, rect), ScreenPoint { x: -1920 + 960, y: 100 });
    }

    #[test]
    fn degenerate_virtual_screen_does_not_divide_by_zero() {
        let vs = Rect { left: 0, top: 0, width: 1, height: 0 };
        assert_eq!(to_absolute(ScreenPoint { x: 1, y: 1 }, vs), (65535, 65535));
    }

    #[test]
    fn js_round_matches_math_round_on_halves() {
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert_eq!(js_round(-2.6), -3.0);
    }

    #[test]
    fn empty_rect_detection() {
        assert!(Rect::default().is_empty());
        assert!(!VS.is_empty());
    }
}
