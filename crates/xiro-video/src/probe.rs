//! Measures what each encoder sustains for a monitor, size and frame rate, so the app
//! picks a stream mode that keeps up instead of letting frames queue.

use std::time::{Duration, Instant};

use windows::core::Result;

use windows::Win32::Graphics::Direct3D11::{D3D11_TEXTURE2D_DESC, ID3D11Texture2D};

use crate::capture::Duplication;
use crate::convert::Converter;
use crate::encoder::{EncoderSettings, H264Encoder, candidates};
use crate::gpu;
use crate::h264;
use crate::session::{EncoderPreference, StreamSpec, VideoConfig, Warm, fail, reuse, sample_time};

/// An encoder passes when it delivers this share of the wanted frame rate.
pub const PASS_RATIO: f64 = 0.85;
/// Every few frames is forced to be a key frame, the expensive kind, so the result is not flattered.
const KEY_EVERY: i64 = 10;
/// A monitor that was just turned on can take a moment to show its first image.
const FIRST_FRAME_WAIT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq)]
pub struct ProbeResult {
    pub encoder: String,
    pub hardware: bool,
    pub achieved_fps: f64,
    /// It produced a stream header the iPad can decode.
    pub valid: bool,
}

impl ProbeResult {
    pub fn sustains(&self, fps: u32) -> bool {
        self.valid && self.achieved_fps >= fps as f64 * PASS_RATIO
    }
}

/// Tries the encoders the spec allows, best first, and stops at the first that keeps up.
/// Returns every result measured (the last is the winner when one sustained the rate).
pub fn probe(spec: &StreamSpec, duration: Duration) -> Result<Vec<ProbeResult>> {
    probe_warm(spec, duration, &mut None)
}

/// Like [`probe`], reusing a warm device and leaving the winning encoder warm for the stream.
pub fn probe_warm(spec: &StreamSpec, duration: Duration, warm: &mut Option<Warm>) -> Result<Vec<ProbeResult>> {
    let (adapter, output) = gpu::find_output(&spec.display)?.ok_or_else(|| fail(&format!("{} cannot be captured", spec.display)))?;
    let luid = unsafe { adapter.GetDesc1()?.AdapterLuid };
    let (gpu, _) = reuse(warm, &adapter, luid)?;
    let mut duplication = Duplication::new(&gpu, &output)?;
    let started = Instant::now();
    let mut sample = None;
    while sample.is_none() && started.elapsed() < FIRST_FRAME_WAIT {
        // A desktop that never changes still produces one first frame.
        duplication.grab(100, |desktop, _| {
            sample = Some(copy_of(&gpu, desktop)?);
            Ok(())
        })?;
    }
    let Some(texture) = sample else {
        return Err(fail("the display shows no image to test with"));
    };
    let source = (duplication.width, duplication.height);
    drop(duplication);
    let mut converter = Converter::new(&gpu, source, (spec.width, spec.height), spec.fps)?;
    let settings = EncoderSettings { width: converter.width, height: converter.height, fps: spec.fps, bitrate: spec.bitrate, gop: spec.gop };

    let _realtime = crate::realtime::Realtime::enter();
    let mut results = Vec::new();
    let mut winner = None;
    for candidate in candidates(Some(luid))?.into_iter().filter(|c| spec.encoder.accepts(c)) {
        let (name, hardware) = (candidate.name.clone(), candidate.hardware);
        let Ok(mut encoder) = H264Encoder::open(candidate, &gpu, settings) else { continue };
        let (result, config) = measure(&mut encoder, &mut converter, &texture, spec.fps, duration);
        let result = ProbeResult { encoder: name, hardware, ..result };
        // Software is only worth testing when no hardware encoder produced a usable stream.
        let done = result.sustains(spec.fps) || (result.valid && result.hardware && spec.encoder == EncoderPreference::Auto);
        results.push(result);
        if done {
            winner = config.map(|c| (encoder, c));
            break;
        }
    }
    let (encoder, config) = winner.unzip();
    *warm = Some(Warm { gpu, luid, encoder, config });
    Ok(results)
}

fn measure(
    encoder: &mut H264Encoder,
    converter: &mut Converter,
    desktop: &ID3D11Texture2D,
    fps: u32,
    duration: Duration,
) -> (ProbeResult, Option<VideoConfig>) {
    let interval = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
    let (mut frames, mut config, mut n) = (0u32, None, 0i64);
    let started = Instant::now();
    let mut next = started;
    while started.elapsed() < duration {
        // Every frame is converted and encoded, as when the desktop changes all the time.
        let encoded = converter.convert(desktop, None).and_then(|nv12| encoder.encode(&nv12, sample_time(), n % KEY_EVERY == 0));
        let Ok(samples) = encoded else { break };
        for sample in samples {
            let unit = h264::to_access_unit(&sample.data);
            if let (Some(sps), Some(pps)) = (&unit.sps, &unit.pps) {
                config = h264::avcc_record(sps, pps).and_then(|avcc| h264::codec_string(&avcc).map(|codec| VideoConfig { codec, avcc })).or(config);
            }
            frames += u32::from(config.is_some() && !unit.avcc.is_empty());
        }
        n += 1;
        next += interval;
        match next.checked_duration_since(Instant::now()) {
            Some(wait) => std::thread::sleep(wait),
            None => next = Instant::now(),
        }
    }
    let seconds = started.elapsed().as_secs_f64().max(0.001);
    let valid = config.is_some();
    (ProbeResult { encoder: String::new(), hardware: false, achieved_fps: frames as f64 / seconds, valid }, config)
}

/// A private copy of a desktop image (the duplication frame must be handed back).
fn copy_of(gpu: &gpu::Gpu, desktop: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { desktop.GetDesc(&mut desc) };
    let desc = D3D11_TEXTURE2D_DESC { MiscFlags: 0, CPUAccessFlags: 0, ..desc };
    let mut copy = None;
    unsafe {
        gpu.device.CreateTexture2D(&desc, None, Some(&mut copy))?;
        let copy = copy.ok_or_else(|| fail("no texture"))?;
        gpu.context.CopyResource(&copy, desktop);
        Ok(copy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sustaining_needs_a_valid_stream_and_enough_frames() {
        let r = ProbeResult { encoder: "x".into(), hardware: true, achieved_fps: 52.0, valid: true };
        assert!(r.sustains(60));
        assert!(!r.sustains(120));
        assert!(!ProbeResult { valid: false, ..r }.sustains(30));
    }
}
