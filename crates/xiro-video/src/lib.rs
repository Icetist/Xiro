//! Capture, colour conversion and H.264 encoding, all on the GPU: Desktop
//! Duplication -> D3D11 video processor (BGRA to NV12) -> Media Foundation encoder.

pub mod capture;
pub mod clock;
pub mod convert;
pub mod cursor;
pub mod encoder;
pub mod gpu;
pub mod h264;
pub mod pipeline;
pub mod probe;
pub mod realtime;
pub mod session;

pub use pipeline::{Event, Pipeline, Prober};
pub use probe::{ProbeResult, probe, probe_warm};
pub use session::{EncoderPreference, StreamSpec, VideoConfig, VideoFrame};
