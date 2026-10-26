//! Picks the virtual display's stream mode: native at full refresh if the encoder keeps up, else
//! fewer frames, then half size. Frame rate goes first because each frame interval is latency.

use xiro_display::{Size, stream_sizes};

const PASS_RATIO: f64 = 0.85;
const BASE_FPS: u32 = 60;
/// Achieved frame rates are rounded down to a multiple of this.
const FPS_STEP: u32 = 5;

/// What one encoder test measured at one size.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeOutcome {
    pub encoder: String,
    pub achieved_fps: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamMode {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub scale: u32,
    pub encoder: String,
}

fn sustains(achieved: f64, fps: u32) -> bool {
    achieved >= fps as f64 * PASS_RATIO
}

/// Never ask the encoder for more frames than it delivers: they would only queue.
pub fn sustainable_fps(wanted: u32, achieved: f64, min_fps: u32) -> u32 {
    if sustains(achieved, wanted) {
        wanted
    } else {
        min_fps.max((achieved as u32 / FPS_STEP) * FPS_STEP).min(wanted)
    }
}

/// `probe` shows the display at a size and measures the best encoder there.
/// `native_scale` lays the native size out like the device's points; half size uses half of it.
pub fn choose_stream_mode<E>(
    panel: Size,
    native_scale: u32,
    fps: u32,
    min_fps: u32,
    mut probe: impl FnMut(Size, u32) -> Result<ProbeOutcome, E>,
) -> Result<StreamMode, E> {
    let [native, half] = stream_sizes(panel);
    let pick = |size: Size, scale: u32, result: &ProbeOutcome| {
        let mode = |fps| StreamMode { width: size.width, height: size.height, fps, scale, encoder: result.encoder.clone() };
        if sustains(result.achieved_fps, fps) {
            Some(mode(fps))
        } else if fps > BASE_FPS && sustains(result.achieved_fps, BASE_FPS) {
            Some(mode(BASE_FPS))
        } else {
            None
        }
    };
    let at_native = probe(native, fps)?;
    if let Some(mode) = pick(native, native_scale, &at_native) {
        return Ok(mode);
    }
    // Small iPhones have no half size the display driver can show: stay native.
    let (fallback, scale, result) = if half == native {
        (native, native_scale, at_native)
    } else {
        let at_half = probe(half, fps)?;
        if let Some(mode) = pick(half, native_scale / 2, &at_half) {
            return Ok(mode);
        }
        (half, native_scale / 2, at_half)
    };
    // Nothing sustains 60 fps: stream at what the encoder can really do, so frames never queue.
    let capped = sustainable_fps(fps, result.achieved_fps, min_fps);
    Ok(StreamMode { width: fallback.width, height: fallback.height, fps: capped, scale, encoder: result.encoder })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PANEL: Size = Size { width: 2360, height: 1640 };

    fn run(native: f64, half: f64, fps: u32) -> (StreamMode, Vec<(u32, u32)>) {
        let mut calls = Vec::new();
        let mode = choose_stream_mode::<()>(PANEL, 200, fps, 15, |size, f| {
            calls.push((size.width, f));
            let achieved = if size.width == PANEL.width { native } else { half };
            Ok(ProbeOutcome { encoder: format!("enc{}", size.width), achieved_fps: achieved })
        })
        .unwrap();
        (mode, calls)
    }

    #[test]
    fn native_size_at_full_rate_when_the_encoder_keeps_up() {
        let (mode, calls) = run(118.0, 0.0, 120);
        assert_eq!(mode, StreamMode { width: 2360, height: 1640, fps: 120, scale: 200, encoder: "enc2360".into() });
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn frame_rate_is_cut_before_resolution() {
        let (mode, _) = run(70.0, 0.0, 120);
        assert_eq!((mode.width, mode.fps, mode.scale), (2360, 60, 200));
    }

    #[test]
    fn half_size_when_native_is_too_slow() {
        let (mode, calls) = run(30.0, 100.0, 120);
        assert_eq!((mode.width, mode.height, mode.fps, mode.scale), (1180, 820, 60, 100));
        assert_eq!(calls, vec![(2360, 120), (1180, 120)]);
    }

    #[test]
    fn half_size_at_what_the_encoder_really_does() {
        let (mode, _) = run(10.0, 37.0, 60);
        assert_eq!((mode.width, mode.fps), (1180, 35));
        assert_eq!(run(1.0, 3.0, 60).0.fps, 15, "never below the floor");
    }

    #[test]
    fn sustainable_fps_rounds_down_and_respects_bounds() {
        assert_eq!(sustainable_fps(60, 59.0, 15), 60);
        assert_eq!(sustainable_fps(60, 44.0, 15), 40);
        assert_eq!(sustainable_fps(60, 2.0, 15), 15);
        assert_eq!(sustainable_fps(10, 2.0, 15), 10, "never above what was asked");
    }

    #[test]
    fn half_size_uses_half_the_device_scale() {
        let mode = choose_stream_mode::<()>(Size { width: 1488, height: 2266 }, 200, 60, 15, |size, _| {
            Ok(ProbeOutcome { encoder: "e".into(), achieved_fps: if size.width == 1488 { 10.0 } else { 60.0 } })
        })
        .unwrap();
        assert_eq!((mode.width, mode.height, mode.scale), (744, 1134, 100));
    }

    #[test]
    fn a_phone_without_a_half_size_stays_native_at_what_the_encoder_manages() {
        let mut probes = 0;
        let mode = choose_stream_mode::<()>(Size { width: 1179, height: 2556 }, 300, 60, 15, |_, _| {
            probes += 1;
            Ok(ProbeOutcome { encoder: "e".into(), achieved_fps: 31.0 })
        })
        .unwrap();
        assert_eq!((mode.width, mode.height, mode.scale, mode.fps), (1179, 2556, 300, 30));
        assert_eq!(probes, 1, "no second probe of the same size");
    }

    #[test]
    fn probe_errors_propagate() {
        let result = choose_stream_mode(PANEL, 300, 60, 15, |_, _| Err::<ProbeOutcome, _>("no encoder"));
        assert_eq!(result.unwrap_err(), "no encoder");
    }
}
