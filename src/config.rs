//! Persistent settings and the pairing token in %LOCALAPPDATA%\Xiro.
//! File formats stay compatible so an upgrade keeps existing settings.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

pub const FPS_RANGE: (f64, f64) = (15.0, 240.0);
pub const BITRATE_MBPS_RANGE: (f64, f64) = (1.0, 150.0);
pub const GOP_SECONDS_RANGE: (f64, f64) = (0.25, 10.0);
const TOKEN_BYTES: usize = 18;
const MIN_TOKEN_CHARS: usize = 22;
/// Retired keys; accepted silently so old config files still load.
const RETIRED_KEYS: [&str; 2] = ["ffmpegPath", "adapterIndex"];

/// Frame rate: the iPad's refresh rate, or a fixed cap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fps {
    Auto,
    Cap(u32),
}

/// Which monitor to stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayChoice {
    /// The iPad-shaped virtual monitor when the driver is installed, else the best existing one.
    Auto,
    Index(usize),
    Name(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub https_port: u16,
    pub http_port: u16,
    pub fps: Fps,
    pub bitrate_mbps: Option<f64>,
    /// "auto", "hardware", "software" or part of an encoder's name.
    pub encoder: String,
    pub display: DisplayChoice,
    /// Seconds between key frames: lower recovers faster after Wi-Fi hiccups, costs bandwidth.
    pub gop_seconds: f64,
    /// Map the iPad's Cmd key to Ctrl so Cmd+C / Cmd+V work as expected.
    pub cmd_as_ctrl: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            https_port: 8443,
            http_port: 8080,
            fps: Fps::Auto,
            bitrate_mbps: None,
            encoder: "auto".into(),
            display: DisplayChoice::Auto,
            gop_seconds: 1.0,
            cmd_as_ctrl: true,
        }
    }
}

pub fn default_json() -> Value {
    json!({
        "httpsPort": 8443,
        "httpPort": 8080,
        "fps": "auto",
        "bitrateMbps": null,
        "encoder": "auto",
        "display": "auto",
        "gopSeconds": 1,
        "cmdAsCtrl": true
    })
}

fn in_range(value: &Value, (lo, hi): (f64, f64)) -> Option<f64> {
    value.as_f64().filter(|v| v.is_finite() && *v >= lo && *v <= hi)
}

fn port(value: &Value) -> Option<u16> {
    value.as_u64().and_then(|p| u16::try_from(p).ok()).filter(|p| *p > 0)
}

fn reject(warnings: &mut Vec<String>, key: &str, why: &str) {
    warnings.push(format!("Config \"{key}\" {why}; using default"));
}

/// Merges user settings over the defaults, dropping invalid values with a warning each.
pub fn normalize(raw: &Value) -> (Config, Vec<String>) {
    let mut config = Config::default();
    let mut warnings = Vec::new();
    let empty = Map::new();
    let entries = raw.as_object().unwrap_or(&empty);
    for (key, value) in entries {
        match key.as_str() {
            "httpsPort" => match port(value) {
                Some(p) => config.https_port = p,
                None => reject(&mut warnings, key, "must be a port number"),
            },
            "httpPort" => match port(value) {
                Some(p) => config.http_port = p,
                None => reject(&mut warnings, key, "must be a port number"),
            },
            "fps" => match (value.as_str(), in_range(value, FPS_RANGE)) {
                (Some("auto"), _) => config.fps = Fps::Auto,
                (_, Some(f)) => config.fps = Fps::Cap(f.round() as u32),
                _ => reject(&mut warnings, key, "must be \"auto\" or between 15 and 240"),
            },
            "bitrateMbps" => match (value.is_null() || value.as_str() == Some("auto"), in_range(value, BITRATE_MBPS_RANGE)) {
                (true, _) => config.bitrate_mbps = None,
                (_, Some(b)) => config.bitrate_mbps = Some(b),
                _ => reject(&mut warnings, key, "must be null or between 1 and 150"),
            },
            "encoder" => match value.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                Some(e) => config.encoder = e.to_owned(),
                None => reject(&mut warnings, key, "must be a name"),
            },
            "display" => match value {
                Value::String(s) if s == "auto" => config.display = DisplayChoice::Auto,
                Value::String(s) => config.display = DisplayChoice::Name(s.clone()),
                Value::Number(n) if n.as_u64().is_some_and(|i| i >= 1) => {
                    config.display = DisplayChoice::Index(n.as_u64().unwrap_or(1) as usize);
                }
                _ => reject(&mut warnings, key, "must be \"auto\", a display number or a name like \\\\.\\DISPLAY2"),
            },
            "gopSeconds" => match in_range(value, GOP_SECONDS_RANGE) {
                Some(g) => config.gop_seconds = g,
                None => reject(&mut warnings, key, "must be between 0.25 and 10"),
            },
            "cmdAsCtrl" => match value.as_bool() {
                Some(b) => config.cmd_as_ctrl = b,
                None => reject(&mut warnings, key, "must be true or false"),
            },
            k if RETIRED_KEYS.contains(&k) => {}
            _ => warnings.push(format!("Unknown config key \"{key}\" ignored")),
        }
    }
    (config, warnings)
}

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| {
        let home = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
        home.join("AppData").join("Local")
    });
    base.join("Xiro")
}

