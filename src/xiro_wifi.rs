//! Xiro Wi-Fi, toggled from the window. Off at every start because hosting slows the PC's Wi-Fi;
//! name and password persist (wifi.json) so devices rejoin by themselves.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::json;
use xiro_hotspot::{Credentials, Hotspot, State};

use crate::config::{read_json, write_json};
use crate::log;

pub struct XiroWifi {
    file: PathBuf,
    credentials: Credentials,
    hotspot: Mutex<Option<Hotspot>>,
    /// A start is in progress (it takes a few seconds).
    busy: AtomicBool,
    /// Bumped by every stop, so a start still in flight knows it was overtaken.
    generation: AtomicU64,
    /// The person's latest choice (a start that failed or was overtaken doesn't change it).
    wanted: AtomicBool,
}

fn load_credentials(file: &Path, pc_name: &str) -> std::io::Result<Credentials> {
    let stored = read_json(file).and_then(|v| {
        let get = |key: &str| v.get(key).and_then(|s| s.as_str()).map(str::to_owned);
        Some(Credentials { ssid: get("ssid")?, passphrase: get("passphrase")? })
    });
    match stored.filter(Credentials::is_valid) {
        Some(credentials) => Ok(credentials),
        None => Credentials::generate(pc_name),
    }
}

impl XiroWifi {
    /// The controller, off, with the saved network name and password (made on first use).
    pub fn load(dir: &Path, pc_name: &str) -> std::io::Result<Self> {
        let file = dir.join("wifi.json");
        let credentials = load_credentials(&file, pc_name)?;
        let wifi = Self {
            file,
            credentials,
            hotspot: Mutex::new(None),
            busy: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            wanted: AtomicBool::new(false),
        };
        wifi.save()?;
        Ok(wifi)
    }

    fn save(&self) -> std::io::Result<()> {
        write_json(&self.file, &json!({ "ssid": self.credentials.ssid, "passphrase": self.credentials.passphrase }))
    }

    /// Up right now (Windows can take the network down, e.g. when Wi-Fi is switched off).
    pub fn is_on(&self) -> bool {
        self.hotspot.lock().is_ok_and(|slot| slot.as_ref().is_some_and(Hotspot::is_up))
    }

    /// Whether the person last switched it on (it may still be starting, or have failed).
    pub fn wants_on(&self) -> bool {
        self.wanted.load(Ordering::SeqCst)
    }

    /// Coming up right now (it takes a few seconds).
    pub fn is_starting(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    pub fn credentials(&self) -> &Credentials {
        &self.credentials
    }

    /// Brings the network up (blocking, a few seconds); `on_lost` runs if Windows later drops it.
    pub fn turn_on(&self, on_lost: impl Fn(String) + Send + Sync + 'static) -> Result<(), String> {
        if self.busy.swap(true, Ordering::SeqCst) {
            return Err("Xiro Wi-Fi is already starting.".into());
        }
        self.wanted.store(true, Ordering::SeqCst);
        let generation = self.generation.load(Ordering::SeqCst);
        // A network Windows took down still holds the adapter until it is dropped.
        self.stop();
        let result = Hotspot::start(&self.credentials, move |state| match state {
            State::On => {}
            State::Off => on_lost("Xiro Wi-Fi stopped.".into()),
            State::Failed(why) => on_lost(why),
        });
        let outcome = result.and_then(|hotspot| self.keep(hotspot, generation));
        self.busy.store(false, Ordering::SeqCst);
        outcome
    }

    /// Stores a started network, unless someone turned Xiro Wi-Fi off meanwhile.
    fn keep(&self, hotspot: Hotspot, generation: u64) -> Result<(), String> {
        let mut slot = self.hotspot.lock().map_err(|_| "Xiro Wi-Fi is unavailable.".to_owned())?;
        if self.generation.load(Ordering::SeqCst) != generation + 1 {
            return Err("Xiro Wi-Fi was turned off while it was starting.".into());
        }
        *slot = Some(hotspot);
        drop(slot);
        log::info!("Xiro Wi-Fi is on: {}", self.credentials.ssid);
        Ok(())
    }

    fn stop(&self) {
        let mut slot = self.hotspot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.generation.fetch_add(1, Ordering::SeqCst);
        let old = slot.take();
        drop(slot); // stopping takes seconds: not while `is_on` (polled by the tray) waits
        drop(old);
    }

    pub fn turn_off(&self) {
        self.wanted.store(false, Ordering::SeqCst);
        self.stop();
        log::info!("Xiro Wi-Fi is off");
    }

    /// Off because Xiro quits.
    pub fn turn_off_for_exit(&self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xiro-wifi-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_network_keeps_its_name_and_password() {
        let dir = temp("keep");
        let first = XiroWifi::load(&dir, "DESK.local").unwrap();
        assert!(!first.is_on());
        assert_eq!(first.credentials().ssid, "Xiro-DESK");
        let again = XiroWifi::load(&dir, "DESK.local").unwrap();
        assert_eq!(again.credentials(), first.credentials(), "devices rejoin by themselves");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn starts_off_even_if_it_was_on_and_replaces_broken_files() {
        let dir = temp("state");
        let file = dir.join("wifi.json");
        write_json(&file, &json!({ "ssid": "Xiro-PC", "passphrase": "abcd2345efgh", "enabled": true })).unwrap();
        let wifi = XiroWifi::load(&dir, "PC").unwrap();
        assert!(!wifi.is_on() && !wifi.wants_on());
        assert_eq!(wifi.credentials().passphrase, "abcd2345efgh");
        assert!(read_json(&file).unwrap().get("enabled").is_none(), "nothing says to turn it on next time");
        write_json(&file, &json!({ "ssid": "Xiro-PC", "passphrase": "short" })).unwrap();
        assert!(XiroWifi::load(&dir, "PC").unwrap().credentials().is_valid());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
