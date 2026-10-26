//! Key generation and certificate issuance for the local CA and the server.
//! Pure RustCrypto: the build has no C compiler, so ring / aws-lc-rs / OpenSSL are out.

use std::time::{Duration, SystemTime};

use const_oid::db::{rfc4519, rfc5280};
use der::Decode;
use der::referenced::OwnedToRef;
use der::asn1::{Any, Ia5String, OctetString, SetOfVec, Utf8StringRef};
use rand_core::{OsRng, RngCore};
use rsa::pkcs1v15::{Signature, SigningKey};
use rsa::{RsaPrivateKey, RsaPublicKey};
use sha2::Sha256;
use spki::SubjectPublicKeyInfoOwned;
use x509_cert::Certificate;
use x509_cert::attr::AttributeTypeAndValue;
use x509_cert::builder::{Builder, CertificateBuilder, Profile};
use x509_cert::ext::pkix::constraints::name::{GeneralSubtree, NameConstraints};
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::{
    AuthorityKeyIdentifier, BasicConstraints, ExtendedKeyUsage, KeyUsage, KeyUsages, SubjectAltName,
    SubjectKeyIdentifier,
};
use x509_cert::name::{Name, RdnSequence, RelativeDistinguishedName};
use x509_cert::serial_number::SerialNumber;
use x509_cert::time::{Time, Validity};

use crate::CertError;
use crate::names::{PERMITTED_DNS, PERMITTED_IP_RANGES, as_ipv4};

/// SChannel's PKCS#8 import handles RSA 2048 on every Windows version.
const RSA_BITS: usize = 2048;
const DAY: Duration = Duration::from_secs(86_400);
const CA_YEARS: u64 = 5;
const DAYS_PER_YEAR: u64 = 365;
/// iOS rejects server certificates valid for more than 825 days.
pub(crate) const SERVER_DAYS: u64 = 397;
/// First serial byte is fixed to 0x01 so the DER INTEGER stays positive.
const SERIAL_BYTES: usize = 16;
const SERIAL_LEAD_BYTE: u8 = 0x01;

const CA_COMMON_NAME: &str = "Xiro Local CA";
const CA_ORGANIZATION: &str = "Xiro (this PC only)";

pub(crate) struct Ca {
    pub cert: Certificate,
    pub key: RsaPrivateKey,
}

pub(crate) struct Issued {
    pub cert: Certificate,
    pub key: RsaPrivateKey,
}

pub(crate) fn duration_days(days: u64) -> Duration {
    DAY * u32::try_from(days).unwrap_or(u32::MAX)
}

fn new_key() -> Result<RsaPrivateKey, CertError> {
    RsaPrivateKey::new(&mut OsRng, RSA_BITS).map_err(|e| CertError::new(format!("RSA key generation failed: {e}")))
}

fn random_serial() -> Result<SerialNumber, CertError> {
    let mut bytes = [0u8; SERIAL_BYTES];
    OsRng
        .try_fill_bytes(&mut bytes[1..])
        .map_err(|e| CertError::new(format!("OS random number generator failed: {e}")))?;
    bytes[0] = SERIAL_LEAD_BYTE;
    Ok(SerialNumber::new(&bytes)?)
}

/// notBefore is back-dated a day so a slightly-behind iPad clock still accepts it.
fn validity(lifetime: Duration) -> Result<Validity, CertError> {
    let now = SystemTime::now();
    let start = now.checked_sub(DAY).unwrap_or(now);
    Ok(Validity { not_before: Time::try_from(start)?, not_after: Time::try_from(now + lifetime)? })
}

fn name(attributes: &[(const_oid::ObjectIdentifier, &str)]) -> Result<Name, CertError> {
    let mut rdns = Vec::with_capacity(attributes.len());
    for (oid, value) in attributes {
        let value = Any::from(Utf8StringRef::new(value)?);
        let set = SetOfVec::try_from(vec![AttributeTypeAndValue { oid: *oid, value }])?;
        rdns.push(RelativeDistinguishedName(set));
    }
    Ok(RdnSequence(rdns))
}

fn spki_of(key: &RsaPrivateKey) -> Result<SubjectPublicKeyInfoOwned, CertError> {
    Ok(SubjectPublicKeyInfoOwned::from_key(RsaPublicKey::from(key))?)
}

/// RFC 5280 method-1 key id (SHA-1 of the public key bits).
fn key_id(spki: &SubjectPublicKeyInfoOwned) -> Result<SubjectKeyIdentifier, CertError> {
    Ok(SubjectKeyIdentifier::try_from(spki.owned_to_ref())?)
}

/// Prefers the CA's SKI extension so the server AKI matches what the issuer advertises.
fn ca_key_id(ca: &Certificate) -> Result<OctetString, CertError> {
    let tbs = &ca.tbs_certificate;
    let declared = tbs.extensions.iter().flatten().find(|e| e.extn_id == rfc5280::ID_CE_SUBJECT_KEY_IDENTIFIER);
    match declared {
        Some(ext) => Ok(SubjectKeyIdentifier::from_der(ext.extn_value.as_bytes())?.0),
        None => Ok(key_id(&tbs.subject_public_key_info)?.0),
    }
}

