//! Installs and removes what Xiro needs from Windows: display driver, its settings, firewall rule.
//! Xiro is one portable exe; a single elevated step installs or repairs everything.

use std::path::Path;

use xiro_display::{MAX_SIZES, Size, fits_driver, install as driver, secure, stream_sizes, with_both_orientations};
use xiro_shell::install;

use crate::autostart;
use crate::protocol::PANELS;
use crate::screens;

/// The signed upstream driver package (github.com/VirtualDrivers/Virtual-Display-Driver,
/// MIT), fetched by scripts/fetch-driver.ps1 with a pinned checksum.
const DRIVER_FILES: [(&str, &[u8]); 3] = [
    ("MttVDD.inf", include_bytes!("../vendor/vdd/MttVDD.inf")),
    ("MttVDD.dll", include_bytes!("../vendor/vdd/MttVDD.dll")),
    ("mttvdd.cat", include_bytes!("../vendor/vdd/mttvdd.cat")),
];

/// Driver sizes: native and half size in both orientations per panel, connected devices first
/// (newest first), then every known panel (iPads first), as many as the driver takes.
pub fn wanted_sizes(seen: &[Size]) -> Vec<Size> {
    let known = PANELS.iter().map(|&(width, height, _)| Size { width, height });
    let mut sizes: Vec<Size> = Vec::new();
    for panel in seen.iter().copied().chain(known) {
        let mut new: Vec<Size> = Vec::new();
        for size in with_both_orientations(&stream_sizes(panel)) {
            if fits_driver(size) && !sizes.contains(&size) && !new.contains(&size) {
                new.push(size);
            }
        }
        if sizes.len() + new.len() <= MAX_SIZES {
            sizes.extend(new);
        }
    }
    sizes
}

/// Why the second screen can't work right now, if it can't.
pub fn display_problem() -> Option<&'static str> {
    match driver::driver_installed() {
        Ok(false) => Some("the display driver is not installed"),
        Err(_) => Some("Windows could not list its display drivers"),
        Ok(true) if secure::settings_need_update(&wanted_sizes(&screens::load())) => Some("the display driver's settings need an update"),
        Ok(true) => None,
    }
}

/// What the elevated step would do for this exe at its current location.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupNeed {
    /// No display driver yet.
    Install,
    /// The driver is there but broken (stuck, or old settings).
    Repair,
    /// Only the firewall rule is missing or names another copy of Xiro (it was moved).
    Firewall,
}

pub fn setup_need(exe: &Path, ports: &[u16]) -> Option<SetupNeed> {
    match (display_problem(), driver::driver_installed()) {
        (Some(_), Ok(false)) => Some(SetupNeed::Install),
        (Some(_), _) => Some(SetupNeed::Repair),
        (None, _) if install::firewall_rule_matches(exe, ports) == Some(false) => Some(SetupNeed::Firewall),
        (None, _) => None,
    }
}

/// The elevated steps refuse to run unelevated, so nothing happens half-way.
fn require_admin() -> Result<(), String> {
    if install::is_elevated() { Ok(()) } else { Err("This step must run as administrator.".into()) }
}

/// Elevated (`--setup-system`): installs or restarts the driver, writes its settings and the firewall rule.
/// `seen` are the screens that connected (`--screens`).
pub fn setup_system(exe: &Path, ports: &[u16], seen: &[Size]) -> Result<(), String> {
    require_admin()?;
    // Sizes up front: a connection never waits for the driver, and the app never needs to
    // write the admin-only driver settings.
    let changed = secure::register_all(&wanted_sizes(seen)).map_err(|e| e.to_string())?;
    if driver::driver_installed().map_err(|e| e.to_string())? {
        // Changed settings already restarted the driver; otherwise restart only if it lost its monitor.
        let present = xiro_display::VirtualDisplay::new(std::env::temp_dir().join("xiro-layout-probe.json")).available();
        if !changed && !present.unwrap_or(false) {
            driver::restart_driver().map_err(|e| e.to_string())?;
        }
    } else {
        secure::install_embedded_driver(&DRIVER_FILES, "MttVDD.inf").map_err(|e| e.to_string())?;
    }
    install::add_firewall_rule(exe, ports)
}

