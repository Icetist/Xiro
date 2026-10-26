//! The device screens that have connected, newest first. The display driver can only offer
//! a few sizes, so the elevated step gives these theirs before any other model's.

use std::path::PathBuf;

use xiro_display::Size;

use crate::config;
use crate::protocol::PANELS;

const FILE: &str = "screens.txt";
/// How many are remembered; every iPad gets the rest of the driver's sizes.
const KEEP: usize = 2;

fn file() -> PathBuf {
    config::data_dir().join(FILE)
}

/// The panel as listed in PANELS (landscape), if it is a known one.
fn known(size: Size) -> Option<Size> {
    let landscape = Size { width: size.width.max(size.height), height: size.width.min(size.height) };
    PANELS.iter().any(|&(w, h, _)| (w, h) == (landscape.width, landscape.height)).then_some(landscape)
}

/// Reads `2556x1179,2360x1640`: known panels only, each once, at most [`KEEP`].
pub fn parse(text: &str) -> Vec<Size> {
    let mut out: Vec<Size> = Vec::new();
    for item in text.split(',') {
        let Some((w, h)) = item.trim().split_once('x') else { continue };
        let (Ok(width), Ok(height)) = (w.parse(), h.parse()) else { continue };
        if let Some(panel) = known(Size { width, height }) {
            if out.len() < KEEP && !out.contains(&panel) {
                out.push(panel);
            }
        }
    }
    out
}

pub fn format(screens: &[Size]) -> String {
    screens.iter().map(|s| format!("{}x{}", s.width, s.height)).collect::<Vec<_>>().join(",")
}

pub fn load() -> Vec<Size> {
    std::fs::read_to_string(file()).map(|text| parse(&text)).unwrap_or_default()
}

/// `screens` with `panel` moved to the front, if it is a known panel.
fn with_first(screens: &[Size], panel: Size) -> Vec<Size> {
    let Some(panel) = known(panel) else { return screens.to_vec() };
    parse(&format(&[&[panel], screens].concat()))
}

/// Remembers that this panel connected. Best effort: at worst its sizes aren't kept first.
pub fn record(panel: Size) {
    let screens = load();
    let next = with_first(&screens, panel);
    if next != screens {
        let _ = std::fs::write(file(), format(&next));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn size(width: u32, height: u32) -> Size {
        Size { width, height }
    }

    #[test]
    fn only_known_panels_are_read_in_landscape_once_each() {
        assert_eq!(parse("1179x2556, 2360x1640,2556x1179,123x45,junk,x,"), vec![size(2556, 1179), size(2360, 1640)]);
        assert_eq!(parse(""), vec![]);
        assert_eq!(parse("2360x1640,2048x1536,2752x2064").len(), KEEP);
    }

    #[test]
    fn the_newest_screen_comes_first() {
        let screens = vec![size(2360, 1640), size(2048, 1536)];
        assert_eq!(with_first(&screens, size(1179, 2556)), vec![size(2556, 1179), size(2360, 1640)]);
        assert_eq!(with_first(&screens, size(2048, 1536)), vec![size(2048, 1536), size(2360, 1640)]);
        assert_eq!(with_first(&screens, size(100, 100)), screens, "unknown panels aren't kept");
        assert_eq!(parse(&format(&screens)), screens);
    }
}
