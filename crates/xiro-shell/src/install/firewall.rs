//! The inbound firewall rule via the Windows Firewall COM API, scoped to LocalSubnet on
//! every profile (a phone hotspot is usually classed Public).

use std::path::Path;

use windows::Win32::Foundation::VARIANT_TRUE;
use windows::Win32::NetworkManagement::WindowsFirewall::{
    INetFwPolicy2, INetFwRule, INetFwRules, NET_FW_ACTION_ALLOW, NET_FW_IP_PROTOCOL_TCP, NET_FW_PROFILE2_ALL,
    NET_FW_RULE_DIR_IN, NetFwPolicy2, NetFwRule,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::BSTR;

use super::com::ComScope;

const RULE_NAME: &str = "Xiro";
const DESCRIPTION: &str = "Lets your iPad or iPhone on the same local network connect to Xiro.";
const REMOTE_ADDRESSES: &str = "LocalSubnet";
/// Bound on the removal loop, in case Remove ever reports success without removing.
const MAX_DUPLICATES: usize = 32;

fn fail(what: &'static str) -> impl Fn(windows::core::Error) -> String {
    move |e| format!("{what}: {}", e.message())
}

fn rules() -> Result<INetFwRules, String> {
    // SAFETY: COM is initialised by the caller's ComScope.
    unsafe {
        let policy: INetFwPolicy2 =
            CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER).map_err(fail("Windows Firewall is unavailable"))?;
        policy.Rules().map_err(fail("could not read the firewall rules"))
    }
}

/// Removes every rule named "Xiro" (Remove deletes one match per call).
fn remove_all(rules: &INetFwRules) -> Result<(), String> {
    let name = BSTR::from(RULE_NAME);
    for _ in 0..MAX_DUPLICATES {
        // SAFETY: `rules` is a live collection; Item fails once no rule has this name.
        if unsafe { rules.Item(&name) }.is_err() {
            return Ok(());
        }
        // SAFETY: as above.
        unsafe { rules.Remove(&name) }.map_err(fail("could not remove the old firewall rule (run as administrator)"))?;
    }
    Ok(())
}

/// `ports`: the only TCP ports opened (Xiro's HTTP and HTTPS).
pub(crate) fn add(exe: &Path, ports: &[u16]) -> Result<(), String> {
    if ports.is_empty() {
        return Err("no ports to open".into());
    }
    let ports = port_list(ports);
    let _com = ComScope::enter()?;
    let rules = rules()?;
    remove_all(&rules)?;
    let set = fail("could not configure the firewall rule");
    // SAFETY: COM objects created and used within this scope; BSTRs outlive each call.
    unsafe {
        let rule: INetFwRule = CoCreateInstance(&NetFwRule, None, CLSCTX_INPROC_SERVER).map_err(&set)?;
        rule.SetName(&BSTR::from(RULE_NAME)).map_err(&set)?;
        rule.SetDescription(&BSTR::from(DESCRIPTION)).map_err(&set)?;
        rule.SetApplicationName(&BSTR::from(exe.display().to_string())).map_err(&set)?;
        rule.SetProtocol(NET_FW_IP_PROTOCOL_TCP.0).map_err(&set)?;
        rule.SetLocalPorts(&BSTR::from(ports.as_str())).map_err(&set)?;
        rule.SetDirection(NET_FW_RULE_DIR_IN).map_err(&set)?;
        rule.SetRemoteAddresses(&BSTR::from(REMOTE_ADDRESSES)).map_err(&set)?;
        rule.SetProfiles(NET_FW_PROFILE2_ALL.0).map_err(&set)?;
        rule.SetAction(NET_FW_ACTION_ALLOW).map_err(&set)?;
        rule.SetEnabled(VARIANT_TRUE).map_err(&set)?;
        rules.Add(&rule).map_err(fail("could not add the firewall rule (run as administrator)"))
    }
}

/// The "Xiro" rule's program and ports: `Some(None)` if absent, `None` if unreadable. No admin needed.
pub(crate) fn program() -> Option<Option<(String, String)>> {
    let _com = ComScope::enter().ok()?;
    let rules = rules().ok()?;
    // SAFETY: `rules` is a live collection; Item fails when no rule has this name.
    let Ok(rule) = (unsafe { rules.Item(&BSTR::from(RULE_NAME)) }) else { return Some(None) };
    // SAFETY: reading properties of the rule just looked up.
    let (program, ports) = unsafe { (rule.ApplicationName().ok()?, rule.LocalPorts().ok()?) };
    Some(Some((program.to_string(), ports.to_string())))
}

/// The ports as the firewall writes them back ("8080,8443").
pub(crate) fn port_list(ports: &[u16]) -> String {
    ports.iter().map(u16::to_string).collect::<Vec<_>>().join(",")
}

pub(crate) fn remove() -> Result<(), String> {
    let _com = ComScope::enter()?;
    remove_all(&rules()?)
}
