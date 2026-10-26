//! Local CA and TLS for the HTTPS server; Safari only exposes WebCodecs in secure contexts.
//! The CA is name-constrained to local names and private IPv4 so a stolen key can't impersonate public sites.
//!
//! RUSTSEC-2023-0071 does not apply: `rsa` only signs certificates offline at startup, with blinding;
//! TLS handshakes run in SChannel.

mod issue;
mod meta;
mod names;
mod store;
#[cfg(test)]
mod tests;

use std::fmt;
use std::path::Path;
use std::time::SystemTime;

use der::Encode;
use sha2::{Digest, Sha256};

pub use names::{PERMITTED_DNS, PERMITTED_IP_RANGES, is_permitted_name};

use issue::{Ca, has_name_constraints, key_matches, not_after};
use meta::{RENEW_BEFORE_DAYS, RenewalCheck, ServerMeta, needs_renewal};
use store::{CA_CRT, CA_KEY, KeyFormat, SERVER_CRT, SERVER_KEY, SERVER_META};

/// Certificates ready for the HTTPS server.
#[derive(Clone)]
pub struct CertBundle {
    /// Server certificate PEM followed by the CA PEM.
    pub cert_pem: String,
    /// Server private key, PKCS#8 PEM.
    pub key_pem: String,
    /// CA certificate DER, offered for download so the iPad can trust it.
    pub ca_der: Vec<u8>,
    /// SHA-256 of `ca_der` formatted as iOS displays it.
    pub ca_fingerprint: String,
    pub renewed: bool,
    /// A new CA was created, so the iPad must trust it again.
    pub ca_created: bool,
    /// Requested names that the CA's name constraints permit.
    pub sans: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertError(String);

impl CertError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for CertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CertError {}

macro_rules! cert_error_from {
    ($($source:ty => $what:literal),* $(,)?) => {$(
        impl From<$source> for CertError {
            fn from(e: $source) -> Self {
                Self(format!(concat!($what, ": {}"), e))
            }
        }
    )*};
}

cert_error_from! {
    der::Error => "DER encoding error",
    spki::Error => "public key encoding error",
    pkcs8::Error => "PKCS#8 error",
    x509_cert::builder::Error => "certificate builder error",
    std::io::Error => "I/O error",
}

/// SHA-256 as iOS shows it in "More Details": uppercase hex pairs separated by spaces.
pub fn fingerprint(der: &[u8]) -> String {
    let digest = Sha256::digest(der);
    let pairs: Vec<String> = digest.iter().map(|b| format!("{b:02X}")).collect();
    pairs.join(" ")
}

/// The stored CA, or `None` if it lacks name constraints, expires within
/// `RENEW_BEFORE_DAYS`, or its key doesn't match.
fn load_ca(dir: &Path, now: SystemTime) -> Result<Option<Ca>, CertError> {
    let (Some(cert_pem), Some(key_pem)) = (store::read_optional(dir, CA_CRT)?, store::read_optional(dir, CA_KEY)?)
    else {
        return Ok(None);
    };
    let Some(cert) = store::parse_chain(&cert_pem).and_then(|chain| chain.into_iter().next()) else {
        return Ok(None);
    };
    let renew_by = now + issue::duration_days(RENEW_BEFORE_DAYS);
    if !has_name_constraints(&cert) || not_after(&cert) <= renew_by {
        return Ok(None);
    }
    Ok(store::parse_private_key(&key_pem)
        .filter(|(key, _)| key_matches(&cert, key))
        .map(|(key, _)| Ca { cert, key }))
}

/// `converted`: the key on disk was PKCS#1 and must be rewritten as PKCS#8 for SChannel.
struct ServerFiles {
    key_pem: String,
    cert_pem: String,
    converted: bool,
}

fn load_server(dir: &Path) -> Result<Option<ServerFiles>, CertError> {
    let (Some(key_text), Some(cert_pem)) =
        (store::read_optional(dir, SERVER_KEY)?, store::read_optional(dir, SERVER_CRT)?)
    else {
        return Ok(None);
    };
    let Some((key, format)) = store::parse_private_key(&key_text) else { return Ok(None) };
    let Some(leaf) = store::parse_chain(&cert_pem).and_then(|chain| chain.into_iter().next()) else {
        return Ok(None);
    };
    if !key_matches(&leaf, &key) {
        return Ok(None);
    }
    let converted = format == KeyFormat::Pkcs1;
    let key_pem = if converted { store::key_to_pem(&key)? } else { key_text };
    Ok(Some(ServerFiles { key_pem, cert_pem, converted }))
}

/// Loads or creates the CA and a server certificate for the permitted subset of `requested_sans`.
pub fn ensure_certificates(dir: &Path, requested_sans: &[String]) -> Result<CertBundle, CertError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| CertError::new(format!("cannot create certificate directory {}: {e}", dir.display())))?;
    // A name outside the CA's constraints would invalidate the whole certificate.
    let sans: Vec<String> = requested_sans.iter().filter(|s| is_permitted_name(s)).cloned().collect();
    if sans.is_empty() {
        return Err(CertError::new("No private network address found to issue a certificate for."));
    }
    let now = SystemTime::now();

    let (ca, ca_created) = match load_ca(dir, now)? {
        Some(ca) => (ca, false),
        None => {
            let ca = issue::create_ca()?;
            store::write_file(dir, CA_KEY, &store::key_to_pem(&ca.key)?, true)?;
            store::write_file(dir, CA_CRT, &store::cert_to_pem(&ca.cert)?, false)?;
            (ca, true)
        }
    };
    let ca_der = ca.cert.to_der()?;
    let ca_fingerprint = fingerprint(&ca_der);

    let meta = store::read_optional(dir, SERVER_META)?.and_then(|text| ServerMeta::parse(&text));
    let existing = load_server(dir)?;
    let renewed = needs_renewal(&RenewalCheck {
        ca_created,
        meta: meta.as_ref(),
        ca_fingerprint: &ca_fingerprint,
        sans: &sans,
        server_files_ok: existing.is_some(),
        now,
    });

    let (key_pem, cert_pem) = match existing {
        Some(files) if !renewed => {
            if files.converted {
                store::write_file(dir, SERVER_KEY, &files.key_pem, true)?;
            }
            (files.key_pem, files.cert_pem)
        }
        _ => {
            let server = issue::issue_server_cert(&ca, &sans)?;
            let key_pem = store::key_to_pem(&server.key)?;
            let cert_pem = store::cert_to_pem(&server.cert)? + &store::cert_to_pem(&ca.cert)?;
            let record = ServerMeta {
                sans: sans.clone(),
                ca: ca_fingerprint.clone(),
                not_after: meta::format_iso(not_after(&server.cert))?,
            };
            store::write_file(dir, SERVER_KEY, &key_pem, true)?;
            store::write_file(dir, SERVER_CRT, &cert_pem, false)?;
            store::write_file(dir, SERVER_META, &record.to_json()?, false)?;
            (key_pem, cert_pem)
        }
    };

    Ok(CertBundle { cert_pem, key_pem, ca_der, ca_fingerprint, renewed, ca_created, sans })
}

