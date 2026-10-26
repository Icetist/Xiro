//! Wire protocol shared with the Safari client (web/protocol.js). Binary messages
//! start with a one-byte type tag; control messages are JSON text frames.

use serde_json::Value;

use crate::config::{BITRATE_MBPS_RANGE, FPS_RANGE};

pub const MSG_VIDEO: u8 = 0x01;
pub const MSG_POINTER: u8 = 0x10;
pub const MSG_WHEEL: u8 = 0x11;

/// VIDEO: [type u8][flags u8][sentAt f64 LE (ms, host clock)][AVCC sample...]
pub const VIDEO_HEADER_BYTES: usize = 10;
pub const VIDEO_FLAG_KEY: u8 = 0x01;
/// Catch-up frame re-sent from the GOP cache; the client decodes it even if behind.
pub const VIDEO_FLAG_REPLAY: u8 = 0x02;

pub fn video_message(is_key: bool, sent_at: f64, sample: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(VIDEO_HEADER_BYTES + sample.len());
    message.push(MSG_VIDEO);
    message.push(if is_key { VIDEO_FLAG_KEY } else { 0 });
    message.extend_from_slice(&sent_at.to_le_bytes());
    message.extend_from_slice(sample);
    message
}

/// An iPad panel in its current orientation, and its refresh class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Screen {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
    /// Windows "Scale" (percent) that lays the monitor out like the device's own points.
    pub scale: u32,
}

/// Known iPad/iPhone screens (landscape px = points x devicePixelRatio, iOS 16.4+) and density.
/// The virtual monitor only takes these shapes, so a viewer can't register arbitrary display modes.
pub const PANELS: [(u32, u32, u32); 21] = [
    (2752, 2064, 200), // iPad Pro 13" (M4 and later)
    (2732, 2048, 200), // iPad Pro 12.9", iPad Air 13"
    (2420, 1668, 200), // iPad Pro 11" (M4 and later)
    (2388, 1668, 200), // iPad Pro 11" (1st-4th gen)
    (2360, 1640, 200), // iPad Air 11" / 4th-5th gen, iPad 10th gen and later
    (2266, 1488, 200), // iPad mini 6th gen and later
    (2224, 1668, 200), // iPad Pro 10.5", iPad Air 3rd gen
    (2160, 1620, 200), // iPad 10.2" (7th-9th gen)
    (2048, 1536, 200), // iPad 9.7", iPad mini 5th gen
    (2868, 1320, 300), // iPhone 16 Pro Max, 17 Pro Max
    (2796, 1290, 300), // iPhone 14 Pro Max, 15 Plus, 15 Pro Max, 16 Plus
    (2778, 1284, 300), // iPhone 12 Pro Max, 13 Pro Max, 14 Plus
    (2736, 1260, 300), // iPhone Air
    (2688, 1242, 300), // iPhone XS Max, 11 Pro Max
    (2622, 1206, 300), // iPhone 16 Pro, 17, 17 Pro
    (2556, 1179, 300), // iPhone 14 Pro, 15, 15 Pro, 16
    (2532, 1170, 300), // iPhone 12, 12 Pro, 13, 13 Pro, 14, 16e
    (2436, 1125, 300), // iPhone X, XS, 11 Pro, 12 mini, 13 mini
    (2208, 1242, 300), // iPhone 8 Plus
    (1792, 828, 200),  // iPhone XR, 11
    (1334, 750, 200),  // iPhone 8, SE (2nd and 3rd gen)
];
const SCREEN_HZ: (f64, f64) = (30.0, 240.0);
/// Above this the iPad is ProMotion; only 60 and 120 Hz matter for display modes.
const PROMOTION_THRESHOLD_HZ: f64 = 90.0;

fn in_range(value: &Value, (lo, hi): (f64, f64)) -> Option<f64> {
    value.as_f64().filter(|v| v.is_finite() && *v >= lo && *v <= hi)
}

/// Validates the iPad's panel description; None when it isn't a known iPad panel.
pub fn parse_screen(value: &Value) -> Option<Screen> {
    let width = u32::try_from(value.get("width")?.as_u64()?).ok()?;
    let height = u32::try_from(value.get("height")?.as_u64()?).ok()?;
    let &(_, _, scale) = PANELS.iter().find(|&&(w, h, _)| (width, height) == (w, h) || (width, height) == (h, w))?;
    let promotion = value.get("hz").and_then(|hz| in_range(hz, SCREEN_HZ)).is_some_and(|hz| hz > PROMOTION_THRESHOLD_HZ);
    Some(Screen { width, height, hz: if promotion { 120 } else { 60 }, scale })
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Settings {
    pub fps: Option<u32>,
    pub bitrate_mbps: Option<f64>,
}

impl Settings {
    pub fn is_empty(&self) -> bool {
        self.fps.is_none() && self.bitrate_mbps.is_none()
    }
}

/// Validates a client "settings" message; keeps only in-range numbers.
pub fn parse_settings(msg: &Value) -> Settings {
    Settings {
        fps: msg.get("fps").and_then(|v| in_range(v, FPS_RANGE)).map(|f| f.round() as u32),
        bitrate_mbps: msg.get("bitrateMbps").and_then(|v| in_range(v, BITRATE_MBPS_RANGE)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn video_header_layout_matches_the_client() {
        let m = video_message(true, 1234.5, &[9, 8]);
        assert_eq!(m.len(), VIDEO_HEADER_BYTES + 2);
        assert_eq!(&m[..2], &[MSG_VIDEO, VIDEO_FLAG_KEY]);
        assert_eq!(f64::from_le_bytes(m[2..10].try_into().unwrap()), 1234.5);
        assert_eq!(&m[10..], &[9, 8]);
        assert_eq!(video_message(false, 0.0, &[])[1], 0);
    }

    #[test]
    fn only_real_ipad_panels_are_accepted() {
        assert_eq!(parse_screen(&json!({ "width": 2360, "height": 1640, "hz": 60 })), Some(Screen { width: 2360, height: 1640, hz: 60, scale: 200 }));
        assert_eq!(parse_screen(&json!({ "width": 1640, "height": 2360, "hz": 120 })).unwrap().hz, 120);
        assert_eq!(parse_screen(&json!({ "width": 2752, "height": 2064, "hz": 500 })).unwrap().hz, 60);
        let iphone = parse_screen(&json!({ "width": 1179, "height": 2556, "hz": 120 })).unwrap();
        assert_eq!((iphone.scale, iphone.hz), (300, 120), "iPhones are 3x and may be ProMotion");
        assert_eq!(parse_screen(&json!({ "width": 1334, "height": 750 })).unwrap().scale, 200);
        assert!(parse_screen(&json!({ "width": 1920, "height": 1080 })).is_none());
        assert!(parse_screen(&json!({ "width": "2360", "height": 1640 })).is_none());
        assert!(parse_screen(&Value::Null).is_none());
    }

    #[test]
    fn settings_accept_only_in_range_numbers() {
        assert_eq!(parse_settings(&json!({ "fps": 119.6, "bitrateMbps": 40 })), Settings { fps: Some(120), bitrate_mbps: Some(40.0) });
        assert!(parse_settings(&json!({ "fps": 1000, "bitrateMbps": "40" })).is_empty());
        assert!(parse_settings(&Value::Null).is_empty());
    }
}
