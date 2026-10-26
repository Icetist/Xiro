//! Everything the person at the PC sees and does: the tray and Xiro's window, the setup and
//! repair prompts, unlinking every device and uninstalling.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use xiro_display::VirtualDisplay;
use xiro_shell::{Badge, Choice, DisplayHealth, MenuState, PairingInfo, Prompt, Status, TrayAction, WifiInfo, WifiState};

use crate::app::Running;
use crate::identity::{self, Identity};
use crate::xiro_wifi::XiroWifi;
use crate::setup::SetupNeed;
use crate::{autostart, config, log, pairing, screens, setup};

pub const TITLE: &str = "Xiro";
/// Windows usually gives the PC its Xiro Wi-Fi address within a couple of seconds.
const HOSTED_ADDRESS_TRIES: u32 = 20;
const HOSTED_ADDRESS_POLL: Duration = Duration::from_millis(500);
const HEALTH_POLL: Duration = Duration::from_secs(10);
/// A driver reload hides the monitor for ~15 s; only a longer absence counts as broken.
const HEALTH_MISSES: u32 = 3;

fn prompt(badge: Badge, title: &str, body: &str, buttons: Vec<(&str, Choice)>, default: usize) -> Prompt {
    Prompt {
        caption: TITLE,
        badge,
        title: title.to_owned(),
        body: body.to_owned(),
        detail: None,
        buttons: buttons.into_iter().map(|(label, choice)| (label.to_owned(), choice)).collect(),
        default,
        cancel: 0,
        arm: None,
        timeout: None,
        topmost: false,
    }
}

pub fn tell(badge: Badge, title: &str, body: &str) {
    xiro_shell::ask(prompt(badge, title, body, vec![("OK", Choice::Main)], 0));
}

/// "Cancel" or `yes`; true for `yes`. `safe_default` makes Enter mean `yes`.
fn confirm(badge: Badge, title: &str, body: &str, yes: (&str, Choice), safe_default: bool) -> bool {
    xiro_shell::ask(prompt(badge, title, body, vec![("Cancel", Choice::Plain), yes], usize::from(safe_default))) == 1
}

fn setup_words(need: SetupNeed) -> (&'static str, &'static str, &'static str) {
    match need {
        SetupNeed::Install => (
            "Set up the second screen",
            "Xiro adds a virtual display, so your iPad or iPhone becomes a real second screen, and lets devices on your network reach it through Windows Firewall. Windows asks for permission once.",
            "Set up",
        ),
        SetupNeed::Repair => (
            "Fix the second screen",
            "Xiro will set its display driver up for your devices and restart it. Windows asks for permission, then Xiro restarts.",
            "Fix",
        ),
        SetupNeed::Firewall => (
            "Let devices reach this copy of Xiro",
            "Xiro moved since it was set up, so Windows Firewall doesn't know this copy yet. Windows asks for permission once.",
            "Allow",
        ),
    }
}

/// Where "Not now" is remembered, per copy of Xiro.
fn later_file() -> PathBuf {
    config::data_dir().join("setup-later.txt")
}

/// True if the person chose "Not now" for this exe; then only the window's Fix card offers setup.
pub fn setup_postponed(exe: &Path) -> bool {
    std::fs::read_to_string(later_file()).is_ok_and(|saved| saved.trim() == exe.display().to_string())
}

/// Asks whether to set up (or repair) the virtual display, then does it. True when done.
pub fn offer_setup(need: SetupNeed, ports: &[u16]) -> bool {
    let (title, body, yes) = setup_words(need);
    if !confirm(Badge::Shield, title, body, (yes, Choice::Main), true) {
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::fs::write(later_file(), exe.display().to_string());
        }
        return false;
    }
    let _ = std::fs::remove_file(later_file());
    match setup::run_system_setup(ports) {
        Ok(()) => true,
        Err(why) => {
            log::warning!("Display setup: {why}");
            tell(Badge::Shield, "The second screen isn't set up yet", &why);
            false
        }
    }
}

