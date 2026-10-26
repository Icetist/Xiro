//! Stream sources, both resolving to the same [`Stream`]: sidecar (on-demand virtual monitor
//! shaped like the iPad) and mirror (an existing monitor).

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use xiro_display::{Mode, Size, VirtualDisplay};
use xiro_video::{EncoderPreference, StreamSpec};

use crate::config::{read_json, write_json};
use crate::hub::{Provider, Stream};
use crate::log;
use crate::policy::{ProbeOutcome, choose_stream_mode, sustainable_fps};
use crate::protocol::Screen;

const HZ_HIGH: u32 = 120;
const HZ_BASE: u32 = 60;
pub const MIN_FPS: u32 = 15;
const DEFAULT_SCREEN: Screen = Screen { width: 2360, height: 1640, hz: 60, scale: 200 };
/// Desktop Duplication needs a moment after a display change (the video thread retries every 250 ms).
const SETTLE: Duration = Duration::from_millis(250);
/// Re-decide now and then: a mode picked while the PC was busy shouldn't stick forever.
const MODE_TTL_MS: u64 = 7 * 24 * 3600 * 1000;
/// Bumped when probing changes meaning, so older results are not trusted.
const CACHE_VERSION: &str = "native-v1";
const MIN_BITRATE: u32 = 4_000_000;
const MAX_BITRATE: u32 = 80_000_000;
const BITS_PER_PIXEL: f64 = 0.1;

/// Measures encoders for a stream: the real one runs `xiro_video::probe`.
pub type ProbeFn = Box<dyn Fn(&StreamSpec) -> Result<ProbeOutcome, String> + Send + Sync>;

#[derive(Debug, Clone)]
pub struct StreamSettings {
    /// User frame-rate cap; None = the iPad's refresh rate.
    pub fps_cap: Option<u32>,
    pub bitrate_mbps: Option<f64>,
    pub encoder: EncoderPreference,
    pub gop_seconds: f64,
}

impl StreamSettings {
    pub fn bitrate_for(&self, width: u32, height: u32, fps: u32) -> u32 {
        match self.bitrate_mbps {
            Some(mbps) => (mbps * 1e6).round() as u32,
            None => default_bitrate(width, height, fps),
        }
    }

    fn spec(&self, display: &str, size: Size, fps: u32) -> StreamSpec {
        StreamSpec {
            display: display.to_owned(),
            width: size.width,
            height: size.height,
            fps,
            bitrate: self.bitrate_for(size.width, size.height, fps),
            gop: ((fps as f64 * self.gop_seconds).round() as u32).max(1),
            encoder: self.encoder.clone(),
        }
    }

    fn cache_suffix(&self) -> String {
        let bitrate = self.bitrate_mbps.map_or("auto".into(), |b| b.to_string());
        format!("{:?}|{bitrate}", self.encoder)
    }
}

pub fn default_bitrate(width: u32, height: u32, fps: u32) -> u32 {
    let raw = ((width as f64 * height as f64 * fps as f64 * BITS_PER_PIXEL) / 100_000.0).round() as u32 * 100_000;
    raw.clamp(MIN_BITRATE, MAX_BITRATE)
}

/// The real encoder test: the winner if one sustains the rate, else the fastest.
pub fn real_probe(prober: xiro_video::Prober) -> ProbeFn {
    Box::new(move |spec| {
        let results = prober.probe(spec.clone(), PROBE_TIME)?;
        let winner = results.iter().find(|r| r.sustains(spec.fps)).or_else(|| {
            results.iter().filter(|r| r.valid).max_by(|a, b| a.achieved_fps.total_cmp(&b.achieved_fps))
        });
        for r in &results {
            log::info!("  {}: {:.0} fps{}", r.encoder, r.achieved_fps, if r.valid { "" } else { " (no usable stream)" });
        }
        winner
            .map(|r| ProbeOutcome { encoder: r.encoder.clone(), achieved_fps: r.achieved_fps })
            .ok_or_else(|| "No working H.264 encoder found on this PC".to_owned())
    })
}

const PROBE_TIME: Duration = Duration::from_millis(1200);

/// The chosen mode per iPad, so reconnecting skips the probing sequence.
pub struct ModeCache {
    file: PathBuf,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

impl ModeCache {
    pub fn new(file: PathBuf) -> Self {
        Self { file }
    }

    pub fn clear(&self) {
        let _ = write_json(&self.file, &json!({}));
    }

    fn get(&self, key: &str) -> Option<crate::policy::StreamMode> {
        let entry = read_json(&self.file)?.get(key)?.clone();
        let num = |k: &str| entry.get(k)?.as_u64().and_then(|v| u32::try_from(v).ok());
        let at = entry.get("at")?.as_u64()?;
        if now_ms().saturating_sub(at) >= MODE_TTL_MS {
            return None;
        }
        Some(crate::policy::StreamMode {
            width: num("width")?,
            height: num("height")?,
            fps: num("fps")?,
            scale: num("scale")?,
            encoder: entry.get("encoder")?.as_str()?.to_owned(),
        })
    }

