//! Names the local CA may vouch for. One name outside the constraints makes iOS reject
//! the whole certificate, so stray names (e.g. from a VPN) are filtered out here.

use std::net::Ipv4Addr;

/// Each name and any subdomain of it.
pub const PERMITTED_DNS: [&str; 2] = ["local", "localhost"];

/// (network, mask) pairs.
pub const PERMITTED_IP_RANGES: [([u8; 4], [u8; 4]); 6] = [
    ([10, 0, 0, 0], [255, 0, 0, 0]),
    ([172, 16, 0, 0], [255, 240, 0, 0]),
    ([192, 168, 0, 0], [255, 255, 0, 0]),
    ([169, 254, 0, 0], [255, 255, 0, 0]),
    // CGNAT / Tailscale-style overlays.
    ([100, 64, 0, 0], [255, 192, 0, 0]),
    ([127, 0, 0, 0], [255, 0, 0, 0]),
];

const MAX_OCTET_DIGITS: usize = 3;

/// Strict dotted quad (leading zeros allowed). `None` if not IPv4-shaped,
/// `Some(Err(()))` if shaped like one but an octet is over 255.
fn parse_dotted_ipv4(name: &str) -> Option<Result<Ipv4Addr, ()>> {
    let parts: Vec<&str> = name.split('.').collect();
    let shaped = parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= MAX_OCTET_DIGITS && p.bytes().all(|b| b.is_ascii_digit()));
    if !shaped {
        return None;
    }
    let mut octets = [0u8; 4];
    for (slot, part) in octets.iter_mut().zip(&parts) {
        match part.parse::<u8>() {
            Ok(value) => *slot = value,
            Err(_) => return Some(Err(())),
        }
    }
    Some(Ok(Ipv4Addr::from(octets)))
}

pub(crate) fn as_ipv4(name: &str) -> Option<Ipv4Addr> {
    parse_dotted_ipv4(name).and_then(Result::ok)
}

fn ip_permitted(ip: Ipv4Addr) -> bool {
    let value = u32::from(ip);
    PERMITTED_IP_RANGES
        .iter()
        .any(|(net, mask)| value & u32::from_be_bytes(*mask) == u32::from_be_bytes(*net))
}

/// DNS names must also be printable ASCII, the only form a dNSName (IA5String) can take.
pub fn is_permitted_name(name: &str) -> bool {
    match parse_dotted_ipv4(name) {
        Some(Ok(ip)) => return ip_permitted(ip),
        Some(Err(())) => return false,
        None => {}
    }
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_graphic()) {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    PERMITTED_DNS.iter().any(|d| {
        lower == *d || (lower.len() > d.len() && lower.ends_with(d) && lower[..lower.len() - d.len()].ends_with('.'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_private_addresses_and_local_names() {
        for name in [
            "192.168.1.5",
            "100.101.1.2",
            "10.0.0.1",
            "172.31.255.255",
            "169.254.3.4",
            "127.0.0.1",
            "Desk-PC.local",
            "localhost",
            "LOCAL",
            "a.b.localhost",
        ] {
            assert!(is_permitted_name(name), "{name} should be permitted");
        }
    }

    #[test]
    fn rejects_public_and_malformed_names() {
        for name in [
            "8.8.8.8",
            "172.32.0.1",
            "100.128.0.1",
            "bank.example.com",
            "notlocal",
            "evil-local",
            "999.168.1.1",
            "192.168.1.300",
            "",
            "my pc.local",
            "caf\u{e9}.local",
        ] {
            assert!(!is_permitted_name(name), "{name:?} should be rejected");
        }
    }

    #[test]
    fn recognises_ipv4_literals() {
        assert_eq!(as_ipv4("192.168.001.030"), Some(Ipv4Addr::new(192, 168, 1, 30)));
        assert_eq!(as_ipv4("mypc.local"), None);
        assert_eq!(as_ipv4("1.2.3"), None);
        assert_eq!(as_ipv4("1.2.3.4.5"), None);
    }
}
