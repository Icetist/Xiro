//! The network's name and password, and the QR code text that joins it.

/// Wi-Fi network names are at most 32 bytes.
const MAX_SSID_BYTES: usize = 32;
const PREFIX: &str = "Xiro-";
/// WPA2 needs 8 to 63 characters.
const MIN_PASSPHRASE: usize = 8;
const MAX_PASSPHRASE: usize = 63;
const PASSPHRASE_LEN: usize = 16;
/// No look-alikes (0/O, 1/l/I), so the password can also be typed from the screen.
const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub ssid: String,
    pub passphrase: String,
}

/// "Xiro-" and the PC's name, kept to letters, digits and dashes.
pub fn network_name(pc_name: &str) -> String {
    let base: String = pc_name
        .split('.')
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let base = if base.is_empty() { "PC".to_owned() } else { base };
    let mut name = format!("{PREFIX}{base}");
    name.truncate(MAX_SSID_BYTES);
    name
}

impl Credentials {
    /// A new network for this PC, with a random password.
    pub fn generate(pc_name: &str) -> std::io::Result<Self> {
        let mut bytes = [0u8; PASSPHRASE_LEN];
        getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
        // 256 is not a multiple of the alphabet's 56: the bias is a few percent per
        // character, irrelevant for a 16-character WPA2 password.
        let passphrase = bytes.iter().map(|b| ALPHABET[usize::from(*b) % ALPHABET.len()] as char).collect();
        Ok(Self { ssid: network_name(pc_name), passphrase })
    }

    /// Stored credentials are only reused when Windows will accept them.
    pub fn is_valid(&self) -> bool {
        let ssid_ok = !self.ssid.is_empty() && self.ssid.len() <= MAX_SSID_BYTES && self.ssid.is_ascii();
        let pass_ok = (MIN_PASSPHRASE..=MAX_PASSPHRASE).contains(&self.passphrase.len())
            && self.passphrase.bytes().all(|b| b.is_ascii_graphic());
        ssid_ok && pass_ok
    }

    /// The text of a QR code the iPhone/iPad camera offers to join ("Join network …").
    pub fn qr_text(&self) -> String {
        format!("WIFI:T:WPA;S:{};P:{};;", escape(&self.ssid), escape(&self.passphrase))
    }
}

/// The Wi-Fi QR format escapes `\ ; , : "` with a backslash.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '\\' | ';' | ',' | ':' | '"') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_come_from_the_pc_and_fit() {
        assert_eq!(network_name("DESK-01.local"), "Xiro-DESK-01");
        assert_eq!(network_name("Ünï cødé"), "Xiro-ncd");
        assert_eq!(network_name(""), "Xiro-PC");
        assert_eq!(network_name(&"A".repeat(60)).len(), MAX_SSID_BYTES);
    }

    #[test]
    fn generated_credentials_are_valid_and_random() {
        let a = Credentials::generate("PC").unwrap();
        let b = Credentials::generate("PC").unwrap();
        assert!(a.is_valid() && b.is_valid());
        assert_eq!(a.passphrase.len(), PASSPHRASE_LEN);
        assert_ne!(a.passphrase, b.passphrase);
        assert!(a.passphrase.bytes().all(|c| ALPHABET.contains(&c)));
    }

    #[test]
    fn rejects_what_windows_would() {
        let ok = Credentials { ssid: "Xiro-PC".into(), passphrase: "abcdefgh".into() };
        assert!(ok.is_valid());
        assert!(!Credentials { passphrase: "short".into(), ..ok.clone() }.is_valid());
        assert!(!Credentials { passphrase: "x".repeat(64), ..ok.clone() }.is_valid());
        assert!(!Credentials { passphrase: "has space".into(), ..ok.clone() }.is_valid());
        assert!(!Credentials { ssid: String::new(), ..ok.clone() }.is_valid());
        assert!(!Credentials { ssid: "x".repeat(33), ..ok }.is_valid());
    }

    #[test]
    fn qr_text_escapes_special_characters() {
        let c = Credentials { ssid: "Xiro-PC".into(), passphrase: r#"a;b,c:d"e\f"#.into() };
        assert_eq!(c.qr_text(), r#"WIFI:T:WPA;S:Xiro-PC;P:a\;b\,c\:d\"e\\f;;"#);
    }
}