/// Loads config.json, creating it with defaults on first run.
pub fn load(dir: &Path) -> (Config, Vec<String>) {
    let file = dir.join("config.json");
    let _ = fs::create_dir_all(dir);
    let Ok(text) = fs::read_to_string(&file) else {
        let _ = write_json(&file, &default_json());
        return (Config::default(), Vec::new());
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(raw) => normalize(&raw),
        Err(err) => (Config::default(), vec![format!("Could not read {} ({err}); using defaults", file.display())]),
    }
}

/// The long-lived pairing secret embedded in the QR code.
pub fn load_token(dir: &Path, regenerate: bool) -> std::io::Result<String> {
    let file = dir.join("token.txt");
    if !regenerate {
        if let Ok(existing) = fs::read_to_string(&file) {
            let existing = existing.trim();
            if existing.len() >= MIN_TOKEN_CHARS {
                return Ok(existing.to_owned());
            }
        }
    }
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
    let token = base64_url(&bytes);
    fs::create_dir_all(dir)?;
    // Atomic, like `write_json`: a crash never leaves half a token.
    let tmp = file.with_extension("txt.tmp");
    fs::write(&tmp, &token)?;
    fs::rename(&tmp, &file)?;
    Ok(token)
}

fn base64_url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The live pairing token. "Unlink all devices" replaces it, signing out every paired device.
pub struct PairingToken {
    dir: PathBuf,
    value: std::sync::RwLock<String>,
}

impl PairingToken {
    pub fn load(dir: &Path, regenerate: bool) -> std::io::Result<Self> {
        Ok(Self { dir: dir.to_path_buf(), value: std::sync::RwLock::new(load_token(dir, regenerate)?) })
    }

    /// A token that never touches the disk until rotated, for tests.
    #[cfg(test)]
    pub fn fixed(value: &str) -> Self {
        Self { dir: std::env::temp_dir().join(format!("xiro-token-{}", std::process::id())), value: std::sync::RwLock::new(value.to_owned()) }
    }

    pub fn get(&self) -> String {
        self.value.read().map_or_else(|poisoned| poisoned.into_inner().clone(), |v| v.clone())
    }

    pub fn matches(&self, provided: Option<&str>) -> bool {
        token_matches(&self.get(), provided)
    }

    /// Saves a new token before use, so a restart keeps old devices out too. Held locked
    /// throughout so two unlinks can't leave file and memory out of sync.
    pub fn rotate(&self) -> std::io::Result<()> {
        let mut slot = self.value.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = load_token(&self.dir, true)?;
        Ok(())
    }
}

