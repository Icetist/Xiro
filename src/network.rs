//! LAN address discovery. Interfaces are ranked: private LANs, then tethering/hotspot ranges,
//! then the rest, link-local (169.254.x) last.

use std::net::Ipv4Addr;

use windows::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};
use windows::Win32::System::SystemInformation::{ComputerNameDnsHostname, GetComputerNameExW};
use windows::core::PWSTR;

const VIRTUAL_ADAPTERS: [&str; 8] = ["vethernet", "virtualbox", "vmware", "hyper-v", "wsl", "loopback", "npcap", "docker"];
const ADDRESS_BUFFER_BYTES: u32 = 64 * 1024;

/// Windows hosts Xiro Wi-Fi (and Mobile hotspot) on this kind of adapter.
const HOSTED_ADAPTER: &str = "wi-fi direct virtual adapter";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanAddress {
    pub address: Ipv4Addr,
    pub name: String,
    /// On a network this PC hosts itself (Xiro Wi-Fi or Mobile hotspot).
    pub hosted: bool,
}

pub fn is_hosted_adapter(description: &str) -> bool {
    description.to_lowercase().contains(HOSTED_ADAPTER)
}

/// Lower is better.
pub fn rank(ip: Ipv4Addr, interface: &str) -> u32 {
    let lower = interface.to_lowercase();
    let [a, b, c, _] = ip.octets();
    if VIRTUAL_ADAPTERS.iter().any(|v| lower.contains(v)) {
        50
    } else if a == 192 && b == 168 {
        10
    } else if a == 172 && b == 20 && c == 10 {
        // iPad USB "Personal Hotspot" hands out 172.20.10.x.
        12
    } else if a == 10 {
        15
    } else if a == 172 && (16..=31).contains(&b) {
        20
    } else if a == 169 && b == 254 {
        90
    } else {
        40
    }
}

/// Sorts best first (stable, so the system's order breaks ties).
pub fn ranked(mut found: Vec<LanAddress>) -> Vec<LanAddress> {
    found.sort_by_key(|a| rank(a.address, &a.name));
    found
}

/// IPv4 addresses of every connected network interface, best first.
pub fn lan_addresses() -> Vec<LanAddress> {
    let mut found = Vec::new();
    let mut size = ADDRESS_BUFFER_BYTES;
    let mut buf = vec![0u8; size as usize];
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let head = buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    if unsafe { GetAdaptersAddresses(AF_INET.0 as u32, flags, None, Some(head), &mut size) } != 0 {
        return found;
    }
    let mut adapter = head as *const IP_ADAPTER_ADDRESSES_LH;
    // SAFETY: GetAdaptersAddresses filled `buf` with a linked list that lives as long as `buf`.
    unsafe {
        while let Some(a) = adapter.as_ref() {
            if a.OperStatus == IfOperStatusUp {
                let name = a.FriendlyName.to_string().unwrap_or_default();
                let hosted = is_hosted_adapter(&a.Description.to_string().unwrap_or_default());
                let mut unicast = a.FirstUnicastAddress;
                while let Some(u) = unicast.as_ref() {
                    let sockaddr = u.Address.lpSockaddr;
                    if !sockaddr.is_null() && (*sockaddr).sa_family == AF_INET {
                        let v4 = &*(sockaddr as *const SOCKADDR_IN);
                        let address = Ipv4Addr::from(u32::from_be(v4.sin_addr.S_un.S_addr));
                        if !address.is_loopback() {
                            found.push(LanAddress { address, name: name.clone(), hosted });
                        }
                    }
                    unicast = u.Next;
                }
            }
            adapter = a.Next;
        }
    }
    drop(buf);
    ranked(found)
}

/// mDNS name Windows answers for itself, e.g. "MYPC.local".
pub fn local_hostname() -> String {
    let mut len = 0u32;
    unsafe {
        let _ = GetComputerNameExW(ComputerNameDnsHostname, None, &mut len);
        let mut name = vec![0u16; len as usize + 1];
        if GetComputerNameExW(ComputerNameDnsHostname, Some(PWSTR(name.as_mut_ptr())), &mut len).is_ok() {
            return format!("{}.local", String::from_utf16_lossy(&name[..len as usize]));
        }
    }
    std::env::var("COMPUTERNAME").map_or_else(|_| "localhost".into(), |n| format!("{n}.local"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(ip: &str, name: &str) -> LanAddress {
        LanAddress { address: ip.parse().unwrap(), name: name.into(), hosted: false }
    }

    #[test]
    fn recognises_the_adapter_windows_hosts_networks_on() {
        assert!(is_hosted_adapter("Microsoft Wi-Fi Direct Virtual Adapter #4"));
        assert!(!is_hosted_adapter("Broadcom 802.11n Network Adapter"));
    }

    #[test]
    fn real_lans_beat_tethering_beats_others() {
        assert!(rank("192.168.0.2".parse().unwrap(), "Wi-Fi") < rank("172.20.10.3".parse().unwrap(), "Ethernet 3"));
        assert!(rank("172.20.10.3".parse().unwrap(), "x") < rank("10.0.0.5".parse().unwrap(), "x"));
        assert!(rank("10.0.0.5".parse().unwrap(), "x") < rank("172.17.0.1".parse().unwrap(), "x"));
        assert_eq!(rank("192.168.56.1".parse().unwrap(), "VirtualBox Host-Only"), 50);
        assert_eq!(rank("169.254.3.4".parse().unwrap(), "x"), 90);
        assert_eq!(rank("8.8.8.8".parse().unwrap(), "x"), 40);
    }

    #[test]
    fn ranking_keeps_system_order_for_ties() {
        let list = ranked(vec![addr("172.28.1.1", "vEthernet (WSL)"), addr("192.168.1.5", "Wi-Fi"), addr("192.168.137.1", "Local Area Connection* 2")]);
        let names: Vec<_> = list.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["Wi-Fi", "Local Area Connection* 2", "vEthernet (WSL)"]);
    }

    #[test]
    fn finds_this_machine() {
        assert!(local_hostname().ends_with(".local"));
        assert!(lan_addresses().iter().all(|a| !a.address.is_loopback()));
    }
}
