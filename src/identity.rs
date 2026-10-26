//! The PC's network identity: addresses, HTTPS certificate and pairing secrets. The certificate
//! is reissued when the PC changes networks; the CA stays, so trusting devices keep trusting it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use tokio_native_tls::TlsAcceptor;

use crate::config::{self, Config};
use crate::log;
use crate::network::{self, LanAddress};
use crate::pairing::{self, PairingCodes, PairingUrls};

/// The PC as it is reachable right now.
pub struct Snapshot {
    /// Best first.
    pub addresses: Vec<LanAddress>,
    pub certs: xiro_net::CertBundle,
    pub tls: Arc<TlsAcceptor>,
    /// Pairing link host: the best address the certificate covers, or the mDNS name when offline.
    pub host: String,
}

pub struct Identity {
    dir: PathBuf,
    pub token: Arc<config::PairingToken>,
    pub codes: Arc<PairingCodes>,
    pub hostname: String,
    http_port: u16,
    https_port: u16,
    current: RwLock<Arc<Snapshot>>,
    refreshing: Mutex<()>,
}

/// Same addresses, ignoring order (ranking ties may come back in another order).
fn same_addresses(a: &[LanAddress], b: &[LanAddress]) -> bool {
    a.len() == b.len() && a.iter().all(|x| b.contains(x))
}

/// The address pairing links use: the best one the certificate covers.
fn pick_host(addresses: &[LanAddress], sans: &[String], hostname: &str) -> String {
    addresses
        .iter()
        .map(|a| a.address.to_string())
        .find(|a| sans.contains(a))
        .unwrap_or_else(|| hostname.to_owned())
}

fn snapshot(dir: &Path, hostname: &str, addresses: Vec<LanAddress>) -> Result<Snapshot, String> {
    let mut sans = vec![hostname.to_owned(), "localhost".to_owned()];
    sans.extend(addresses.iter().map(|a| a.address.to_string()));
    let certs = xiro_net::ensure_certificates(&dir.join("certs"), &sans).map_err(|e| e.to_string())?;
    if certs.renewed {
        log::info!("Issued a new HTTPS certificate for this network.");
    }
    let tls = Arc::new(xiro_net::tls_acceptor(&certs).map_err(|e| e.to_string())?);
    let host = pick_host(&addresses, &certs.sans, hostname);
    Ok(Snapshot { addresses, certs, tls, host })
}

impl Identity {
    /// Works without a network: the PC is then known only by its mDNS name.
    pub fn load(dir: &Path, config: &Config, new_token: bool) -> Result<Self, String> {
        let hostname = network::local_hostname();
        let current = snapshot(dir, &hostname, network::lan_addresses())?;
        let token = config::PairingToken::load(dir, new_token).map_err(|e| format!("Could not store the pairing code: {e}"))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            token: Arc::new(token),
            codes: Arc::new(PairingCodes::new(dir.join("pairing-codes.json"))),
            hostname,
            http_port: config.http_port,
            https_port: config.https_port,
            current: RwLock::new(Arc::new(current)),
            refreshing: Mutex::new(()),
        })
    }

    pub fn current(&self) -> Arc<Snapshot> {
        self.current.read().map_or_else(|poisoned| poisoned.into_inner().clone(), |s| s.clone())
    }

    /// Re-reads the network; on a change reissues the certificate. Returns the new snapshot if changed.
    pub fn refresh(&self) -> Result<Option<Arc<Snapshot>>, String> {
        // One at a time: two reissues at once would write the same certificate files.
        let _one = self.refreshing.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let addresses = network::lan_addresses();
        if same_addresses(&addresses, &self.current().addresses) {
            return Ok(None);
        }
        let next = Arc::new(snapshot(&self.dir, &self.hostname, addresses)?);
        match self.current.write() {
            Ok(mut slot) => *slot = next.clone(),
            Err(poisoned) => *poisoned.into_inner() = next.clone(),
        }
        Ok(Some(next))
    }

    /// Pairing links with a fresh one-time code (never the long-lived token).
    /// With `on_xiro_wifi`, they point at the PC's address on its own network.
    pub fn urls(&self, on_xiro_wifi: bool) -> Result<PairingUrls, String> {
        let code = self.codes.issue().map_err(|e| format!("Could not create a pairing code: {e}"))?;
        let now = self.current();
        let host = if on_xiro_wifi { hosted_address(&now).unwrap_or_else(|| now.host.clone()) } else { now.host.clone() };
        Ok(pairing::pairing_urls(&host, &self.hostname, &code, self.http_port, self.https_port))
    }
}

/// The PC's address on its hosted network, once assigned and covered by the certificate.
pub fn hosted_address(now: &Snapshot) -> Option<String> {
    now.addresses
        .iter()
        .filter(|a| a.hosted && !a.address.is_link_local())
        .map(|a| a.address.to_string())
        .find(|a| now.certs.sans.contains(a))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(ip: &str, name: &str) -> LanAddress {
        LanAddress { address: ip.parse().unwrap(), name: name.into(), hosted: name.starts_with("Local Area Connection*") }
    }

    #[test]
    fn address_changes_ignore_order() {
        let (a, b) = (addr("192.168.1.5", "Wi-Fi"), addr("192.168.137.1", "Local Area Connection* 2"));
        assert!(same_addresses(&[a.clone(), b.clone()], &[b.clone(), a.clone()]));
        assert!(!same_addresses(&[a.clone()], &[a.clone(), b.clone()]));
        assert!(!same_addresses(&[a], &[b]));
        assert!(same_addresses(&[], &[]));
    }

    #[test]
    fn links_point_at_the_best_covered_address_or_the_name() {
        let addresses = [addr("8.8.8.8", "odd"), addr("192.168.1.5", "Wi-Fi")];
        let sans = vec!["PC.local".to_owned(), "192.168.1.5".to_owned()];
        assert_eq!(pick_host(&addresses, &sans, "PC.local"), "192.168.1.5");
        assert_eq!(pick_host(&[], &sans, "PC.local"), "PC.local", "no network yet: the mDNS name");
    }

    #[test]
    fn follows_network_changes_and_keeps_the_ca() {
        let dir = std::env::temp_dir().join(format!("xiro-identity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = snapshot(&dir, "PC.local", vec![addr("192.168.1.5", "Wi-Fi")]).unwrap();
        let moved = snapshot(&dir, "PC.local", vec![addr("172.20.10.2", "Ethernet 3")]).unwrap();
        assert_eq!(moved.host, "172.20.10.2");
        assert!(moved.certs.sans.contains(&"172.20.10.2".to_owned()) && !moved.certs.sans.contains(&"192.168.1.5".to_owned()));
        assert_eq!(first.certs.ca_fingerprint, moved.certs.ca_fingerprint, "devices keep trusting the PC");
        let both = snapshot(&dir, "PC.local", vec![addr("192.168.1.5", "Wi-Fi"), addr("192.168.137.1", "Local Area Connection* 13")]).unwrap();
        assert_eq!(both.host, "192.168.1.5");
        assert_eq!(hosted_address(&both).as_deref(), Some("192.168.137.1"), "Xiro Wi-Fi links use the hosted network");
        assert_eq!(hosted_address(&moved), None);
        let offline = snapshot(&dir, "PC.local", Vec::new()).unwrap();
        assert_eq!(offline.host, "PC.local");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