const PEM_CERT_END: &str = "-----END CERTIFICATE-----";

fn leaf_pem(chain_pem: &str) -> Result<&str, CertError> {
    chain_pem
        .find(PEM_CERT_END)
        .map(|end| &chain_pem[..end + PEM_CERT_END.len()])
        .ok_or_else(|| CertError::new("server certificate PEM is empty"))
}

/// Only the leaf is passed: SChannel's `from_pkcs8` picks the *last* PEM cert as the
/// identity, so the chain would select the keyless CA (SEC_E_NO_CREDENTIALS).
pub fn tls_acceptor(bundle: &CertBundle) -> Result<tokio_native_tls::TlsAcceptor, CertError> {
    let leaf = leaf_pem(&bundle.cert_pem)?;
    let identity = native_tls::Identity::from_pkcs8(leaf.as_bytes(), bundle.key_pem.as_bytes())
        .map_err(|e| CertError::new(format!("cannot load server identity: {e}")))?;
    let acceptor = native_tls::TlsAcceptor::builder(identity)
        .min_protocol_version(Some(native_tls::Protocol::Tlsv12))
        .build()
        .map_err(|e| CertError::new(format!("cannot create TLS acceptor: {e}")))?;
    Ok(tokio_native_tls::TlsAcceptor::from(acceptor))
}