/// Constant-time comparison, so response timing reveals nothing about the token.
pub fn token_matches(expected: &str, provided: Option<&str>) -> bool {
    let Some(provided) = provided else { return false };
    let (a, b) = (expected.as_bytes(), provided.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn read_json(file: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(file).ok()?).ok()
}

/// Atomic: a crash mid-write leaves the previous file, never a truncated one.
pub fn write_json(file: &Path, value: &Value) -> std::io::Result<()> {
    let tmp = file.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?)?;
    fs::rename(&tmp, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xiro-config-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn keeps_valid_values_and_rejects_bad_ones() {
        let (config, warnings) = normalize(&json!({ "fps": 120, "bitrateMbps": 9999, "bogus": 1 }));
        assert_eq!(config.fps, Fps::Cap(120));
        assert_eq!(config.bitrate_mbps, None);
        assert_eq!(warnings.len(), 2);
        assert_eq!(normalize(&json!({})).0.fps, Fps::Auto);
        assert!(normalize(&json!({ "fps": "auto" })).1.is_empty());
        assert!(normalize(&json!({ "fps": "fast" })).1[0].contains("fps"));
    }

    #[test]
    fn understands_display_choices_and_retired_keys() {
        let (c, w) = normalize(&json!({ "display": 2, "ffmpegPath": "C:\\ff.exe", "adapterIndex": null }));
        assert_eq!(c.display, DisplayChoice::Index(2));
        assert!(w.is_empty());
        assert_eq!(normalize(&json!({ "display": "\\\\.\\DISPLAY3" })).0.display, DisplayChoice::Name("\\\\.\\DISPLAY3".into()));
        assert_eq!(normalize(&json!({ "display": 0 })).1.len(), 1);
    }

    #[test]
    fn validates_ports_booleans_and_gop() {
        let (c, w) = normalize(&json!({ "httpsPort": 9443, "httpPort": 70000, "cmdAsCtrl": false, "gopSeconds": 0.5, "encoder": "software" }));
        assert_eq!(c.https_port, 9443);
        assert_eq!(c.http_port, 8080);
        assert!(!c.cmd_as_ctrl);
        assert_eq!(c.gop_seconds, 0.5);
        assert_eq!(c.encoder, "software");
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn creates_defaults_and_reads_them_back() {
        let dir = temp_dir("load");
        let (first, _) = load(&dir);
        assert_eq!(first, Config::default());
        assert!(dir.join("config.json").exists());
        fs::write(dir.join("config.json"), "{ not json").unwrap();
        let (fallback, warnings) = load(&dir);
        assert_eq!(fallback, Config::default());
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn token_is_stable_until_regenerated() {
        let dir = temp_dir("token");
        let t1 = load_token(&dir, false).unwrap();
        assert!(t1.len() >= MIN_TOKEN_CHARS);
        assert_eq!(load_token(&dir, false).unwrap(), t1);
        let t2 = load_token(&dir, true).unwrap();
        assert_ne!(t1, t2);
        assert!(token_matches(&t2, Some(&t2)));
        assert!(!token_matches(&t2, Some(&t1)));
        assert!(!token_matches(&t2, None));
        assert!(!token_matches(&t2, Some("short")));
    }

    #[test]
    fn unlinking_replaces_the_token_now_and_after_a_restart() {
        let dir = temp_dir("token-rotate");
        let token = PairingToken::load(&dir, false).unwrap();
        let old = token.get();
        assert!(token.matches(Some(&old)));
        token.rotate().unwrap();
        assert!(!token.matches(Some(&old)), "the old devices are out");
        assert!(token.matches(Some(&token.get())));
        assert_eq!(PairingToken::load(&dir, false).unwrap().get(), token.get(), "saved");
    }

    #[test]
    fn json_writes_are_atomic_and_readable() {
        let dir = temp_dir("json");
        let file = dir.join("x.json");
        write_json(&file, &json!({ "a": 1 })).unwrap();
        assert_eq!(read_json(&file), Some(json!({ "a": 1 })));
        assert!(!dir.join("x.json.tmp").exists());
        assert_eq!(read_json(&dir.join("missing.json")), None);
    }
}
