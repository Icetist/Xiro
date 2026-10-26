//! H.264 via Media Foundation: the GPU's hardware encoder fed from video memory, or the software
//! fallback. Tuned for latency: no B-frames, CBR, low-latency mode, one frame in flight.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{E_FAIL, LUID, VARIANT_FALSE, VARIANT_TRUE};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_BOOL, VT_UI4};
use windows::core::{Error, GUID, Interface, PWSTR, Result};

use super::gpu::Gpu;

/// How long to wait for the encoder to accept a frame before giving up on it.
const INPUT_TIMEOUT: Duration = Duration::from_millis(500);
/// Floor for waiting on a frame's output (hardware encoders answer in 1-5 ms).
const MIN_OUTPUT_WAIT: Duration = Duration::from_millis(40);
const HNS_PER_SECOND: i64 = 10_000_000;
/// Encoder speed/quality trade-off (0 = fastest, 100 = best). Leans fast; text stays readable at our bitrates.
const QUALITY_VS_SPEED: u32 = 33;
/// VBV buffer in frames: small keeps latency steady, larger helps key frames.
const VBV_FRAMES: u32 = 3;
/// How long dropping an encoder waits for its event thread to end.
const PUMP_JOIN_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderSettings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Bits per second.
    pub bitrate: u32,
    /// Frames between key frames.
    pub gop: u32,
}

/// Annex B output for one input frame.
pub struct EncodedSample {
    pub data: Vec<u8>,
    pub time: i64,
}

/// An encoder installed on this PC.
pub struct Candidate {
    pub name: String,
    pub hardware: bool,
    activate: IMFActivate,
}

/// Hardware encoders on the capture GPU first (best first, as Windows sorts them), then software.
pub fn candidates(adapter: Option<LUID>) -> Result<Vec<Candidate>> {
    let mut found = Vec::new();
    let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
    let passes = [
        (MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER, true),
        (MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER, false),
    ];
    for (flags, hardware) in passes {
        unsafe {
            let attributes = match adapter.filter(|_| hardware) {
                Some(luid) => {
                    let mut attrs = None;
                    MFCreateAttributes(&mut attrs, 1)?;
                    let attrs = attrs.expect("MFCreateAttributes returned nothing");
                    let bytes = std::slice::from_raw_parts((&luid as *const LUID).cast::<u8>(), size_of::<LUID>());
                    attrs.SetBlob(&MFT_ENUM_ADAPTER_LUID, bytes)?;
                    Some(attrs)
                }
                None => None,
            };
            let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count = 0u32;
            MFTEnum2(MFT_CATEGORY_VIDEO_ENCODER, flags, None, Some(&output), attributes.as_ref(), &mut list, &mut count)?;
            for i in 0..count as usize {
                if let Some(activate) = (*list.add(i)).take() {
                    let name = friendly_name(&activate).unwrap_or_else(|| "H.264 encoder".into());
                    found.push(Candidate { name, hardware, activate });
                }
            }
            CoTaskMemFree(Some(list as _));
        }
    }
    Ok(found)
}

fn friendly_name(activate: &IMFActivate) -> Option<String> {
    let mut name = PWSTR::null();
    let mut len = 0;
    unsafe {
        activate.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut name, &mut len).ok()?;
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(name.0, len as usize));
        CoTaskMemFree(Some(name.0 as _));
        Some(text)
    }
}

fn variant_u32(value: u32) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_UI4,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { ulVal: value },
            }),
        },
    }
}

fn variant_bool(value: bool) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_BOOL,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { boolVal: if value { VARIANT_TRUE } else { VARIANT_FALSE } },
            }),
        },
    }
}

