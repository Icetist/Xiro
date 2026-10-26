//! `server.json` and the decision of when to re-issue the server certificate.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use der::DateTime;
use serde::{Deserialize, Serialize};

use crate::CertError;

/// Renew early so a device offline for a while never meets an expired certificate.
pub(crate) const RENEW_BEFORE_DAYS: u64 = 30;

/// What was issued last time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ServerMeta {
    pub sans: Vec<String>,
    /// Fingerprint of the CA that signed the server certificate.
    pub ca: String,
    /// ISO 8601 in `Date.toISOString()` form.
    #[serde(rename = "notAfter")]
    pub not_after: String,
}

impl ServerMeta {
    /// A malformed file yields `None`, which forces a renewal.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }

    pub(crate) fn to_json(&self) -> Result<String, CertError> {
        serde_json::to_string_pretty(self).map_err(|e| CertError::new(format!("cannot encode server.json: {e}")))
    }
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ`, same shape as `Date.toISOString()`.
pub(crate) fn format_iso(time: SystemTime) -> Result<String, CertError> {
    let dt = DateTime::from_system_time(time)?;
    let millis = time.duration_since(UNIX_EPOCH).map(|d| d.subsec_millis()).unwrap_or(0);
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        dt.year(),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minutes(),
        dt.seconds()
    ))
}

fn number<T: std::str::FromStr>(text: &str, range: std::ops::Range<usize>) -> Option<T> {
    let part = text.get(range)?;
    if !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

/// Length of `YYYY-MM-DDTHH:MM:SSZ`, the shortest form accepted.
const MIN_ISO_LEN: usize = 20;

/// Accepts UTC `...SSZ` or `...SS.fffZ`.
pub(crate) fn parse_iso(text: &str) -> Option<SystemTime> {
    let bytes = text.as_bytes();
    let separators_ok = bytes.get(4) == Some(&b'-')
        && bytes.get(7) == Some(&b'-')
        && bytes.get(10) == Some(&b'T')
        && bytes.get(13) == Some(&b':')
        && bytes.get(16) == Some(&b':');
    if !text.is_ascii() || text.len() < MIN_ISO_LEN || !separators_ok || !text.ends_with('Z') {
        return None;
    }
    let rest = &text[19..text.len() - 1];
    let millis = match rest.strip_prefix('.') {
        None if rest.is_empty() => 0,
        Some(frac) if !frac.is_empty() && frac.bytes().all(|b| b.is_ascii_digit()) => {
            let padded = format!("{frac:0<3}");
            padded.get(..3)?.parse::<u64>().ok()?
        }
        _ => return None,
    };
    let dt = DateTime::new(
        number(text, 0..4)?,
        number(text, 5..7)?,
        number(text, 8..10)?,
        number(text, 11..13)?,
        number(text, 14..16)?,
        number(text, 17..19)?,
    )
    .ok()?;
    Some(dt.to_system_time() + Duration::from_millis(millis))
}

pub(crate) struct RenewalCheck<'a> {
    pub ca_created: bool,
    pub meta: Option<&'a ServerMeta>,
    pub ca_fingerprint: &'a str,
    pub sans: &'a [String],
    /// server.key and server.crt exist, parse, and belong together.
    pub server_files_ok: bool,
    pub now: SystemTime,
}

fn same_set(a: &[String], b: &[String]) -> bool {
    let mut a = a.to_vec();
    let mut b = b.to_vec();
    a.sort();
    b.sort();
    a == b
}

pub(crate) fn needs_renewal(check: &RenewalCheck<'_>) -> bool {
    let Some(meta) = check.meta else { return true };
    let renew_by = check.now + crate::issue::duration_days(RENEW_BEFORE_DAYS);
    let expires_soon = parse_iso(&meta.not_after).is_none_or(|not_after| not_after < renew_by);
    check.ca_created
        || expires_soon
        || meta.ca != check.ca_fingerprint
        || !same_set(&meta.sans, check.sans)
        || !check.server_files_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "AA BB";

    fn sans(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn meta(days_left: u64) -> ServerMeta {
        let not_after = SystemTime::now() + crate::issue::duration_days(days_left);
        ServerMeta { sans: sans(&["localhost", "10.0.0.2"]), ca: FP.to_owned(), not_after: format_iso(not_after).unwrap() }
    }

    fn check<'a>(meta: Option<&'a ServerMeta>, requested: &'a [String]) -> RenewalCheck<'a> {
        RenewalCheck { ca_created: false, meta, ca_fingerprint: FP, sans: requested, server_files_ok: true, now: SystemTime::now() }
    }

    #[test]
    fn keeps_a_current_certificate_regardless_of_san_order() {
        let m = meta(200);
        let requested = sans(&["10.0.0.2", "localhost"]);
        assert!(!needs_renewal(&check(Some(&m), &requested)));
    }

    #[test]
    fn renews_on_each_trigger() {
        let m = meta(200);
        let same = sans(&["localhost", "10.0.0.2"]);
        assert!(needs_renewal(&check(None, &same)), "no metadata");
        assert!(needs_renewal(&check(Some(&meta(29)), &same)), "expiring within 30 days");
        assert!(needs_renewal(&check(Some(&m), &sans(&["localhost", "10.0.0.3"]))), "SAN change");
        assert!(needs_renewal(&check(Some(&m), &sans(&["localhost"]))), "SAN removed");
        assert!(needs_renewal(&RenewalCheck { ca_fingerprint: "CC DD", ..check(Some(&m), &same) }), "CA change");
        assert!(needs_renewal(&RenewalCheck { ca_created: true, ..check(Some(&m), &same) }), "CA created");
        assert!(needs_renewal(&RenewalCheck { server_files_ok: false, ..check(Some(&m), &same) }), "key missing");
        let garbled = ServerMeta { not_after: "soon".to_owned(), ..m.clone() };
        assert!(needs_renewal(&check(Some(&garbled), &same)), "unparseable date");
    }

    #[test]
    fn iso_round_trips_and_reads_node_output() {
        let parsed = parse_iso("2027-11-25T17:38:28.360Z").unwrap();
        assert_eq!(format_iso(parsed).unwrap(), "2027-11-25T17:38:28.360Z");
        assert_eq!(format_iso(parse_iso("2030-01-02T03:04:05Z").unwrap()).unwrap(), "2030-01-02T03:04:05.000Z");
        for bad in ["", "2027-11-25", "2027-11-25T17:38:28", "2027-13-25T17:38:28Z", "2027-11-25T17:38:28.Z"] {
            assert!(parse_iso(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn reads_node_server_json() {
        let text = r#"{ "sans": ["TheBigLeague.local", "localhost"], "ca": "18 97", "notAfter": "2027-11-25T17:38:28.360Z" }"#;
        let parsed = ServerMeta::parse(text).unwrap();
        assert_eq!(parsed.sans, sans(&["TheBigLeague.local", "localhost"]));
        assert!(parsed.to_json().unwrap().contains("\"notAfter\": \"2027-11-25T17:38:28.360Z\""));
        assert!(ServerMeta::parse("{not json").is_none());
    }
}
