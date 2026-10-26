//! Setting up the Virtual Display Driver, for the installer: its settings file,
//! and the root-enumerated device the driver binds to (what `devcon install` does).

use std::path::Path;

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    DICD_GENERATE_ID, DICS_FLAG_CONFIGSPECIFIC, DICS_FLAG_GLOBAL, DICS_PROPCHANGE, DIF_PROPERTYCHANGE, DIF_REGISTERDEVICE,
    DIF_REMOVE, DIGCF_ALLCLASSES, DIGCF_PRESENT, DIREG_DRV, HDEVINFO, INSTALLFLAG_FORCE, MAX_CLASS_NAME_LEN,
    SP_CLASSINSTALL_HEADER, SP_DEVINFO_DATA, SP_PROPCHANGE_PARAMS, SPDRP_HARDWAREID, SUOI_FORCEDELETE,
    SetupDiCallClassInstaller, SetupDiCreateDeviceInfoList, SetupDiCreateDeviceInfoW, SetupDiDestroyDeviceInfoList,
    SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceRegistryPropertyW, SetupDiGetINFClassW,
    SetupDiOpenDevRegKey, SetupDiSetClassInstallParamsW, SetupDiSetDeviceRegistryPropertyW, SetupUninstallOEMInfW,
    UpdateDriverForPlugAndPlayDevicesW,
};
use windows::Win32::System::Registry::{KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegGetValueW};
use windows::Win32::Foundation::ERROR_NO_MORE_ITEMS;
use windows::core::{GUID, HSTRING, PCWSTR, w};

use crate::error::{DisplayError, Result};
use crate::Size;
use crate::vdd::settings_for;

/// The upstream default settings the driver ships with.
const DEFAULT_SETTINGS: &str = include_str!("../../../vendor/vdd/vdd_settings.xml");
/// Hardware id in MttVDD.inf; the device created here must carry exactly this.
const HARDWARE_ID: &str = r"Root\MttVDD";
/// SetupAPI enumerator for root-enumerated (software) devices.
const ROOT_ENUMERATOR: PCWSTR = w!("ROOT");
/// Hardware ids are a REG_MULTI_SZ; a few hundred bytes is plenty for one device.
const HARDWARE_ID_BUFFER: usize = 1024;
/// Driver-store names are short (`oem123.inf`).
const INF_NAME_CHARS: usize = 64;

/// vdd_settings.xml offering exactly these sizes (in order, up to the driver's limit) at 60 Hz.
pub fn settings_with(sizes: &[Size]) -> String {
    // The embedded default always has both lists (checked by a test).
    settings_for(DEFAULT_SETTINGS, sizes).unwrap_or_else(|_| DEFAULT_SETTINGS.to_owned())
}

/// Destroys the device information set on drop.
struct DeviceSet(HDEVINFO);