/// Runs the elevated step (Windows asks for permission). Err with a sentence for the user.
pub fn run_system_setup(ports: &[u16]) -> Result<(), String> {
    let ports: Vec<String> = ports.iter().map(u16::to_string).collect();
    let seen = screens::format(&screens::load());
    match install::run_elevated(&["--setup-system", "--ports", &ports.join(","), "--screens", &seen]) {
        Ok(0) => Ok(()),
        Ok(code) => Err(format!("Setting up the display driver failed (code {code}). The log in %LOCALAPPDATA%\\Xiro has the details.")),
        Err(_) => Err("Windows didn't give permission, so nothing was changed.".into()),
    }
}

/// Elevated uninstall step. Every part is attempted; errors are reported together.
pub fn remove_system() -> Result<(), String> {
    require_admin()?;
    let mut errors = Vec::new();
    errors.extend(install::remove_firewall_rule().err());
    errors.extend(driver::remove_driver().map_err(|e| e.to_string()).err());
    errors.extend(secure::remove_settings().map_err(|e| e.to_string()).err());
    if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
}

/// The uninstall part that needs elevation: firewall rule, display driver and its settings.
pub fn uninstall_system() -> Result<(), String> {
    match install::run_elevated(&["--remove-system"]) {
        Ok(0) => Ok(()),
        Ok(code) => Err(format!("Some of Xiro could not be removed (code {code}). The log in %LOCALAPPDATA%\\Xiro says what; your settings were kept.")),
        Err(_) => Err("Windows didn't give permission, so nothing was removed.".into()),
    }
}

/// Uninstalls the rest: autostart, leftovers of older versions, settings, pairing and logs
/// (deleted a few seconds after exit). Xiro.exe itself stays for the person to delete.
pub fn forget_everything() -> Result<(), String> {
    autostart::disable();
    install::remove_leftovers(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn size(width: u32, height: u32) -> Size {
        Size { width, height }
    }

    #[test]
    fn every_ipad_gets_its_sizes_in_both_orientations_within_the_driver_limit() {
        let sizes = wanted_sizes(&[]);
        assert!(sizes.len() <= MAX_SIZES);
        for (w, h, _) in PANELS.iter().filter(|p| p.2 == 200 && p.0 > 2000) {
            for s in with_both_orientations(&stream_sizes(size(*w, *h))) {
                assert!(sizes.contains(&s), "{s:?}");
            }
        }
        assert!(sizes.iter().all(|s| fits_driver(*s)), "nothing the driver would choke on");
    }

    #[test]
    fn devices_that_connected_come_first() {
        let iphone = size(2556, 1179);
        let sizes = wanted_sizes(&[iphone]);
        assert_eq!(&sizes[..2], &[iphone, size(1179, 2556)], "native only: its half is too narrow upright");
        assert!(sizes.len() <= MAX_SIZES);
        assert!(sizes.contains(&size(2360, 1640)), "iPads still fit");
        let ipad = wanted_sizes(&[size(2360, 1640)]);
        assert_eq!(ipad.iter().filter(|s| **s == size(2360, 1640)).count(), 1, "once");
    }

    #[test]
    fn the_driver_package_is_embedded() {
        assert!(DRIVER_FILES.iter().all(|(_, bytes)| !bytes.is_empty()));
        // The INF is UTF-16 (byte-order mark FF FE); the DLL a Windows binary.
        assert_eq!(&DRIVER_FILES[0].1[..2], &[0xff, 0xfe]);
        assert_eq!(&DRIVER_FILES[1].1[..2], b"MZ");
    }
}
