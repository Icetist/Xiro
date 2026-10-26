//! Pairing links (shown as QR codes) and their one-time codes. The link is plain HTTP, so it never
//! carries the long-lived token: the page redeems a short-lived single-use code over HTTPS.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use qrcodegen::{QrCode, QrCodeEcc};
use serde_json::{Map, Value, json};

use crate::config::{read_json, token_matches, write_json};

/// Long enough to scan, trust the certificate and come back; short enough that a leaked code is useless.
pub const CODE_LIFETIME_MS: u64 = 15 * 60 * 1000;
const CODE_BYTES: usize = 16;

pub struct PairingUrls {
    /// Plain-HTTP setup page: installs trust in the PC's certificate, then hands off.
    pub setup_url: String,
    /// The secure viewer, for devices that already trust the PC.
    pub app_url: String,
}

/// Encodes a URL fragment value like URLSearchParams does for our characters.
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => (b as char).to_string(),
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// `hostname` is the mDNS name (e.g. MYPC.local): it survives the PC getting a new IP.
pub fn pairing_urls(ip: &str, hostname: &str, code: &str, http_port: u16, https_port: u16) -> PairingUrls {
    let hash = format!("c={}&p={https_port}&h={}", encode(code), encode(hostname));
    PairingUrls { setup_url: format!("http://{ip}:{http_port}/#{hash}"), app_url: format!("https://{ip}:{https_port}/#c={}", encode(code)) }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// One-time codes, kept in a small file so `--pair` in a terminal works while the tray app runs.
pub struct PairingCodes {
    file: PathBuf,
    /// Issue and redeem both rewrite the file: serialised so a redeemed code can't be written back.
    lock: Mutex<()>,
}

impl PairingCodes {
    pub fn new(file: PathBuf) -> Self {
        Self { file, lock: Mutex::new(()) }
    }

    fn exclusive(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Live codes only (expired ones are dropped whenever the file is touched).
    fn live(&self, now: u64) -> Map<String, Value> {
        let mut codes = read_json(&self.file).and_then(|v| v.as_object().cloned()).unwrap_or_default();
        codes.retain(|_, expires| expires.as_u64().is_some_and(|e| e > now));
        codes
    }

    pub fn issue(&self) -> std::io::Result<String> {
        use base64::Engine;
        let mut bytes = [0u8; CODE_BYTES];
        getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
        let code = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let _one = self.exclusive();
        let now = now_ms();
        let mut codes = self.live(now);
        codes.insert(code.clone(), json!(now + CODE_LIFETIME_MS));
        write_json(&self.file, &Value::Object(codes))?;
        Ok(code)
    }

    /// Forgets every code not used yet (after "Unlink all", old QR codes are worthless).
    pub fn clear(&self) -> std::io::Result<()> {
        let _one = self.exclusive();
        write_json(&self.file, &Value::Object(Map::new()))
    }

    /// True once per issued code, within its lifetime.
    pub fn redeem(&self, code: &str) -> bool {
        let _one = self.exclusive();
        let now = now_ms();
        let mut codes = self.live(now);
        // Constant-time comparison against every live code: timing reveals nothing.
        let found = codes.keys().find(|known| token_matches(known, Some(code))).cloned();
        let Some(found) = found else { return false };
        codes.remove(&found);
        write_json(&self.file, &Value::Object(codes)).is_ok()
    }
}

/// The QR code as rows of modules (true = dark), without a quiet zone.
pub fn qr_modules(text: &str) -> Option<Vec<Vec<bool>>> {
    let qr = QrCode::encode_text(text, QrCodeEcc::Medium).ok()?;
    let size = qr.size();
    Some((0..size).map(|y| (0..size).map(|x| qr.get_module(x, y)).collect()).collect())
}

/// Terminal rendering: two rows per line with half-block characters, light quiet zone.
pub fn qr_lines(text: &str) -> Vec<String> {
    const QUIET: usize = 2;
    let Some(modules) = qr_modules(text) else { return Vec::new() };
    let size = modules.len();
    let dark = |x: usize, y: usize| x >= QUIET && y >= QUIET && x - QUIET < size && y - QUIET < size && modules[y - QUIET][x - QUIET];
    let span = size + 2 * QUIET;
    (0..span)
        .step_by(2)
        .map(|y| {
            (0..span)
                .map(|x| match (dark(x, y), dark(x, y + 1)) {
                    (true, true) => ' ',
                    (true, false) => '▄',
                    (false, true) => '▀',
                    (false, false) => '█',
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_carry_a_code_never_the_token() {
        let urls = pairing_urls("192.168.1.5", "MY-PC.local", "abc_DEF-123", 8080, 8443);
        assert_eq!(urls.setup_url, "http://192.168.1.5:8080/#c=abc_DEF-123&p=8443&h=MY-PC.local");
        assert_eq!(urls.app_url, "https://192.168.1.5:8443/#c=abc_DEF-123");
        assert_eq!(encode("a b/&"), "a+b%2F%26");
    }

    #[test]
    fn codes_work_once_and_expire() {
        let dir = std::env::temp_dir().join(format!("xiro-codes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let codes = PairingCodes::new(dir.join("pairing-codes.json"));
        let a = codes.issue().unwrap();
        let b = codes.issue().unwrap();
        assert_ne!(a, b);
        assert!(a.len() >= 22);
        assert!(codes.redeem(&a));
        assert!(!codes.redeem(&a), "single use");
        assert!(!codes.redeem("guess"));
        assert!(codes.redeem(&b), "other codes stay valid");
        let c = codes.issue().unwrap();
        write_json(&dir.join("pairing-codes.json"), &json!({ c.clone(): 1 })).unwrap();
        assert!(!codes.redeem(&c), "expired");
        let d = codes.issue().unwrap();
        codes.clear().unwrap();
        assert!(!codes.redeem(&d), "cleared by an unlink");
    }

    #[test]
    fn qr_code_is_square_and_rendered_in_half_rows() {
        let modules = qr_modules("http://192.168.1.5:8080/#c=x").unwrap();
        assert!(modules.len() >= 21 && modules.iter().all(|row| row.len() == modules.len()));
        let n = qr_modules("hello").unwrap().len();
        let lines = qr_lines("hello");
        assert_eq!(lines.len(), (n + 4).div_ceil(2));
        assert!(lines.iter().all(|l| l.chars().count() == n + 4));
    }
}