/// Limits the CA to local names and private IPv4 so a stolen key can't impersonate public sites.
pub(crate) fn name_constraints() -> Result<NameConstraints, CertError> {
    let subtree = |base| GeneralSubtree { base, minimum: 0, maximum: None };
    let mut permitted = Vec::with_capacity(PERMITTED_DNS.len() + PERMITTED_IP_RANGES.len());
    for dns in PERMITTED_DNS {
        permitted.push(subtree(GeneralName::DnsName(Ia5String::new(dns)?)));
    }
    for (net, mask) in PERMITTED_IP_RANGES {
        let bytes: Vec<u8> = net.iter().chain(mask.iter()).copied().collect();
        permitted.push(subtree(GeneralName::IpAddress(OctetString::new(bytes)?)));
    }
    Ok(NameConstraints { permitted_subtrees: Some(permitted), excluded_subtrees: None })
}

fn sign(builder: CertificateBuilder<'_, SigningKey<Sha256>>) -> Result<Certificate, CertError> {
    // Randomised signing enables RSA blinding (RUSTSEC-2023-0071).
    builder
        .build_with_rng::<Signature>(&mut OsRng)
        .map_err(|e| CertError::new(format!("certificate signing failed: {e}")))
}

pub(crate) fn create_ca() -> Result<Ca, CertError> {
    let key = new_key()?;
    let spki = spki_of(&key)?;
    let subject = name(&[(rfc4519::CN, CA_COMMON_NAME), (rfc4519::O, CA_ORGANIZATION)])?;
    let signer = SigningKey::<Sha256>::new(key.clone());
    let lifetime = duration_days(CA_YEARS * DAYS_PER_YEAR);
    let ski = key_id(&spki)?;
    let mut builder = CertificateBuilder::new(
        Profile::Manual { issuer: None },
        random_serial()?,
        validity(lifetime)?,
        subject,
        spki,
        &signer,
    )?;
    builder.add_extension(&BasicConstraints { ca: true, path_len_constraint: None })?;
    builder.add_extension(&KeyUsage(KeyUsages::KeyCertSign | KeyUsages::CRLSign))?;
    builder.add_extension(&ski)?;
    builder.add_extension(&name_constraints()?)?;
    Ok(Ca { cert: sign(builder)?, key })
}

fn san_entry(name: &str) -> Result<GeneralName, CertError> {
    match as_ipv4(name) {
        Some(ip) => Ok(GeneralName::IpAddress(OctetString::new(ip.octets().to_vec())?)),
        None => Ok(GeneralName::DnsName(Ia5String::new(name)?)),
    }
}

/// `sans` must already be filtered by `is_permitted_name`; CN is the first SAN.
pub(crate) fn issue_server_cert(ca: &Ca, sans: &[String]) -> Result<Issued, CertError> {
    let first = sans.first().ok_or_else(|| CertError::new("no names to issue a certificate for"))?;
    let key = new_key()?;
    let spki = spki_of(&key)?;
    let signer = SigningKey::<Sha256>::new(ca.key.clone());
    let issuer = ca.cert.tbs_certificate.subject.clone();
    let mut builder = CertificateBuilder::new(
        Profile::Manual { issuer: Some(issuer) },
        random_serial()?,
        validity(duration_days(SERVER_DAYS))?,
        name(&[(rfc4519::CN, first)])?,
        spki,
        &signer,
    )?;
    let alt_names = sans.iter().map(|s| san_entry(s)).collect::<Result<Vec<_>, _>>()?;
    let aki = AuthorityKeyIdentifier { key_identifier: Some(ca_key_id(&ca.cert)?), ..Default::default() };
    builder.add_extension(&BasicConstraints { ca: false, path_len_constraint: None })?;
    builder.add_extension(&KeyUsage(KeyUsages::DigitalSignature | KeyUsages::KeyEncipherment))?;
    builder.add_extension(&ExtendedKeyUsage(vec![rfc5280::ID_KP_SERVER_AUTH]))?;
    builder.add_extension(&SubjectAltName(alt_names))?;
    builder.add_extension(&aki)?;
    Ok(Issued { cert: sign(builder)?, key })
}

pub(crate) fn has_name_constraints(cert: &Certificate) -> bool {
    cert.tbs_certificate.extensions.iter().flatten().any(|e| e.extn_id == rfc5280::ID_CE_NAME_CONSTRAINTS)
}

pub(crate) fn key_matches(cert: &Certificate, key: &RsaPrivateKey) -> bool {
    spki_of(key).is_ok_and(|spki| spki == cert.tbs_certificate.subject_public_key_info)
}

pub(crate) fn not_after(cert: &Certificate) -> SystemTime {
    cert.tbs_certificate.validity.not_after.to_system_time()
}