impl Drop for DeviceSet {
    fn drop(&mut self) {
        // SAFETY: the handle came from SetupDi*, is valid and closed only here.
        let _ = unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

fn os_error(what: &str, err: &windows::core::Error) -> DisplayError {
    DisplayError::new(format!("{what} failed: {}", err.message()))
}

fn device_info() -> SP_DEVINFO_DATA {
    SP_DEVINFO_DATA { cbSize: u32::try_from(size_of::<SP_DEVINFO_DATA>()).unwrap_or(u32::MAX), ..Default::default() }
}

/// Parses a REG_MULTI_SZ (UTF-16LE bytes) into its strings.
fn multi_sz(bytes: &[u8]) -> Vec<String> {
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    units.split(|&u| u == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
}

/// A REG_MULTI_SZ holding one string: the string, its terminator, the list terminator.
fn to_multi_sz(value: &str) -> Vec<u8> {
    value.encode_utf16().chain([0, 0]).flat_map(u16::to_le_bytes).collect()
}

fn has_hardware_id(ids: &[String]) -> bool {
    ids.iter().any(|id| id.eq_ignore_ascii_case(HARDWARE_ID))
}

/// Calls `found` with the device `Root\MttVDD` (if present) and the set it belongs to.
fn with_device<T>(found: impl FnOnce(&DeviceSet, &SP_DEVINFO_DATA) -> Result<T>) -> Result<Option<T>> {
    // SAFETY: no pointers besides the static enumerator string.
    let set = unsafe { SetupDiGetClassDevsW(None, ROOT_ENUMERATOR, None, DIGCF_ALLCLASSES | DIGCF_PRESENT) }
        .map_err(|e| os_error("Listing devices", &e))?;
    let set = DeviceSet(set);
    for index in 0.. {
        let mut info = device_info();
        // SAFETY: `info` is sized via cbSize and outlives the call.
        match unsafe { SetupDiEnumDeviceInfo(set.0, index, &mut info) } {
            Ok(()) => {}
            Err(e) if e.code() == ERROR_NO_MORE_ITEMS.to_hresult() => break,
            Err(e) => return Err(os_error("Listing devices", &e)),
        }
        let mut buf = [0u8; HARDWARE_ID_BUFFER];
        // SAFETY: `buf` is a valid slice; `info` was filled by SetupDiEnumDeviceInfo.
        let read = unsafe { SetupDiGetDeviceRegistryPropertyW(set.0, &info, SPDRP_HARDWAREID, None, Some(&mut buf), None) };
        // No hardware id (some root devices) or an implausibly long one: not ours.
        if read.is_ok() && has_hardware_id(&multi_sz(&buf)) {
            return found(&set, &info).map(Some);
        }
    }
    Ok(None)
}

/// Whether a `Root\MttVDD` device is present; needs no admin.
pub fn driver_installed() -> Result<bool> {
    Ok(with_device(|_, _| Ok(()))?.is_some())
}

/// Admin only: restarts the driver's device so it rereads its settings (also fixes a missing monitor).
/// False when the driver is not installed.
pub fn restart_driver() -> Result<bool> {
    let restarted = with_device(|set, info| {
        let params = SP_PROPCHANGE_PARAMS {
            ClassInstallHeader: SP_CLASSINSTALL_HEADER {
                cbSize: size_of::<SP_CLASSINSTALL_HEADER>() as u32,
                InstallFunction: DIF_PROPERTYCHANGE,
            },
            StateChange: DICS_PROPCHANGE,
            Scope: DICS_FLAG_CONFIGSPECIFIC,
            HwProfile: 0,
        };
        let size = size_of::<SP_PROPCHANGE_PARAMS>() as u32;
        // SAFETY: `params` starts with its class-install header and outlives both calls.
        unsafe {
            let whole = (&params as *const SP_PROPCHANGE_PARAMS).cast::<SP_CLASSINSTALL_HEADER>();
            SetupDiSetClassInstallParamsW(set.0, Some(info), Some(whole), size)
                .and_then(|()| SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, set.0, Some(info)))
                .map_err(|e| os_error("Restarting the Virtual Display Driver", &e))
        }
    })?;
    Ok(restarted.is_some())
}

/// The driver package's name in the driver store (e.g. `oem21.inf`), from the device's driver key.
fn store_inf_name(set: &DeviceSet, info: &SP_DEVINFO_DATA) -> Option<String> {
    // SAFETY: opens the device's own driver key for reading; closed below.
    let key = unsafe { SetupDiOpenDevRegKey(set.0, info, DICS_FLAG_GLOBAL.0, 0, DIREG_DRV, KEY_READ.0) }.ok()?;
    let mut buf = [0u16; INF_NAME_CHARS];
    let mut bytes = (INF_NAME_CHARS * 2) as u32;
    // SAFETY: `buf` holds `bytes` bytes and outlives the call.
    let rc = unsafe { RegGetValueW(key, None, w!("InfPath"), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut bytes)) };
    // SAFETY: the key was opened above and is closed once.
    let _ = unsafe { RegCloseKey(key) };
    if rc.is_err() {
        return None;
    }
    let text = String::from_utf16_lossy(&buf);
    let name = text.split('\0').next().unwrap_or_default();
    is_store_inf_name(name).then(|| name.to_owned())
}

/// Only a bare driver-store name like `oem21.inf`, never a path.
fn is_store_inf_name(name: &str) -> bool {
    name.len() > ".inf".len() && name.to_ascii_lowercase().ends_with(".inf") && !name.contains(['\\', '/', ':'])
}

/// Admin only: removes the driver's device and its driver-store package. False when absent.
pub fn remove_driver() -> Result<bool> {
    let found = with_device(|set, info| {
        let inf = store_inf_name(set, info);
        // SAFETY: `info` belongs to `set`.
        unsafe { SetupDiCallClassInstaller(DIF_REMOVE, set.0, Some(info)) }
            .map_err(|e| os_error("Removing the Virtual Display Driver", &e))?;
        Ok(inf)
    })?;
    let Some(inf) = found else { return Ok(false) };
    let Some(inf) = inf else {
        return Err(DisplayError::new("The display driver was removed, but its package stays in the driver store (its name could not be read)"));
    };
    // SAFETY: a bare driver-store name; the device that used it is gone.
    let removed = unsafe { SetupUninstallOEMInfW(&HSTRING::from(inf.as_str()), SUOI_FORCEDELETE, None) };
    if !removed.as_bool() {
        let why = windows::core::Error::from_thread();
        return Err(DisplayError::new(format!("The driver package {inf} could not be removed from the driver store: {}", why.message())));
    }
    Ok(true)
}

/// Admin only: creates `Root\MttVDD` if missing and installs the INF on it, like `devcon install`.
pub fn install_driver(inf_path: &Path) -> Result<()> {
    let inf = HSTRING::from(inf_path.as_os_str());
    let hardware_id = HSTRING::from(HARDWARE_ID);
    if !driver_installed()? {
        create_device(&inf)?;
    }
    // INSTALLFLAG_FORCE, as devcon: install even if Windows prefers an already bound driver.
    // SAFETY: both strings outlive the call.
    let update = unsafe { UpdateDriverForPlugAndPlayDevicesW(None, &hardware_id, &inf, INSTALLFLAG_FORCE, None) };
    update.map_err(|e| os_error("Installing the Virtual Display Driver", &e))
}

/// Registers a new device node for the INF's class with our hardware id.
fn create_device(inf: &HSTRING) -> Result<()> {
    let mut class = GUID::zeroed();
    let mut class_name = [0u16; MAX_CLASS_NAME_LEN as usize];
    // SAFETY: out-buffers are valid for the call.
    unsafe { SetupDiGetINFClassW(inf, &mut class, &mut class_name, None) }
        .map_err(|e| os_error("Reading the driver INF", &e))?;
    // SAFETY: `class` outlives the call.
    let set = unsafe { SetupDiCreateDeviceInfoList(Some(&class), None) }.map_err(|e| os_error("Creating the device", &e))?;
    let set = DeviceSet(set);
    let mut info = device_info();
    // SAFETY: all pointers are valid for the call.
    unsafe {
        SetupDiCreateDeviceInfoW(set.0, PCWSTR(class_name.as_ptr()), &class, PCWSTR::null(), None, DICD_GENERATE_ID, Some(&mut info))
    }
    .map_err(|e| os_error("Creating the device", &e))?;
    let ids = to_multi_sz(HARDWARE_ID);
    // SAFETY: `info` belongs to `set`; `ids` is a valid slice.
    unsafe { SetupDiSetDeviceRegistryPropertyW(set.0, &mut info, SPDRP_HARDWAREID, Some(&ids)) }
        .map_err(|e| os_error("Setting the device hardware id", &e))?;
    // SAFETY: as above.
    unsafe { SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set.0, Some(&info)) }
        .map_err(|e| os_error("Registering the device", &e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_offer_only_the_given_sizes_with_the_upstream_options() {
        let sizes = [Size { width: 2360, height: 1640 }, Size { width: 1640, height: 2360 }];
        let xml = settings_with(&sizes);
        assert_eq!(xml.matches("<resolution>").count(), 2, "the upstream sizes are gone");
        assert_eq!(xml.matches("<g_refresh_rate>").count(), 1, "60 Hz only");
        assert!(xml.contains("<HardwareCursor>true</HardwareCursor>"), "upstream options are kept");
        assert!(xml.starts_with("<?xml"));
        assert!(crate::vdd::settings_match(&xml, &sizes));
    }

    #[test]
    fn only_bare_driver_store_names_are_removed() {
        assert!(is_store_inf_name("oem21.inf"));
        assert!(!is_store_inf_name(r"C:\Windows\INF\oem21.inf"));
        assert!(!is_store_inf_name(r"..\x.inf"));
        assert!(!is_store_inf_name(".inf"));
        assert!(!is_store_inf_name("oem21.sys"));
    }

    #[test]
    fn hardware_ids_round_trip_as_multi_sz() {
        let bytes = to_multi_sz(HARDWARE_ID);
        assert_eq!(bytes.len(), (HARDWARE_ID.len() + 2) * 2);
        assert_eq!(multi_sz(&bytes), vec![HARDWARE_ID.to_owned()]);
        assert!(has_hardware_id(&[r"ROOT\mttvdd".to_owned()]));
        assert!(!has_hardware_id(&[r"Root\Other".to_owned()]));
    }
}
