//! Everything the main window says, apart from drawing so it is easy to read and test.
//! Plain words: the reader may not know what certificates or drivers are.

use crate::{PairingInfo, WifiState};

pub(crate) const WINDOW_TITLE: &str = "Xiro";
pub(crate) const HEADING: &str = "Scan with your iPad\u{2019}s Camera";
pub(crate) const STEP_JOIN: &str = "1  Join Xiro Wi-Fi";
pub(crate) const STEP_OPEN: &str = "2  Open Xiro";
pub(crate) const COPY: &str = "Copy";
pub(crate) const COPIED: &str = "Copied";
pub(crate) const THIS_PC: &str = "This PC";
pub(crate) const SECOND_SCREEN: &str = "Second screen";
pub(crate) const SLOT_WAITING: &str = "Appears here when your iPad or iPhone connects";
pub(crate) const SLOT_REPAIR: &str = "Its display driver isn\u{2019}t running. Until it is, the device mirrors this PC.";
pub(crate) const REPAIR: &str = "Fix";
pub(crate) const PASSWORD: &str = "Password";

pub(crate) const WIFI_TITLE: &str = "Xiro Wi-Fi";
pub(crate) const AUTOSTART_TITLE: &str = "Start with Windows";
pub(crate) const AUTOSTART_DETAIL: &str = "Waits in the tray, ready when you are.";
pub(crate) const DEVICES_TITLE: &str = "Paired devices";
pub(crate) const DEVICES_DETAIL: &str = "Sign them all out. Scan again to pair.";
pub(crate) const UNLINK: &str = "Unlink all devices";
pub(crate) const HELP: &str = "Help";
pub(crate) const LOGS: &str = "Logs";
pub(crate) const UNINSTALL: &str = "Uninstall";

pub(crate) const BACK: &str = "Back";
pub(crate) const HELP_TITLE: &str = "Unlinking and removing Xiro";
pub(crate) const HELP_SECTIONS: [(&str, &str); 3] = [
    (
        "Unlink a device",
        "Choose Unlink all devices. Every iPad and iPhone is signed out at once; scan the code again to bring one back.",
    ),
    (
        "On each iPad or iPhone",
        "1. Delete the Xiro icon from the Home Screen.\n2. Settings \u{203a} General \u{203a} VPN & Device Management \u{203a} Xiro Local CA \u{203a} Remove Profile.",
    ),
    (
        "On this PC",
        "Choose Uninstall. It removes the display driver, the firewall rule, your pairing and settings. Then delete Xiro.exe.",
    ),
];
pub(crate) const FINGERPRINT_LABEL: &str = "This PC\u{2019}s certificate";

/// The line under the Xiro Wi-Fi switch.
pub(crate) fn wifi_detail(state: WifiState, info: &PairingInfo) -> String {
    match (state, &info.wifi) {
        (WifiState::Starting, _) => "Starting\u{2026}".to_owned(),
        (WifiState::On, Some(wifi)) => format!("On as \u{201c}{}\u{201d}", wifi.ssid),
        _ => "For places with no Wi-Fi you share with the PC.".to_owned(),
    }
}

/// Under the code: what to do with it.
pub(crate) fn caption(joining: bool, info: &PairingInfo) -> String {
    match (&info.wifi, joining) {
        (Some(wifi), true) => format!("Tap Join to connect to \u{201c}{}\u{201d}, then go to step 2.", wifi.ssid),
        _ => "Tap the link that appears, follow Safari\u{2019}s two quick steps, then choose Allow on this PC.".to_owned(),
    }
}

/// The certificate fingerprint in two halves, easy to compare with the one Safari shows.
pub(crate) fn fingerprint(info: &PairingInfo) -> String {
    let groups: Vec<&str> = info.fingerprint.trim().split(':').collect();
    let half = groups.len().div_ceil(2);
    groups.chunks(half.max(1)).map(|g| g.join(":")).collect::<Vec<_>>().join("\n")
}