/// Feeds the window's Fix card: static problems show at once, a missing monitor after a while.
/// A new device model needs its sizes set up; Xiro offers that by itself.
pub fn watch_display(layout_file: PathBuf) -> Arc<AtomicBool> {
    let broken = Arc::new(AtomicBool::new(setup::display_problem().is_some()));
    let flag = broken.clone();
    std::thread::spawn(move || {
        let display = VirtualDisplay::new(layout_file);
        let mut misses = 0;
        let mut seen = screens::load();
        loop {
            let problem = setup::display_problem();
            let now_seen = screens::load();
            if now_seen != seen {
                seen = now_seen;
                if problem.is_some() {
                    xiro_shell::request(TrayAction::RepairDisplay);
                }
            }
            let present = problem.is_none() && display.available().unwrap_or(false);
            misses = if present { 0 } else { misses + 1 };
            flag.store(problem.is_some() || misses >= HEALTH_MISSES, Ordering::Relaxed);
            std::thread::sleep(HEALTH_POLL);
        }
    });
    broken
}

pub fn pairing_info(identity: &Identity, wifi: &XiroWifi) -> PairingInfo {
    let on_wifi = wifi.is_on();
    let urls = identity.urls(on_wifi).unwrap_or_else(|err| {
        log::error!("{err}");
        pairing::PairingUrls { setup_url: String::new(), app_url: String::new() }
    });
    let now = identity.current();
    let mut addresses: Vec<String> = now.addresses.iter().map(|a| format!("{} ({})", a.address, a.name)).collect();
    addresses.push(identity.hostname.clone());
    let credentials = wifi.credentials();
    PairingInfo {
        setup_url: urls.setup_url,
        app_url: urls.app_url,
        fingerprint: now.certs.ca_fingerprint.clone(),
        addresses,
        wifi: on_wifi.then(|| WifiInfo { ssid: credentials.ssid.clone(), passphrase: credentials.passphrase.clone(), qr_text: credentials.qr_text() }),
    }
}

/// Briefly waits for the Xiro Wi-Fi address and a certificate covering it, so the code works.
fn wait_for_hosted_address(identity: &Identity) {
    for _ in 0..HOSTED_ADDRESS_TRIES {
        if let Err(err) = identity.refresh() {
            log::warning!("{err}");
        }
        if identity::hosted_address(&identity.current()).is_some() {
            return;
        }
        std::thread::sleep(HOSTED_ADDRESS_POLL);
    }
    log::warning!("Xiro Wi-Fi is on, but this PC has no address on it yet");
}

/// Starts Xiro Wi-Fi in the background (takes a few seconds), then refreshes the window's code.
pub fn start_xiro_wifi(wifi: Arc<XiroWifi>, identity: Arc<Identity>) {
    std::thread::spawn(move || {
        let lost = |why: String| xiro_shell::notify(TITLE, &format!("Xiro Wi-Fi stopped. {why}"));
        match wifi.turn_on(lost) {
            Ok(()) => {
                wait_for_hosted_address(&identity);
                xiro_shell::show_pairing();
            }
            // Switched off again while starting: that was the person's choice, not a failure.
            Err(_) if !wifi.wants_on() => {}
            Err(why) => xiro_shell::notify(TITLE, &format!("Could not start Xiro Wi-Fi. {why}")),
        }
    });
}

/// Asked whenever a device redeems a pairing code: the person at the PC has the final say.
pub fn approve_pairing(who: &str) -> bool {
    let text = "It will see this PC's screen and control its mouse and keyboard. Only allow it if you just scanned the code yourself. On public Wi-Fi, pair over Xiro Wi-Fi instead.";
    xiro_shell::ask_to_pair("Allow this device to pair?", who, text)
}

fn unlink_all(running: &Running) {
    let sure = confirm(
        Badge::Unlink,
        "Unlink every device?",
        "Every iPad and iPhone paired with this PC is signed out and disconnected now. To use one again, scan the code in Xiro.",
        ("Unlink all", Choice::Destructive),
        false,
    );
    if !sure {
        return;
    }
    match running.unlink_all() {
        Ok(_) => {
            xiro_shell::notify(TITLE, "Every device was unlinked. Scan the code to connect one again.");
            xiro_shell::show_pairing(); // a fresh code for whoever comes back
        }
        Err(why) => tell(Badge::Unlink, "Couldn't unlink", &why),
    }
}