    /// The most recently used mode (any device), still within its lifetime.
    pub fn latest(&self) -> Option<crate::policy::StreamMode> {
        let all = read_json(&self.file)?;
        let newest = all.as_object()?.iter().filter(|(k, _)| k.starts_with(CACHE_VERSION)).max_by_key(|(_, v)| v.get("at").and_then(Value::as_u64).unwrap_or(0))?;
        self.get(newest.0)
    }

    /// Caching is a convenience: a failed write only means probing again next time.
    fn set(&self, key: &str, mode: &crate::policy::StreamMode) {
        let mut all = read_json(&self.file).filter(Value::is_object).unwrap_or_else(|| json!({}));
        all[key] = json!({ "width": mode.width, "height": mode.height, "fps": mode.fps, "scale": mode.scale, "encoder": mode.encoder, "at": now_ms() });
        if let Err(err) = write_json(&self.file, &all) {
            log::warning!("Could not save the stream mode: {err}");
        }
    }
}

pub struct SidecarProvider {
    display: VirtualDisplay,
    settings: StreamSettings,
    modes: ModeCache,
    probe: ProbeFn,
    last_screen: Mutex<Screen>,
}

impl SidecarProvider {
    pub fn new(display: VirtualDisplay, settings: StreamSettings, modes: ModeCache, probe: ProbeFn) -> Self {
        Self { display, settings, modes, probe, last_screen: Mutex::new(DEFAULT_SCREEN) }
    }

    /// Turns the virtual monitor on in this shape; returns its GDI name and refresh rate.
    fn show(&self, size: Size, fps: u32, scale: u32, cancel: &AtomicBool) -> Result<(String, u32), String> {
        // Never turn the monitor on after shutdown began.
        if cancel.load(Ordering::Relaxed) {
            return Err("Xiro is shutting down".into());
        }
        let hz = if fps > HZ_BASE { HZ_HIGH } else { HZ_BASE };
        let attached = self.display.attach(Mode { width: size.width, height: size.height, hz }, scale).map_err(|e| e.to_string())?;
        if attached.changed {
            std::thread::sleep(SETTLE);
        }
        Ok((attached.gdi_name, attached.hz))
    }
}

impl SidecarProvider {
    /// The stream the next connection most likely wants, for pre-warming the encoder on `gpu_display`.
    pub fn likely_stream(&self, gpu_display: &str) -> Option<StreamSpec> {
        let mode = self.modes.latest()?;
        let size = Size { width: mode.width, height: mode.height };
        let spec = self.settings.spec(gpu_display, size, mode.fps);
        Some(StreamSpec { encoder: EncoderPreference::Named(mode.encoder), ..spec })
    }
}

impl Provider for SidecarProvider {
    fn is_virtual(&self) -> bool {
        true
    }

    fn acquire(&self, screen: Option<Screen>, progress: &dyn Fn(&str), cancel: &AtomicBool) -> Result<Stream, String> {
        let panel = {
            let mut last = self.last_screen.lock().map_err(|_| "internal state poisoned".to_owned())?;
            *last = screen.unwrap_or(*last);
            *last
        };
        let panel_size = Size { width: panel.width, height: panel.height };
        // Remembered first, so the setup that adds a new model's sizes knows which.
        crate::screens::record(panel_size);
        self.display.prepare(panel_size).map_err(|e| e.to_string())?;
        let wanted = self.settings.fps_cap.map_or(panel.hz, |cap| cap.min(panel.hz));
        let key = format!("{CACHE_VERSION}|{}x{}@{wanted}|{}", panel.width, panel.height, self.settings.cache_suffix());
        let mode = match self.modes.get(&key) {
            Some(mode) => mode,
            None => {
                progress("Testing the video encoder for this iPad (first time only, a few seconds)…");
                choose_stream_mode(panel_size, panel.scale, wanted, MIN_FPS, |size, fps| {
                    let scale = if size == panel_size { panel.scale } else { panel.scale / 2 };
                    let (name, hz) = self.show(size, fps, scale, cancel)?;
                    let fps = fps.min(hz);
                    log::info!("Testing encoders at {}x{} {fps} fps...", size.width, size.height);
                    (self.probe)(&self.settings.spec(&name, size, fps))
                })?
            }
        };
        let size = Size { width: mode.width, height: mode.height };
        let (name, hz) = self.show(size, mode.fps, mode.scale, cancel)?;
        self.modes.set(&key, &mode);
        let fps = mode.fps.min(hz);
        let spec = self.settings.spec(&name, size, fps);
        Ok(Stream { name, width: mode.width, height: mode.height, fps, bitrate: spec.bitrate, encoder: EncoderPreference::Named(mode.encoder) })
    }

