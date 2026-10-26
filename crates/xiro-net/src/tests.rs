//! File-level tests of `ensure_certificates`. RSA key generation is slow in
//! debug builds, so each test walks several scenarios over the same keys.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use const_oid::db::rfc5280;
use der::{Decode, Encode};
use pkcs1::EncodeRsaPrivateKey;
use pkcs8::der::pem::LineEnding;
use x509_cert::Certificate;
use x509_cert::ext::Extension;
use x509_cert::ext::pkix::constraints::name::NameConstraints;
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::{AuthorityKeyIdentifier, SubjectAltName, SubjectKeyIdentifier};

use crate::issue::{SERVER_DAYS, duration_days, has_name_constraints, name_constraints};
use crate::meta::ServerMeta;
use crate::store::{self, CA_CRT, CA_KEY, KeyFormat, SERVER_CRT, SERVER_KEY, SERVER_META};
use crate::{ensure_certificates, fingerprint, tls_acceptor};

const SLACK: Duration = Duration::from_secs(120);

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap()
}

fn chain(dir: &Path) -> Vec<Certificate> {
    store::parse_chain(&read(dir, SERVER_CRT)).unwrap()
}

fn extension<'a>(cert: &'a Certificate, oid: const_oid::ObjectIdentifier) -> &'a Extension {
    cert.tbs_certificate.extensions.iter().flatten().find(|e| e.extn_id == oid).unwrap()
}

fn assert_close(actual: SystemTime, expected: SystemTime) {
    let diff = actual.duration_since(expected).or_else(|_| expected.duration_since(actual)).unwrap();
    assert!(diff < SLACK, "{actual:?} vs {expected:?}");
}

fn check_ca(ca: &Certificate) {
    let nc = extension(ca, rfc5280::ID_CE_NAME_CONSTRAINTS);
    assert!(nc.critical, "NameConstraints must be critical");
    let parsed = NameConstraints::from_der(nc.extn_value.as_bytes()).unwrap();
    assert_eq!(parsed, name_constraints().unwrap());
    let permitted = parsed.permitted_subtrees.unwrap();
    assert_eq!(permitted.len(), 8);
    assert!(matches!(&permitted[0].base, GeneralName::DnsName(d) if d.as_str() == "local"));
    assert!(matches!(&permitted[1].base, GeneralName::DnsName(d) if d.as_str() == "localhost"));
    assert!(matches!(&permitted[5].base, GeneralName::IpAddress(ip) if ip.as_bytes() == [169, 254, 0, 0, 255, 255, 0, 0]));
    assert!(parsed.excluded_subtrees.is_none());
    assert!(extension(ca, rfc5280::ID_CE_BASIC_CONSTRAINTS).critical);
    assert!(extension(ca, rfc5280::ID_CE_KEY_USAGE).critical);
    assert_eq!(ca.tbs_certificate.issuer, ca.tbs_certificate.subject);
    // RFC 4514 prints RDNs last-first; the DER order is CN then O.
    assert_eq!(ca.tbs_certificate.subject.to_string(), "O=Xiro (this PC only),CN=Xiro Local CA");
}

