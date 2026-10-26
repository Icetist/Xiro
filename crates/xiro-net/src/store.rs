//! Certificate files under `%LOCALAPPDATA%\Xiro\certs\`. Keys are read as PKCS#1 or
//! PKCS#8 and written as PKCS#8, so an already-trusted CA keeps loading.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use der::EncodePem;
use der::pem::LineEnding;
use pkcs1::DecodeRsaPrivateKey;
use pkcs8::{DecodePrivateKey, EncodePrivateKey};
use rsa::RsaPrivateKey;
use x509_cert::Certificate;

use crate::CertError;

pub(crate) const CA_KEY: &str = "ca.key";
pub(crate) const CA_CRT: &str = "ca.crt";
pub(crate) const SERVER_KEY: &str = "server.key";
pub(crate) const SERVER_CRT: &str = "server.crt";
pub(crate) const SERVER_META: &str = "server.json";

const TEMP_SUFFIX: &str = ".tmp";
const PKCS1_LABEL: &str = "-----BEGIN RSA PRIVATE KEY-----";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyFormat {
    Pkcs1,
    Pkcs8,
}

/// Only "not found" maps to `None`; treating e.g. an antivirus lock as missing
/// would replace a CA the iPad already trusts.
pub(crate) fn read_optional(dir: &Path, name: &str) -> Result<Option<String>, CertError> {
    match fs::read_to_string(dir.join(name)) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(CertError::new(format!("cannot read {name}: {e}"))),
    }
}

#[cfg(unix)]
fn open_for_write(path: &Path, secret: bool) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    const OWNER_ONLY: u32 = 0o600;
    const DEFAULT_MODE: u32 = 0o644;
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(if secret { OWNER_ONLY } else { DEFAULT_MODE })
        .open(path)
}

#[cfg(not(unix))]
fn open_for_write(path: &Path, _secret: bool) -> io::Result<fs::File> {
    // The inherited %LOCALAPPDATA% ACL already limits access to the user, SYSTEM and Administrators.
    fs::File::create(path)
}

/// Temp file plus rename, so a crash never leaves a half-written key behind.
pub(crate) fn write_file(dir: &Path, name: &str, contents: &str, secret: bool) -> Result<(), CertError> {
    let target = dir.join(name);
    let temp = dir.join(format!("{name}{TEMP_SUFFIX}"));
    let result = open_for_write(&temp, secret)
        .and_then(|mut file| {
            file.write_all(contents.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&temp, &target));
    result.map_err(|e| {
        // Best effort cleanup; the original error is what matters.
        let _ = fs::remove_file(&temp);
        CertError::new(format!("cannot write {name}: {e}"))
    })
}

pub(crate) fn parse_private_key(pem: &str) -> Option<(RsaPrivateKey, KeyFormat)> {
    if pem.contains(PKCS1_LABEL) {
        RsaPrivateKey::from_pkcs1_pem(pem.trim()).ok().map(|k| (k, KeyFormat::Pkcs1))
    } else {
        RsaPrivateKey::from_pkcs8_pem(pem.trim()).ok().map(|k| (k, KeyFormat::Pkcs8))
    }
}

/// PKCS#8, the form SChannel's identity import takes.
pub(crate) fn key_to_pem(key: &RsaPrivateKey) -> Result<String, CertError> {
    let pem = key.to_pkcs8_pem(LineEnding::LF)?;
    Ok(pem.as_str().to_owned())
}

pub(crate) fn cert_to_pem(cert: &Certificate) -> Result<String, CertError> {
    Ok(cert.to_pem(LineEnding::LF)?)
}

/// `None` if any part of the bundle fails to parse.
pub(crate) fn parse_chain(pem: &str) -> Option<Vec<Certificate>> {
    Certificate::load_pem_chain(pem.as_bytes()).ok().filter(|chain| !chain.is_empty())
}
