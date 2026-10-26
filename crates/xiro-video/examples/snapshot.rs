//! Captures and encodes a few frames of a display and writes them as an Annex B
//! .h264 file, to check colours and the pointer by eye (decode with any player).
use std::io::Write;
use std::time::{Duration, Instant};
use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFStartup};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use xiro_video::session::{Output, Session};
use xiro_video::{EncoderPreference, StreamSpec};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let out = std::path::PathBuf::from(&a[0]);
    let display = a.get(1).cloned().unwrap_or_else(|| r"\.\DISPLAY1".into());
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap(); MFStartup(MF_VERSION, MFSTARTUP_FULL).unwrap(); }
    let spec = StreamSpec { display, width: 1366, height: 768, fps: 30, bitrate: 20_000_000, gop: 30, encoder: EncoderPreference::Auto };
    let mut session = Session::open(spec, &mut None).unwrap();
    let mut file = std::fs::File::create(&out).unwrap();
    let (mut frames, start) = (0, Instant::now());
    while frames < 15 && start.elapsed() < Duration::from_secs(5) {
        for o in session.step(frames == 0, Duration::from_millis(50)).unwrap() {
            match o {
                Output::Config(c) => {
                    let sps_len = u16::from_be_bytes([c.avcc[6], c.avcc[7]]) as usize;
                    let sps = &c.avcc[8..8 + sps_len];
                    let p = 8 + sps_len + 1;
                    let pps_len = u16::from_be_bytes([c.avcc[p], c.avcc[p + 1]]) as usize;
                    let pps = &c.avcc[p + 2..p + 2 + pps_len];
                    for nal in [sps, pps] {
                        file.write_all(&[0, 0, 0, 1]).unwrap();
                        file.write_all(nal).unwrap();
                    }
                    println!("config {}", c.codec);
                }
                Output::Frame(f) => {
                    let mut i = 0;
                    while i + 4 <= f.data.len() {
                        let len = u32::from_be_bytes(f.data[i..i + 4].try_into().unwrap()) as usize;
                        file.write_all(&[0, 0, 0, 1]).unwrap();
                        file.write_all(&f.data[i + 4..i + 4 + len]).unwrap();
                        i += 4 + len;
                    }
                    frames += 1;
                }
            }
        }
    }
    println!("wrote {frames} frames with {}", session.encoder_name());
}