const fn pack(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

/// The worker thread that waits on an async encoder's events.
struct EventPump {
    events: Receiver<MF_EVENT_TYPE>,
    thread: Option<JoinHandle<()>>,
}

struct SendGenerator(IMFMediaEventGenerator);
// SAFETY: the event generator of an async MFT is free-threaded (MFTs must allow calls from any thread).
unsafe impl Send for SendGenerator {}

impl EventPump {
    fn start(generator: IMFMediaEventGenerator) -> Self {
        let (tx, events) = mpsc::channel();
        let generator = SendGenerator(generator);
        let thread = std::thread::Builder::new()
            .name("xiro-encoder-events".into())
            .spawn(move || {
                let generator = generator;
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                    // Ends with MF_E_SHUTDOWN when the encoder is shut down.
                    while let Ok(event) = generator.0.GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0)) {
                        let Ok(kind) = event.GetType() else { break };
                        if tx.send(MF_EVENT_TYPE(kind as i32)).is_err() {
                            break;
                        }
                    }
                }
            })
            .expect("spawning the encoder event thread");
        Self { events, thread: Some(thread) }
    }
}

pub struct H264Encoder {
    transform: IMFTransform,
    /// Kept to shut the encoder down the supported way (`ShutdownObject`).
    activate: IMFActivate,
    codec: Option<ICodecAPI>,
    pump: Option<EventPump>,
    _manager: Option<IMFDXGIDeviceManager>,
    input_id: u32,
    output_id: u32,
    input_credits: u32,
    provides_samples: bool,
    output_size: u32,
    frame_duration: i64,
    pub name: String,
    pub hardware: bool,
    pub settings: EncoderSettings,
}

impl H264Encoder {
    /// Takes the candidate: an activation object must not be activated twice (some drivers crash).
    pub fn open(candidate: Candidate, gpu: &Gpu, settings: EncoderSettings) -> Result<Self> {
        unsafe {
            let transform: IMFTransform = candidate.activate.ActivateObject()?;
            let attrs = transform.GetAttributes().ok();
            let is_async = attrs.as_ref().and_then(|a| a.GetUINT32(&MF_TRANSFORM_ASYNC).ok()).unwrap_or(0) != 0;
            if let Some(a) = &attrs {
                if is_async {
                    a.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
                }
                let _ = a.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            let d3d_aware = attrs.as_ref().and_then(|a| a.GetUINT32(&MF_SA_D3D11_AWARE).ok()).unwrap_or(0) != 0;
            let manager = if d3d_aware {
                let mut token = 0;
                let mut manager = None;
                MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
                let manager = manager.expect("no DXGI device manager");
                manager.ResetDevice(&gpu.device, token)?;
                transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;
                Some(manager)
            } else {
                None
            };

            let (mut input_id, mut output_id) = ([0u32], [0u32]);
            if transform.GetStreamIDs(&mut input_id, &mut output_id).is_err() {
                (input_id, output_id) = ([0], [0]);
            }
            let codec = transform.cast::<ICodecAPI>().ok();
            let mut encoder = Self {
                transform,
                activate: candidate.activate.clone(),
                codec,
                pump: None,
                _manager: manager,
                input_id: input_id[0],
                output_id: output_id[0],
                input_credits: 0,
                provides_samples: false,
                output_size: 0,
                frame_duration: HNS_PER_SECOND / settings.fps.max(1) as i64,
                name: candidate.name.clone(),
                hardware: candidate.hardware,
                settings,
            };
            // Some encoders only honour low-latency settings made before the media types.
            encoder.configure_codec();
            encoder.set_types()?;
            encoder.configure_codec();
            let info = encoder.transform.GetOutputStreamInfo(encoder.output_id)?;
            encoder.provides_samples = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
            encoder.output_size = info.cbSize.max(settings.width * settings.height);

            encoder.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            encoder.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            if is_async {
                encoder.pump = Some(EventPump::start(encoder.transform.cast()?));
            }
            Ok(encoder)
        }
    }

    fn set_codec(&self, key: &GUID, value: VARIANT) -> bool {
        self.codec.as_ref().is_some_and(|c| unsafe { c.SetValue(key, &value).is_ok() })
    }

    /// Best effort: encoders support different subsets; unsupported knobs are skipped.
    fn configure_codec(&self) {
        let s = self.settings;
        self.set_codec(&CODECAPI_AVLowLatencyMode, variant_bool(true));
        self.set_codec(&CODECAPI_AVEncCommonRateControlMode, variant_u32(eAVEncCommonRateControlMode_CBR.0 as u32));
        self.set_codec(&CODECAPI_AVEncCommonMeanBitRate, variant_u32(s.bitrate));
        self.set_codec(&CODECAPI_AVEncCommonBufferSize, variant_u32(s.bitrate / s.fps.max(1) * VBV_FRAMES));
        self.set_codec(&CODECAPI_AVEncMPVGOPSize, variant_u32(s.gop));
        self.set_codec(&CODECAPI_AVEncMPVDefaultBPictureCount, variant_u32(0));
        self.set_codec(&CODECAPI_AVEncCommonQualityVsSpeed, variant_u32(QUALITY_VS_SPEED));
        self.set_codec(&CODECAPI_AVEncH264CABACEnable, variant_bool(true));
    }

    fn video_type(&self, subtype: &GUID) -> Result<IMFMediaType> {
        let s = self.settings;
        unsafe {
            let t = MFCreateMediaType()?;
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
            t.SetUINT64(&MF_MT_FRAME_SIZE, pack(s.width, s.height))?;
            t.SetUINT64(&MF_MT_FRAME_RATE, pack(s.fps, 1))?;
            t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
            t.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
            t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
            t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
            Ok(t)
        }
    }

    fn set_types(&self) -> Result<()> {
        unsafe {
            let output = self.video_type(&MFVideoFormat_H264)?;
            output.SetUINT32(&MF_MT_AVG_BITRATE, self.settings.bitrate)?;
            output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            if self.transform.SetOutputType(self.output_id, &output, 0).is_err() {
                // Not every encoder offers High profile; Main still has CABAC.
                output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32)?;
                self.transform.SetOutputType(self.output_id, &output, 0)?;
            }
            let input = self.video_type(&MFVideoFormat_NV12)?;
            self.transform.SetInputType(self.input_id, &input, 0)
        }
    }

