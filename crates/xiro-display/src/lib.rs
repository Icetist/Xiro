//! Virtual monitor shaped like the connected iPad (Virtual Display Driver), on only while connected.
//! Only that monitor is ever changed; nothing needs admin except [`install::install_driver`].

mod ccd;
mod error;
mod gdi;
pub mod install;
pub mod secure;
mod layout;
mod platform;
mod topology;
mod trust;
mod vdd;
mod virtual_display;

pub use error::DisplayError;
pub use vdd::MAX_SIZES;
pub use virtual_display::{SIZES_NOT_SET_UP, VirtualDisplay};

/// A display size in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

/// A display mode: size in pixels and refresh rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
}

/// The result of [`VirtualDisplay::attach`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attached {
    /// GDI name of the monitor, e.g. `\\.\DISPLAY6`: what capture APIs match on.
    pub gdi_name: String,
    /// Refresh rate in use (60 when the wanted rate was refused).
    pub hz: u32,
    /// The monitor was turned on or switched mode, so capture should let it settle.
    pub changed: bool,
}

/// Half of `n` rounded to the nearest even number (halves up), at least 2: encoders need even sizes.
fn even_half(n: u32) -> u32 {
    let quarters = n / 4 + u32::from(n % 4 >= 2);
    quarters.max(1) * 2
}

/// Driver's documented limits; one size outside them in its settings stops it creating its monitor.
const DRIVER_WIDTHS: std::ops::RangeInclusive<u32> = 640..=7680;
const DRIVER_HEIGHTS: std::ops::RangeInclusive<u32> = 480..=4320;

pub fn fits_driver(size: Size) -> bool {
    DRIVER_WIDTHS.contains(&size.width) && DRIVER_HEIGHTS.contains(&size.height)
}

/// Native size plus an exact half for slow encoders (the iPad upscales 2x, staying sharp).
/// Both are native when the driver can't show the half in both orientations.
pub fn stream_sizes(panel: Size) -> [Size; 2] {
    let half = Size { width: even_half(panel.width), height: even_half(panel.height) };
    let rotated = Size { width: half.height, height: half.width };
    [panel, if fits_driver(half) && fits_driver(rotated) { half } else { panel }]
}

/// Adds the rotated variant of each size, so turning the iPad never needs a driver reload.
pub fn with_both_orientations(sizes: &[Size]) -> Vec<Size> {
    sizes.iter().flat_map(|s| [*s, Size { width: s.height, height: s.width }]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn size(width: u32, height: u32) -> Size {
        Size { width, height }
    }

    #[test]
    fn stream_sizes_are_the_native_panel_and_an_exact_half() {
        assert_eq!(stream_sizes(size(2360, 1640)), [size(2360, 1640), size(1180, 820)]);
        // Odd halves are rounded to even, which every encoder accepts.
        assert_eq!(stream_sizes(size(2266, 1488))[1], size(1134, 744));
        assert_eq!(with_both_orientations(&[size(4, 3)]), vec![size(4, 3), size(3, 4)]);
    }

    #[test]
    fn sizes_the_driver_cannot_show_are_never_offered() {
        assert!(fits_driver(size(640, 480)) && fits_driver(size(7680, 4320)));
        assert!(!fits_driver(size(639, 1000)) && !fits_driver(size(1000, 479)) && !fits_driver(size(7681, 1000)));
        // iPhone 15: its half (1278x590) is too narrow when upright, so it streams native only.
        assert_eq!(stream_sizes(size(2556, 1179)), [size(2556, 1179), size(2556, 1179)]);
        assert_eq!(stream_sizes(size(1179, 2556)), [size(1179, 2556), size(1179, 2556)]);
    }

    #[test]
    fn halves_match_javascript_rounding() {
        // JS: Math.max(2, Math.round(n / 2 / 2) * 2), where .5 rounds up.
        let js = |n: u32| ((f64::from(n) / 4.0 + 0.5).floor() as u32 * 2).max(2);
        for n in 0..5000 {
            assert_eq!(even_half(n), js(n), "{n}");
        }
    }

    #[test]
    fn virtual_display_can_be_shared_with_a_worker_thread() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<VirtualDisplay>();
    }
}