fn check_server(leaf: &Certificate, ca: &Certificate) {
    let tbs = &leaf.tbs_certificate;
    assert_eq!(tbs.subject.to_string(), "CN=localhost");
    assert_eq!(tbs.issuer, ca.tbs_certificate.subject);
    let serial = tbs.serial_number.as_bytes();
    assert_eq!(serial.len(), 16);
    assert_eq!(serial[0], 0x01);
    let now = SystemTime::now();
    assert_close(tbs.validity.not_before.to_system_time(), now - duration_days(1));
    assert_close(tbs.validity.not_after.to_system_time(), now + duration_days(SERVER_DAYS));
    assert!(extension(leaf, rfc5280::ID_CE_KEY_USAGE).critical);
    let san = SubjectAltName::from_der(extension(leaf, rfc5280::ID_CE_SUBJECT_ALT_NAME).extn_value.as_bytes()).unwrap();
    assert!(matches!(&san.0[0], GeneralName::DnsName(d) if d.as_str() == "localhost"));
    assert!(matches!(&san.0[1], GeneralName::IpAddress(ip) if ip.as_bytes() == [127, 0, 0, 1]));
    assert!(matches!(&san.0[2], GeneralName::DnsName(d) if d.as_str() == "mypc.local"));
    assert_eq!(san.0.len(), 3, "8.8.8.8 must have been dropped");
    let aki = AuthorityKeyIdentifier::from_der(extension(leaf, rfc5280::ID_CE_AUTHORITY_KEY_IDENTIFIER).extn_value.as_bytes()).unwrap();
    let ski = SubjectKeyIdentifier::from_der(extension(ca, rfc5280::ID_CE_SUBJECT_KEY_IDENTIFIER).extn_value.as_bytes()).unwrap();
    assert_eq!(aki.key_identifier, Some(ski.0));
    extension(leaf, rfc5280::ID_CE_EXT_KEY_USAGE);
}

#[test]
fn issues_reuses_renews_and_replaces() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("certs");

    // 1. Fresh directory: CA and server certificate are created.
    let first = ensure_certificates(&dir, &names(&["localhost", "127.0.0.1", "mypc.local", "8.8.8.8"])).unwrap();
    assert!(first.ca_created && first.renewed);
    assert_eq!(first.sans, names(&["localhost", "127.0.0.1", "mypc.local"]));
    let ca = Certificate::from_der(&first.ca_der).unwrap();
    check_ca(&ca);
    assert_eq!(first.ca_fingerprint, fingerprint(&ca.to_der().unwrap()));
    let certs = chain(&dir);
    assert_eq!(certs.len(), 2, "server.crt holds server cert then CA");
    assert_eq!(certs[1], ca);
    check_server(&certs[0], &ca);
    assert!(!has_name_constraints(&certs[0]));
    assert!(first.key_pem.starts_with("-----BEGIN PRIVATE KEY-----"));
    assert!(read(&dir, CA_KEY).starts_with("-----BEGIN PRIVATE KEY-----"));
    assert_eq!(first.cert_pem, read(&dir, SERVER_CRT));
    let meta = ServerMeta::parse(&read(&dir, SERVER_META)).unwrap();
    assert_eq!(meta.sans, first.sans);
    assert_eq!(meta.ca, first.ca_fingerprint);
    tls_acceptor(&first).unwrap();

    // 2. Same names (any order): nothing changes.
    let second = ensure_certificates(&dir, &names(&["mypc.local", "localhost", "127.0.0.1"])).unwrap();
    assert!(!second.ca_created && !second.renewed);
    assert_eq!(second.ca_fingerprint, first.ca_fingerprint);
    assert_eq!(second.cert_pem, first.cert_pem);
    assert_eq!(second.key_pem, first.key_pem);

    // 3. PKCS#1 keys load; the server key is rewritten as PKCS#8 without re-issuing.
    for name in [CA_KEY, SERVER_KEY] {
        let (key, format) = store::parse_private_key(&read(&dir, name)).unwrap();
        assert_eq!(format, KeyFormat::Pkcs8);
        std::fs::write(dir.join(name), key.to_pkcs1_pem(LineEnding::CRLF).unwrap().as_bytes()).unwrap();
    }
    let third = ensure_certificates(&dir, &second.sans).unwrap();
    assert!(!third.ca_created && !third.renewed);
    assert_eq!(third.ca_fingerprint, first.ca_fingerprint);
    assert_eq!(third.key_pem, first.key_pem);
    assert!(read(&dir, SERVER_KEY).starts_with("-----BEGIN PRIVATE KEY-----"));
    assert!(read(&dir, CA_KEY).starts_with("-----BEGIN RSA PRIVATE KEY-----"), "CA key is left as found");

    // 4. A SAN change re-issues the server certificate under the same CA.
    let fourth = ensure_certificates(&dir, &names(&["localhost", "10.1.2.3"])).unwrap();
    assert!(!fourth.ca_created && fourth.renewed);
    assert_eq!(fourth.ca_fingerprint, first.ca_fingerprint);
    assert_ne!(fourth.key_pem, first.key_pem);

    // 5. A missing server key forces re-issue.
    std::fs::remove_file(dir.join(SERVER_KEY)).unwrap();
    let fifth = ensure_certificates(&dir, &fourth.sans).unwrap();
    assert!(!fifth.ca_created && fifth.renewed);

    // 6. A CA without name constraints (here: the leaf) is replaced.
    std::fs::write(dir.join(CA_CRT), store::cert_to_pem(&chain(&dir)[0]).unwrap()).unwrap();
    let sixth = ensure_certificates(&dir, &fourth.sans).unwrap();
    assert!(sixth.ca_created && sixth.renewed);
    assert_ne!(sixth.ca_fingerprint, first.ca_fingerprint);
    check_ca(&Certificate::from_der(&sixth.ca_der).unwrap());
}