    /// Changes the bitrate without restarting, where the encoder allows it.
    pub fn set_bitrate(&mut self, bitrate: u32) -> bool {
        // Only a bitrate the encoder accepted counts: a reused encoder is matched on its settings.
        let applied = self.set_codec(&CODECAPI_AVEncCommonMeanBitRate, variant_u32(bitrate));
        if applied {
            self.settings.bitrate = bitrate;
        }
        applied
    }

    fn input_sample(&self, nv12: &ID3D11Texture2D, time: i64) -> Result<IMFSample> {
        unsafe {
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, nv12, 0, false)?;
            if let Ok(two_d) = buffer.cast::<IMF2DBuffer>() {
                buffer.SetCurrentLength(two_d.GetContiguousLength()?)?;
            }
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(self.frame_duration)?;
            Ok(sample)
        }
    }

    /// Encodes one NV12 frame. Usually returns that frame's output; an encoder that
    /// holds frames back returns them on a later call.
    pub fn encode(&mut self, nv12: &ID3D11Texture2D, time: i64, force_key: bool) -> Result<Vec<EncodedSample>> {
        let mut out = Vec::new();
        if force_key {
            self.set_codec(&CODECAPI_AVEncVideoForceKeyFrame, variant_u32(1));
        }
        let sample = self.input_sample(nv12, time).map_err(|e| context(e, "input sample"))?;
        if self.pump.is_none() {
            unsafe { self.transform.ProcessInput(self.input_id, &sample, 0)? };
            while let Some(s) = self.pull()? {
                out.push(s);
            }
            return Ok(out);
        }

        let deadline = Instant::now() + INPUT_TIMEOUT;
        while self.input_credits == 0 {
            if !self.wait_event(deadline, &mut out)? {
                return Err(Error::new(E_FAIL, "the encoder stopped accepting frames"));
            }
        }
        unsafe { self.transform.ProcessInput(self.input_id, &sample, 0) }.map_err(|e| context(e, "ProcessInput"))?;
        self.input_credits -= 1;
        // Latency first: wait for this frame before taking the next one.
        let wait = Duration::from_nanos(self.frame_duration as u64 * 200).max(MIN_OUTPUT_WAIT);
        let deadline = Instant::now() + wait;
        while !out.iter().any(|s| s.time == time) {
            if !self.wait_event(deadline, &mut out)? {
                break;
            }
        }
        Ok(out)
    }

    /// Handles one encoder event; false when the deadline passed first.
    fn wait_event(&mut self, deadline: Instant, out: &mut Vec<EncodedSample>) -> Result<bool> {
        let timeout = deadline.saturating_duration_since(Instant::now());
        let pump = self.pump.as_ref().expect("async encoder without event pump");
        let event = match pump.events.recv_timeout(timeout) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => return Ok(false),
            Err(RecvTimeoutError::Disconnected) => return Err(Error::new(E_FAIL, "encoder stopped")),
        };
        if event == METransformNeedInput {
            self.input_credits += 1;
        } else if event == MEError {
            return Err(Error::new(E_FAIL, "the hardware encoder reported an error"));
        } else if event == METransformHaveOutput {
            if let Some(sample) = self.pull()? {
                out.push(sample);
            }
        }
        Ok(true)
    }

    fn pull(&mut self) -> Result<Option<EncodedSample>> {
        loop {
            let sample = if self.provides_samples {
                None
            } else {
                unsafe {
                    let sample = MFCreateSample()?;
                    sample.AddBuffer(&MFCreateMemoryBuffer(self.output_size)?)?;
                    Some(sample)
                }
            };
            let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: self.output_id,
                pSample: std::mem::ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: std::mem::ManuallyDrop::new(None),
            }];
            let mut status = 0;
            let result = unsafe { self.transform.ProcessOutput(0, &mut buffers, &mut status) }.map_err(|e| context(e, "ProcessOutput"));
            let [mut buffer] = buffers;
            let sample = unsafe { std::mem::ManuallyDrop::take(&mut buffer.pSample) };
            drop(unsafe { std::mem::ManuallyDrop::take(&mut buffer.pEvents) });
            match result {
                Ok(()) => return sample.map(|s| read_sample(&s)).transpose(),
                Err(err) if err.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                Err(err) if err.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.renegotiate_output()?;
                    // An async encoder announces the output again with a new event.
                    if self.pump.is_some() {
                        return Ok(None);
                    }
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn renegotiate_output(&mut self) -> Result<()> {
        unsafe {
            let offered = self.transform.GetOutputAvailableType(self.output_id, 0)?;
            self.transform.SetOutputType(self.output_id, &offered, 0)?;
            let info = self.transform.GetOutputStreamInfo(self.output_id)?;
            self.provides_samples = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
            self.output_size = info.cbSize.max(self.settings.width * self.settings.height);
        }
        Ok(())
    }
}

