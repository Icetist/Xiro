//! One capture session: a monitor, its duplication, the converter and an open
//! encoder. Lives entirely on the video thread (COM objects are not shared).

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Dxgi::{IDXGIAdapter1, IDXGIOutput1};

use windows::core::{Error, Result};
use windows::Win32::Foundation::E_FAIL;

use crate::capture::Duplication;
use crate::clock;
use crate::convert::Converter;
use crate::encoder::{Candidate, EncoderSettings, H264Encoder, candidates};
use crate::gpu::{self, Gpu};
use crate::h264;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncoderPreference {
    /// Best available: hardware first.
    Auto,
    Hardware,
    Software,
    /// An encoder whose name contains this text (as reported by a probe).
    Named(String),
}

impl EncoderPreference {
    pub fn accepts(&self, candidate: &Candidate) -> bool {
        self.accepts_encoder(&candidate.name, candidate.hardware)
    }

    pub fn accepts_encoder(&self, encoder: &str, hardware: bool) -> bool {
        match self {
            Self::Auto => true,
            Self::Hardware => hardware,
            Self::Software => !hardware,
            Self::Named(name) => encoder.to_lowercase().contains(&name.to_lowercase()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamSpec {
    /// GDI name of the monitor, e.g. `\\.\DISPLAY7`.
    pub display: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    pub gop: u32,
    pub encoder: EncoderPreference,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoConfig {
    /// WebCodecs codec string, e.g. "avc1.640028".
    pub codec: String,
    /// avcC decoder configuration record.
    pub avcc: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct VideoFrame {
    pub is_key: bool,
    /// Length-prefixed NAL units.
    pub data: Vec<u8>,
    /// Host clock (see [`clock::now_ms`]) when the desktop image was captured.
    pub captured_at: f64,
}

/// Output of one encoded picture.
pub enum Output {
    Config(VideoConfig),
    Frame(VideoFrame),
}

pub struct Session {
    gpu: Gpu,
    luid: LUID,
    output: IDXGIOutput1,
    duplication: Duplication,
    converter: Converter,
    encoder: H264Encoder,
    pub spec: StreamSpec,
    interval: Duration,
    next_due: Instant,
    dirty: bool,
    /// A key frame was asked for and not yet encoded.
    want_key: bool,
    /// The newest desktop image, already converted for the encoder (also re-sent when a key frame is asked for).
    latest: Option<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D>,
    captured_at: f64,
    /// Capture time per encoder timestamp, for encoders that return frames late.
    pending: Vec<(i64, f64)>,
    config: Option<VideoConfig>,
    /// A header to send before the first frame (when the encoder was reused).
    announce: Option<VideoConfig>,
}

/// Capture times kept for encoders that answer late; beyond this they are not coming.
const MAX_PENDING: usize = 64;

pub fn fail(message: &str) -> Error {
    Error::new(E_FAIL, message)
}

/// Opens the first encoder the preference allows that accepts these settings.
pub fn open_encoder(gpu: &Gpu, adapter_luid: windows::Win32::Foundation::LUID, preference: &EncoderPreference, settings: EncoderSettings) -> Result<H264Encoder> {
    let mut last = fail("no H.264 encoder matches the configured preference");
    for candidate in candidates(Some(adapter_luid))?.into_iter().filter(|c| preference.accepts(c)) {
        match H264Encoder::open(candidate, gpu, settings) {
            Ok(encoder) => return Ok(encoder),
            Err(err) => last = err,
        }
    }
    Err(last)
}

/// Device and encoder kept between streams: opening a hardware encoder takes about a
/// second, so they are reused when the adapter and settings match.
pub struct Warm {
    pub gpu: Gpu,
    pub luid: LUID,
    pub encoder: Option<H264Encoder>,
    /// The warm encoder's stream header: a reused encoder may not repeat it.
    pub config: Option<VideoConfig>,
}

/// The device for this adapter (the warm one when it matches), and the warm encoder if it fits.
pub fn reuse(warm: &mut Option<Warm>, adapter: &IDXGIAdapter1, luid: LUID) -> Result<(Gpu, Option<(H264Encoder, VideoConfig)>)> {
    match warm.take() {
        Some(w) if w.luid == luid => Ok((w.gpu, w.encoder.zip(w.config))),
        _ => Ok((gpu::create_device(adapter)?, None)),
    }
}

/// Sample times share one origin, so a reused encoder never sees time go backwards.
pub(crate) fn sample_time() -> i64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    (ORIGIN.get_or_init(Instant::now).elapsed().as_nanos() / 100) as i64
}

impl Session {
    pub fn open(spec: StreamSpec, warm: &mut Option<Warm>) -> Result<Self> {
        let (adapter, output) = gpu::find_output(&spec.display)?.ok_or_else(|| fail(&format!("{} cannot be captured", spec.display)))?;
        let luid = unsafe { adapter.GetDesc1()?.AdapterLuid };
        let (gpu, warm_encoder) = reuse(warm, &adapter, luid)?;
        let duplication = Duplication::new(&gpu, &output)?;
        let converter = Converter::new(&gpu, (duplication.width, duplication.height), (spec.width, spec.height), spec.fps)?;
        let settings = EncoderSettings { width: converter.width, height: converter.height, fps: spec.fps, bitrate: spec.bitrate, gop: spec.gop };
        let fits = |(e, _): &(H264Encoder, VideoConfig)| e.settings == settings && spec.encoder.accepts_encoder(&e.name, e.hardware);
        let (encoder, announce) = match warm_encoder.filter(fits) {
            Some((encoder, config)) => (encoder, Some(config)),
            None => (open_encoder(&gpu, luid, &spec.encoder, settings)?, None),
        };
        let interval = Duration::from_secs_f64(1.0 / spec.fps.max(1) as f64);
        let now = Instant::now();
        Ok(Self {
            gpu,
            luid,
            output,
            duplication,
            converter,
            encoder,
            spec,
            interval,
            next_due: now,
            // Encode the first image as soon as there is one.
            dirty: true,
            want_key: false,
            latest: None,
            captured_at: clock::now_ms(),
            pending: Vec::new(),
            config: announce.clone(),
            announce,
        })
    }

    /// Ends the session but keeps the device and encoder for the next one.
    pub fn into_warm(self) -> Warm {
        let luid = self.luid;
        Warm { gpu: self.gpu, luid, encoder: Some(self.encoder), config: self.config }
    }

    pub fn encoder_name(&self) -> &str {
        &self.encoder.name
    }

    pub fn is_hardware(&self) -> bool {
        self.encoder.hardware
    }

    pub fn size(&self) -> (u32, u32) {
        (self.converter.width, self.converter.height)
    }

    pub fn set_bitrate(&mut self, bitrate: u32) -> bool {
        self.spec.bitrate = bitrate;
        self.encoder.set_bitrate(bitrate)
    }

    /// Recreates the duplication after Windows invalidated it (mode change, UAC prompt).
    pub fn reconnect(&mut self) -> Result<()> {
        let duplication = Duplication::new(&self.gpu, &self.output)?;
        if (duplication.width, duplication.height) != (self.duplication.width, self.duplication.height) {
            self.converter = Converter::new(&self.gpu, (duplication.width, duplication.height), (self.spec.width, self.spec.height), self.spec.fps)?;
        }
        self.duplication = duplication;
        self.latest = None;
        self.dirty = true;
        Ok(())
    }

    /// Waits for the next frame (at most `idle` when nothing changes) and encodes it when due.
    pub fn step(&mut self, force_key: bool, idle: Duration) -> Result<Vec<Output>> {
        // Remembered until a frame is actually encoded (there may be no image yet).
        self.want_key |= force_key;
        let now = Instant::now();
        let work = (self.dirty || self.want_key) && self.latest.is_some();
        // With no image yet, wait for one (never spin on a zero timeout).
        let wait = if work { self.next_due.saturating_duration_since(now) } else { idle };
        let (converter, latest) = (&mut self.converter, &mut self.latest);
        let fresh = self.duplication.grab(wait.as_millis() as u32, |desktop, pointer| {
            *latest = Some(converter.convert(desktop, Some(&pointer))?);
            Ok(())
        })?;
        if fresh {
            self.dirty = true;
            self.captured_at = clock::now_ms();
        }
        let due = Instant::now() >= self.next_due;
        if !(due && (self.dirty || self.want_key)) {
            return Ok(Vec::new());
        }
        let Some(nv12) = self.latest.clone() else {
            return Ok(Vec::new());
        };
        let time = sample_time();
        self.pending.push((time, self.captured_at));
        if self.pending.len() > MAX_PENDING {
            self.pending.remove(0);
        }
        let samples = self.encoder.encode(&nv12, time, self.want_key)?;
        self.dirty = false;
        self.want_key = false;
        // Pace from now, not from the missed slot: never burst to catch up.
        self.next_due = (self.next_due + self.interval).max(Instant::now());
        Ok(samples.into_iter().flat_map(|s| self.outputs(&s.data, s.time)).collect())
    }

    fn outputs(&mut self, annexb: &[u8], time: i64) -> Vec<Output> {
        let unit = h264::to_access_unit(annexb);
        let captured_at = self.take_capture_time(time);
        let mut out: Vec<Output> = self.announce.take().map(Output::Config).into_iter().collect();
        if let (Some(sps), Some(pps)) = (&unit.sps, &unit.pps) {
            if let Some(config) = h264::avcc_record(sps, pps).and_then(|avcc| h264::codec_string(&avcc).map(|codec| VideoConfig { codec, avcc })) {
                if self.config.as_ref() != Some(&config) {
                    self.config = Some(config.clone());
                    out.push(Output::Config(config));
                }
            }
        }
        // Frames before the first parameter sets can't be decoded by anyone.
        if self.config.is_some() && !unit.avcc.is_empty() {
            out.push(Output::Frame(VideoFrame { is_key: unit.is_key, data: unit.avcc, captured_at }));
        }
        out
    }

    fn take_capture_time(&mut self, time: i64) -> f64 {
        match self.pending.iter().position(|(t, _)| *t == time) {
            Some(i) => {
                let (_, at) = self.pending[i];
                // Anything older was dropped by the encoder.
                self.pending.drain(..=i);
                at
            }
            None => self.captured_at,
        }
    }
}

/// Opens an encoder on `spec.display`'s GPU and runs a black frame through it, so the
/// first stream skips the encoder's start-up cost (about a second on hardware).
pub fn prewarm(spec: &StreamSpec) -> Result<Warm> {
    let (adapter, _) = gpu::find_output(&spec.display)?.ok_or_else(|| fail(&format!("{} cannot be captured", spec.display)))?;
    let luid = unsafe { adapter.GetDesc1()?.AdapterLuid };
    let gpu = gpu::create_device(&adapter)?;
    let (width, height) = (crate::convert::even(spec.width), crate::convert::even(spec.height));
    let settings = EncoderSettings { width, height, fps: spec.fps, bitrate: spec.bitrate, gop: spec.gop };
    let mut encoder = open_encoder(&gpu, luid, &spec.encoder, settings)?;
    let black = black_nv12(&gpu, width, height)?;
    let mut config = None;
    for _ in 0..PREWARM_FRAMES {
        for sample in encoder.encode(&black, sample_time(), true)? {
            let unit = h264::to_access_unit(&sample.data);
            if let (Some(sps), Some(pps)) = (&unit.sps, &unit.pps) {
                config = h264::avcc_record(sps, pps).and_then(|avcc| h264::codec_string(&avcc).map(|codec| VideoConfig { codec, avcc })).or(config);
            }
        }
        if config.is_some() {
            break;
        }
    }
    let encoder = config.is_some().then_some(encoder);
    Ok(Warm { gpu, luid, encoder, config })
}

/// Attempts at getting a stream header out of a freshly opened encoder.
const PREWARM_FRAMES: usize = 3;
/// Studio-range black: Y 16, Cb/Cr 128.
const BLACK_Y: u8 = 16;
const BLACK_CHROMA: u8 = 128;

fn black_nv12(gpu: &Gpu, width: u32, height: u32) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D> {
    use windows::Win32::Graphics::Direct3D11::{D3D11_BIND_RENDER_TARGET, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT};
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
    let luma = (width * height) as usize;
    let mut pixels = vec![BLACK_Y; luma];
    pixels.resize(luma + luma / 2, BLACK_CHROMA);
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let data = D3D11_SUBRESOURCE_DATA { pSysMem: pixels.as_ptr().cast(), SysMemPitch: width, SysMemSlicePitch: 0 };
    let mut texture = None;
    unsafe { gpu.device.CreateTexture2D(&desc, Some(&data), Some(&mut texture))? };
    texture.ok_or_else(|| fail("no texture"))
}