    fn release(&self) {
        if let Err(err) = self.display.detach() {
            log::warning!("Could not turn the virtual display off: {err}");
        }
    }
}

/// Streams an existing monitor as it is (no reshaping, rotation ignored).
pub struct MirrorProvider {
    monitor: (String, u32, u32),
    fps: u32,
    settings: StreamSettings,
    probe: ProbeFn,
    chosen: Mutex<Option<(String, u32)>>,
}

impl MirrorProvider {
    pub fn new(name: String, width: u32, height: u32, fps: u32, settings: StreamSettings, probe: ProbeFn) -> Self {
        Self { monitor: (name, width, height), fps, settings, probe, chosen: Mutex::new(None) }
    }
}

impl Provider for MirrorProvider {
    fn is_virtual(&self) -> bool {
        false
    }

    fn acquire(&self, _screen: Option<Screen>, progress: &dyn Fn(&str), _cancel: &AtomicBool) -> Result<Stream, String> {
        let (name, width, height) = self.monitor.clone();
        // NV12 needs even sizes.
        let size = Size { width: width & !1, height: height & !1 };
        let mut chosen = self.chosen.lock().map_err(|_| "internal state poisoned".to_owned())?;
        if chosen.is_none() {
            progress("Testing the video encoder (first time only, a few seconds)…");
            let outcome = (self.probe)(&self.settings.spec(&name, size, self.fps))?;
            *chosen = Some((outcome.encoder, sustainable_fps(self.fps, outcome.achieved_fps, MIN_FPS)));
        }
        let (encoder, fps) = chosen.clone().unwrap_or_default();
        let bitrate = self.settings.bitrate_for(size.width, size.height, fps);
        Ok(Stream { name, width: size.width, height: size.height, fps, bitrate, encoder: EncoderPreference::Named(encoder) })
    }

    fn release(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrate_follows_resolution_within_bounds() {
        assert_eq!(default_bitrate(2360, 1640, 60), 23_200_000);
        assert_eq!(default_bitrate(320, 240, 15), MIN_BITRATE);
        assert_eq!(default_bitrate(2752, 2064, 240), MAX_BITRATE);
        let fixed = StreamSettings { fps_cap: None, bitrate_mbps: Some(12.5), encoder: EncoderPreference::Auto, gop_seconds: 1.0 };
        assert_eq!(fixed.bitrate_for(1, 1, 1), 12_500_000);
    }

    #[test]
    fn mode_cache_round_trips_and_expires() {
        let dir = std::env::temp_dir().join(format!("xiro-modes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = ModeCache::new(dir.join("mode-cache.json"));
        cache.clear();
        let mode = crate::policy::StreamMode { width: 1180, height: 820, fps: 60, scale: 100, encoder: "Intel".into() };
        assert!(cache.get("k").is_none());
        cache.set("k", &mode);
        assert_eq!(cache.get("k"), Some(mode.clone()));
        assert!(cache.latest().is_none(), "only current-version keys count");
        let key = format!("{CACHE_VERSION}|x");
        cache.set(&key, &mode);
        assert_eq!(cache.latest(), Some(mode));
        let mut all = read_json(&dir.join("mode-cache.json")).unwrap();
        all["k"]["at"] = json!(0);
        write_json(&dir.join("mode-cache.json"), &all).unwrap();
        assert!(cache.get("k").is_none(), "old entries are re-probed");
        all["k"] = json!({ "width": "x" });
        write_json(&dir.join("mode-cache.json"), &all).unwrap();
        assert!(cache.get("k").is_none(), "malformed entries are ignored");
    }

    #[test]
    fn mirror_probes_once_and_caps_fps_to_what_the_encoder_does() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = calls.clone();
        let probe: ProbeFn = Box::new(move |spec| {
            counter.fetch_add(1, Ordering::SeqCst);
            assert_eq!((spec.width, spec.height), (1366, 768));
            Ok(ProbeOutcome { encoder: "sw".into(), achieved_fps: 44.0 })
        });
        let settings = StreamSettings { fps_cap: None, bitrate_mbps: None, encoder: EncoderPreference::Auto, gop_seconds: 1.0 };
        let mirror = MirrorProvider::new(r"\\.\DISPLAY1".into(), 1366, 769, 60, settings, probe);
        let cancel = AtomicBool::new(false);
        let a = mirror.acquire(None, &|_| {}, &cancel).unwrap();
        let b = mirror.acquire(None, &|_| {}, &cancel).unwrap();
        assert_eq!(a, b);
        assert_eq!((a.fps, a.height, calls.load(Ordering::SeqCst)), (40, 768, 1));
        assert!(!mirror.is_virtual());
    }
}
