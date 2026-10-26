//! Times each step of opening a stream and probing, on a given display.
use std::time::{Duration, Instant};
use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFStartup};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use xiro_video::capture::Duplication;
use xiro_video::encoder::{EncoderSettings, H264Encoder, candidates};
use xiro_video::{EncoderPreference, StreamSpec, gpu, probe};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let display = a.first().cloned().unwrap_or_else(|| r"\.\DISPLAY1".into());
    let (w, h): (u32, u32) = (a.get(1).map_or(2360, |v| v.parse().unwrap()), a.get(2).map_or(1640, |v| v.parse().unwrap()));
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap(); MFStartup(MF_VERSION, MFSTARTUP_FULL).unwrap(); }
    let t = Instant::now();
    let (adapter, output) = gpu::find_output(&display).unwrap().unwrap();
    println!("find_output {:?}", t.elapsed());
    let t = Instant::now();
    let g = gpu::create_device(&adapter).unwrap();
    println!("create_device {:?}", t.elapsed());
    let t = Instant::now();
    drop(Duplication::new(&g, &output).unwrap());
    println!("duplicate {:?}", t.elapsed());
    let luid = unsafe { adapter.GetDesc1().unwrap().AdapterLuid };
    let t = Instant::now();
    let c = candidates(Some(luid)).unwrap();
    println!("candidates {:?}", t.elapsed());
    let name = c[0].name.clone();
    for candidate in c.into_iter().take(1) {
        let t = Instant::now();
        let e = H264Encoder::open(candidate, &g, EncoderSettings { width: w, height: h, fps: 60, bitrate: 20_000_000, gop: 60 }).unwrap();
        println!("open {name} {:?}", t.elapsed());
        let t = Instant::now();
        drop(e);
        println!("dropped {:?}", t.elapsed());
    }
    let spec = StreamSpec { display, width: w, height: h, fps: 60, bitrate: 20_000_000, gop: 60, encoder: EncoderPreference::Auto };
    for secs in [1200u64, 1200] {
        let t = Instant::now();
        let r = probe(&spec, Duration::from_millis(secs)).unwrap();
        println!("probe {:?}: {:?}", t.elapsed(), r);
    }
}