/// Asks, removes Xiro from this PC and says what is left to do. True when it was removed.
pub fn uninstall(exe: &Path) -> bool {
    let sure = confirm(
        Badge::Remove,
        "Uninstall Xiro?",
        "This removes Xiro's display driver, its firewall rule, and your pairing and settings from this PC. Every paired iPad and iPhone is signed out.",
        ("Uninstall", Choice::Destructive),
        false,
    );
    if !sure {
        return false;
    }
    if let Err(why) = setup::uninstall_system() {
        tell(Badge::Remove, "Xiro couldn't remove everything", &why);
        return false;
    }
    let done = Prompt {
        buttons: vec![("Show Xiro.exe".to_owned(), Choice::Plain), ("Done".to_owned(), Choice::Main)],
        cancel: 1,
        ..prompt(
            Badge::Done,
            "Xiro is uninstalled",
            "Delete Xiro.exe to finish.\n\nOn each iPad or iPhone: delete the Xiro icon from the Home Screen, then Settings \u{203a} General \u{203a} VPN & Device Management \u{203a} Xiro Local CA \u{203a} Remove Profile.",
            Vec::new(),
            1,
        )
    };
    if xiro_shell::ask(done) == 0 {
        let _ = std::process::Command::new("explorer.exe").arg(format!("/select,{}", exe.display())).spawn();
    }
    // Last: the settings folder holds the open log, so it goes once Xiro has exited.
    if let Err(why) = setup::forget_everything() {
        log::warning!("Uninstall clean-up: {why}");
    }
    true
}

/// Restarts Xiro (the new process waits for this one) so it picks up the newly set up display.
fn restart() {
    match std::env::current_exe().and_then(|exe| std::process::Command::new(exe).arg("--restart").spawn()) {
        Ok(_) => xiro_shell::quit(),
        Err(err) => tell(Badge::Shield, "Please restart Xiro", &format!("The second screen is ready after a restart ({err}).")),
    }
}

fn toggle_autostart(exe: &Path) {
    let result = if autostart::is_enabled_for(exe) {
        autostart::disable();
        Ok(())
    } else {
        autostart::enable(&autostart::command(exe))
    };
    if let Err(err) = result {
        xiro_shell::notify(TITLE, &format!("Could not change autostart: {err}"));
    }
}

pub struct Desk<'a> {
    pub identity: &'a Arc<Identity>,
    pub wifi: &'a Arc<XiroWifi>,
    pub running: &'a Running,
    pub display_broken: Arc<AtomicBool>,
    /// Xiro's HTTP and HTTPS ports, for the firewall rule a repair writes.
    pub ports: [u16; 2],
}

pub fn run_tray(desk: &Desk, status: Arc<Mutex<Status>>, open_window: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if open_window {
        xiro_shell::show_pairing();
    }
    let menu_state = || MenuState {
        autostart: autostart::is_enabled_for(&exe),
        xiro_wifi: match (desk.wifi.is_starting(), desk.wifi.is_on()) {
            (true, _) => WifiState::Starting,
            (false, true) => WifiState::On,
            (false, false) => WifiState::Off,
        },
        display: if desk.display_broken.load(Ordering::Relaxed) { DisplayHealth::NeedsRepair } else { DisplayHealth::Ready },
    };
    xiro_shell::run_tray(|| pairing_info(desk.identity, desk.wifi), status, menu_state, |action| match action {
        TrayAction::ToggleAutostart => toggle_autostart(&exe),
        TrayAction::ToggleXiroWifi if desk.wifi.is_on() => {
            let wifi = desk.wifi.clone();
            // Stopping the network takes a few seconds: not on the window's thread.
            std::thread::spawn(move || {
                wifi.turn_off();
                xiro_shell::show_pairing();
            });
        }
        TrayAction::ToggleXiroWifi => start_xiro_wifi(desk.wifi.clone(), desk.identity.clone()),
        TrayAction::UnlinkAll => unlink_all(desk.running),
        TrayAction::RepairDisplay => {
            if offer_setup(SetupNeed::Repair, &desk.ports) {
                restart();
            }
        }
        TrayAction::Uninstall => {
            if uninstall(&exe) {
                xiro_shell::quit();
            }
        }
        TrayAction::OpenLogFolder => {
            let _ = std::process::Command::new("explorer.exe").arg(config::data_dir()).spawn();
        }
        TrayAction::Quit => {}
    })
}
