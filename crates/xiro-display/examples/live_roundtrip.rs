//! Live attach/detach round trip on a real PC; always detaches, then checks the screens are restored.
//! Uses a throwaway layout file so the user's saved position is untouched.

use std::time::Instant;

use windows::Win32::Devices::Display::{GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_FLAGS, EnumDisplaySettingsExW};
use windows::core::HSTRING;
use xiro_display::{Mode, VirtualDisplay, install};

const IPAD: Mode = Mode { width: 2360, height: 1640, hz: 60 };
const SCALE: u32 = 200;

fn active_paths() -> u32 {
    let (mut paths, mut modes) = (0, 0);
    // SAFETY: both out-pointers are valid.
    let rc = unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut paths, &mut modes) };
    if rc == ERROR_SUCCESS { paths } else { u32::MAX }
}

fn mode_of(name: &str) -> Option<(u32, u32, u32, i32, i32)> {
    let mut dm = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
    // SAFETY: `dm` is sized and outlives the call.
    let ok = unsafe { EnumDisplaySettingsExW(&HSTRING::from(name), ENUM_CURRENT_SETTINGS, &mut dm, ENUM_DISPLAY_SETTINGS_FLAGS(0)) };
    // SAFETY: display devmodes hold the position variant.
    let p = unsafe { dm.Anonymous1.Anonymous2.dmPosition };
    ok.as_bool().then_some((dm.dmPelsWidth, dm.dmPelsHeight, dm.dmDisplayFrequency, p.x, p.y))
}

/// Detaches on drop, so even a panic leaves the monitor off.
struct DetachGuard<'a>(&'a VirtualDisplay);

impl Drop for DetachGuard<'_> {
    fn drop(&mut self) {
        let t = Instant::now();
        println!("detach: {:?} in {:?}", self.0.detach(), t.elapsed());
    }
}

fn main() {
    let before = active_paths();
    let primary_before = mode_of(r"\\.\DISPLAY1");
    println!("active paths before: {before}; DISPLAY1: {primary_before:?}");
    println!("driver_installed: {:?}", install::driver_installed());

    let layout = std::env::temp_dir().join("xiro-display-live-layout.json");
    let vd = VirtualDisplay::new(layout);
    let t = Instant::now();
    println!("available: {:?} in {:?}", vd.available(), t.elapsed());

    {
        let _guard = DetachGuard(&vd);
        let t = Instant::now();
        match vd.attach(IPAD, SCALE) {
            Ok(attached) => {
                println!("attach: {attached:?} in {:?}", t.elapsed());
                println!("mode of {}: {:?}", attached.gdi_name, mode_of(&attached.gdi_name));
                println!("active paths while attached: {}", active_paths());
            }
            Err(err) => println!("attach failed after {:?}: {err}", t.elapsed()),
        }
    }

    let after = active_paths();
    let primary_after = mode_of(r"\\.\DISPLAY1");
    println!("active paths after: {after}; DISPLAY1: {primary_after:?}");
    let restored = after == before && primary_after == primary_before;
    println!("{}", if restored { "OK: original screens restored" } else { "WARNING: topology differs from before" });
}