/// Names the failing step, so a log line says more than "Catastrophic failure".
fn context(err: Error, step: &str) -> Error {
    Error::new(err.code(), format!("{step}: {}", err.message()))
}

fn read_sample(sample: &IMFSample) -> Result<EncodedSample> {
    unsafe {
        let buffer = sample.ConvertToContiguousBuffer()?;
        let mut data = std::ptr::null_mut();
        let mut len = 0;
        buffer.Lock(&mut data, None, Some(&mut len))?;
        let bytes = std::slice::from_raw_parts(data, len as usize).to_vec();
        buffer.Unlock()?;
        Ok(EncodedSample { data: bytes, time: sample.GetSampleTime().unwrap_or(0) })
    }
}

impl Drop for H264Encoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            // Ends the event thread's blocking wait.
            if let Ok(shutdown) = self.transform.cast::<IMFShutdown>() {
                let _ = shutdown.Shutdown();
            }
            let _ = self.activate.ShutdownObject();
        }
        if let Some(mut pump) = self.pump.take() {
            if let Some(thread) = pump.thread.take() {
                // Bounded: a driver that never ends its event wait must not hang the video thread.
                let deadline = Instant::now() + PUMP_JOIN_TIMEOUT;
                while !thread.is_finished() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                if thread.is_finished() {
                    let _ = thread.join();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::pack;

    #[test]
    fn packs_media_foundation_size_pairs() {
        assert_eq!(pack(1920, 1080), (1920u64 << 32) | 1080);
        assert_eq!(pack(60, 1), 0x0000_003c_0000_0001);
    }
}