#[test]
fn refuses_when_no_name_is_permitted() {
    let temp = tempfile::tempdir().unwrap();
    let err = ensure_certificates(temp.path(), &names(&["8.8.8.8", "example.com"]));
    assert!(err.is_err());
    assert!(std::fs::read_dir(temp.path()).unwrap().next().is_none(), "nothing written");
}

#[test]
fn leaf_pem_takes_the_first_block() {
    let chain = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\nBBBB\n-----END CERTIFICATE-----\n";
    assert_eq!(crate::leaf_pem(chain).unwrap(), "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----");
    assert!(crate::leaf_pem("").is_err());
}

#[test]
fn fingerprint_matches_ios_format() {
    let fp = fingerprint(b"abc");
    assert_eq!(fp, "BA 78 16 BF 8F 01 CF EA 41 41 40 DE 5D AE 22 23 B0 03 61 A3 96 17 7A 9C B4 10 FF 61 F2 00 15 AD");
    assert_eq!(fingerprint(&[]).split(' ').count(), 32);
}

/// The CA installed on this machine, if any (read-only).
fn node_certs_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Xiro").join("certs");
    (dir.join(CA_CRT).is_file() && dir.join(CA_KEY).is_file()).then_some(dir)
}

#[test]
fn reuses_the_node_generated_ca() {
    let Some(node_dir) = node_certs_dir() else { return };
    let ca_pem = read(&node_dir, CA_CRT);
    let node_ca = store::parse_chain(&ca_pem).unwrap().remove(0);
    let usable = has_name_constraints(&node_ca)
        && crate::issue::not_after(&node_ca) > SystemTime::now() + duration_days(crate::meta::RENEW_BEFORE_DAYS);
    if !usable {
        return; // An expiring or unconstrained CA would legitimately be replaced.
    }
    // Copy only the CA into a temp dir; the real directory is never written.
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join(CA_CRT), &ca_pem).unwrap();
    std::fs::write(temp.path().join(CA_KEY), read(&node_dir, CA_KEY)).unwrap();
    let bundle = ensure_certificates(temp.path(), &names(&["localhost", "127.0.0.1"])).unwrap();
    assert!(!bundle.ca_created && bundle.renewed);
    assert_eq!(bundle.ca_fingerprint, fingerprint(&node_ca.to_der().unwrap()));
    if let Some(meta) = store::read_optional(&node_dir, SERVER_META).unwrap().and_then(|t| ServerMeta::parse(&t)) {
        assert_eq!(bundle.ca_fingerprint, meta.ca, "fingerprint must match what Node recorded");
    }
    let certs = chain(temp.path());
    assert_eq!(certs[1], node_ca);
    tls_acceptor(&bundle).unwrap();
}