/// Other ways to reach this PC, for when the code's address is not reachable.
pub(crate) fn addresses(info: &PairingInfo) -> String {
    let mut lines = Vec::new();
    if !info.app_url.trim().is_empty() {
        lines.push(format!("Already set up? Open {}", info.app_url.trim()));
    }
    let others: Vec<&str> = info.addresses.iter().map(|a| a.trim()).filter(|a| !a.is_empty()).collect();
    if !others.is_empty() {
        lines.push(format!("This PC is also at {}", others.join(", ")));
    }
    lines.join("\n")
}

/// Is a device on the second screen right now?
pub(crate) fn is_live(status: &str) -> bool {
    status.contains("connected") && !status.contains("disconnected")
}

/// The connected device's screen, as the status line reports it
/// ("1 device connected \u{b7} 2360\u{d7}1640 \u{b7} 60 fps").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LiveScreen {
    pub width: u32,
    pub height: u32,
    pub fps: Option<u32>,
}

pub(crate) fn live_screen(status: &str) -> Option<LiveScreen> {
    if !is_live(status) {
        return None;
    }
    let parts: Vec<&str> = status.split('\u{b7}').map(str::trim).collect();
    let (width, height) = parts.iter().find_map(|p| {
        let (w, h) = p.split_once('\u{d7}')?;
        Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
    })?;
    let fps = parts.iter().find_map(|p| p.strip_suffix("fps")?.trim().parse().ok());
    Some(LiveScreen { width, height, fps })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WifiInfo;

    fn info(wifi: bool) -> PairingInfo {
        PairingInfo {
            setup_url: "http://192.168.1.5:8080/#c=x".into(),
            app_url: "https://192.168.1.5:8443/#c=x".into(),
            fingerprint: "AB:CD:EF:01".into(),
            addresses: vec!["10.0.0.2 (Ethernet)".into(), " ".into(), "MYPC.local".into()],
            wifi: wifi.then(|| WifiInfo { ssid: "Xiro-PC".into(), passphrase: "pass".into(), qr_text: "WIFI:".into() }),
        }
    }

    #[test]
    fn the_wifi_line_follows_its_state() {
        assert!(wifi_detail(WifiState::Off, &info(false)).starts_with("For places"));
        assert_eq!(wifi_detail(WifiState::Starting, &info(false)), "Starting\u{2026}");
        assert!(wifi_detail(WifiState::On, &info(true)).contains("\u{201c}Xiro-PC\u{201d}"));
        assert!(wifi_detail(WifiState::On, &info(false)).starts_with("For places"), "no network yet");
    }

    #[test]
    fn captions_say_what_to_do_with_each_code() {
        assert!(caption(true, &info(true)).contains("Join to connect to \u{201c}Xiro-PC\u{201d}"));
        assert!(caption(false, &info(true)).contains("Allow on this PC"));
        assert!(caption(true, &info(false)).contains("Allow on this PC"), "no join step without Xiro Wi-Fi");
    }

    #[test]
    fn help_explains_unlinking_and_removal_on_both_sides() {
        let all: String = HELP_SECTIONS.iter().map(|(h, b)| format!("{h} {b}")).collect();
        assert!(all.contains("Unlink all devices"));
        assert!(all.contains("VPN & Device Management"));
        assert!(all.contains("Uninstall."));
        assert!(all.contains("delete Xiro.exe"));
    }

    #[test]
    fn fingerprints_split_in_halves_and_addresses_skip_blanks() {
        assert_eq!(fingerprint(&info(false)), "AB:CD\nEF:01");
        assert_eq!(
            addresses(&info(false)),
            "Already set up? Open https://192.168.1.5:8443/#c=x\nThis PC is also at 10.0.0.2 (Ethernet), MYPC.local"
        );
    }

    #[test]
    fn the_connected_screen_is_read_from_the_status_line() {
        let live = "1 device connected \u{b7} 2360\u{d7}1640 \u{b7} 60 fps";
        assert!(is_live(live));
        assert_eq!(live_screen(live), Some(LiveScreen { width: 2360, height: 1640, fps: Some(60) }));
        assert_eq!(live_screen("1 device connected"), None, "no size, nothing to draw");
        assert_eq!(live_screen("1 device connected \u{b7} 1640\u{d7}2360").map(|s| s.fps), Some(None));
        assert!(!is_live("Waiting for an iPad or iPhone"));
        assert_eq!(live_screen("device disconnected \u{b7} 2360\u{d7}1640"), None);
    }
}
