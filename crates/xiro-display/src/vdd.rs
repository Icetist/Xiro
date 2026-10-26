//! Virtual Display Driver resolutions (vdd_settings.xml). Its command pipe is never used: the
//! driver crashes on RELOAD_DRIVER, so a device restart makes it re-read its settings.

use std::fs;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::core::w;

use crate::{Size, fits_driver};
use crate::error::{DisplayError, Result};

const DEFAULT_DIR: &str = r"C:\VirtualDisplayDriver";
const SETTINGS_FILE: &str = "vdd_settings.xml";
/// Monitor hardware id the driver reports; identifies its monitor among all others.
pub(crate) const VDD_MONITOR_ID: &str = "mtt1337";
/// Windows refuses the driver's monitor with too many modes (55 worked, 56 failed); one mode per size.
pub const MAX_SIZES: usize = 40;
const RATE_HZ: u32 = 60;

/// Matches the driver's monitor device path, case-insensitively.
pub(crate) fn is_vdd_monitor(key: &str) -> bool {
    key.to_ascii_lowercase().contains(VDD_MONITOR_ID)
}

/// Folder holding vdd_settings.xml (the driver allows relocating it via the registry).
/// Read with the registry API rather than reg.exe: no child process, nothing to hijack.
pub(crate) fn settings_dir() -> PathBuf {
    registry_settings_dir()
        .filter(|dir| Path::new(dir).is_absolute())
        .map_or_else(|| PathBuf::from(DEFAULT_DIR), PathBuf::from)
}

fn registry_settings_dir() -> Option<String> {
    let key = w!(r"SOFTWARE\MikeTheTech\VirtualDisplayDriver");
    let value = w!("VDDPATH");
    let mut bytes = 0u32;
    // SAFETY: size query with no buffer; `bytes` outlives the call.
    let rc = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key, value, RRF_RT_REG_SZ, None, None, Some(&mut bytes)) };
    if rc != ERROR_SUCCESS || bytes == 0 {
        return None; // not relocated
    }
    let mut buf = vec![0u16; (bytes as usize).div_ceil(2)];
    // SAFETY: `buf` holds `bytes` bytes and outlives the call.
    let rc = unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, key, value, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut bytes))
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    let text = String::from_utf16_lossy(&buf);
    let dir = text.split('\0').next().unwrap_or_default().trim();
    (!dir.is_empty()).then(|| dir.to_owned())
}

pub(crate) fn settings_file(dir: &Path) -> PathBuf {
    dir.join(SETTINGS_FILE)
}

/// Every `<resolution>` width/height pair, tolerating whitespace like the driver's parser.
fn present_resolutions(xml: &str) -> Vec<(u32, u32)> {
    const OPEN: &str = "<resolution>";
    let mut found = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find(OPEN) {
        rest = &rest[at + OPEN.len()..];
        if let Some(size) = parse_width_height(rest) {
            found.push(size);
        }
    }
    found
}

fn parse_width_height(s: &str) -> Option<(u32, u32)> {
    let s = expect(s, "<width>")?;
    let (width, s) = number(s)?;
    let s = expect(s, "</width>")?;
    let s = expect(s, "<height>")?;
    let (height, s) = number(s)?;
    expect(s, "</height>")?;
    Some((width, height))
}

/// Skips leading whitespace, then the literal tag.
fn expect<'a>(s: &'a str, tag: &str) -> Option<&'a str> {
    s.trim_start().strip_prefix(tag)
}

fn number(s: &str) -> Option<(u32, &str)> {
    let s = s.trim_start();
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let value = s[..end].parse().ok()?;
    Some((value, &s[end..]))
}

/// Sizes not offered yet, once each; sizes the driver can't show are never listed.
pub(crate) fn missing_resolutions(xml: &str, sizes: &[Size]) -> Vec<Size> {
    let present = present_resolutions(xml);
    let mut missing: Vec<Size> = Vec::new();
    for s in sizes {
        if fits_driver(*s) && !present.contains(&(s.width, s.height)) && !missing.contains(s) {
            missing.push(*s);
        }
    }
    missing
}

/// Sizes `settings_for` writes: once each, only showable ones, at most [`MAX_SIZES`], in order.
fn chosen(sizes: &[Size]) -> Vec<Size> {
    let mut out: Vec<Size> = Vec::new();
    for s in sizes {
        if out.len() < MAX_SIZES && fits_driver(*s) && !out.contains(s) {
            out.push(*s);
        }
    }
    out
}

/// The global refresh rates the settings repeat for every size.
fn global_rates(xml: &str) -> Vec<u32> {
    const OPEN: &str = "<g_refresh_rate>";
    xml.match_indices(OPEN).filter_map(|(at, _)| number(&xml[at + OPEN.len()..]).map(|(hz, _)| hz)).collect()
}

/// Replaces what is between `open` and `close` (both kept); None if either is missing.
fn replace_between(xml: &str, open: &str, close: &str, inner: &str) -> Option<String> {
    let start = xml.find(open)? + open.len();
    let end = start + xml[start..].find(close)?;
    Some(format!("{}{inner}{}", &xml[..start], &xml[end..]))
}

/// `base` (the driver's default file) offering only these sizes at 60 Hz; other options kept.
pub(crate) fn settings_for(base: &str, sizes: &[Size]) -> Result<String> {
    let rates = format!("
		<g_refresh_rate>{RATE_HZ}</g_refresh_rate>
	");
    let entries: String = chosen(sizes)
        .iter()
        .map(|s| {
            format!(
                "
        <resolution>
            <width>{}</width>
            <height>{}</height>
            <refresh_rate>{RATE_HZ}</refresh_rate>
        </resolution>",
                s.width, s.height
            )
        })
        .collect();
    let xml = replace_between(base, "<global>", "</global>", &rates)
        .ok_or_else(|| DisplayError::new("Virtual Display Driver settings have no <global> list"))?;
    replace_between(&xml, "<resolutions>", "</resolutions>", &format!("{entries}
    "))
        .ok_or_else(|| DisplayError::new("Virtual Display Driver settings have no <resolutions> list"))
}

/// Whether the settings offer exactly the modes `settings_for(_, sizes)` would.
pub(crate) fn settings_match(xml: &str, sizes: &[Size]) -> bool {
    let mut present = present_resolutions(xml);
    let mut wanted: Vec<(u32, u32)> = chosen(sizes).iter().map(|s| (s.width, s.height)).collect();
    present.sort_unstable();
    wanted.sort_unstable();
    global_rates(xml) == [RATE_HZ] && present == wanted
}

/// Whether the driver's settings (in `file`) offer every one of these sizes.
pub(crate) fn offers(file: &Path, sizes: &[Size]) -> Result<bool> {
    let xml = fs::read_to_string(file).map_err(|err| {
        DisplayError::new(format!("Could not read the Virtual Display Driver settings ({}): {err}", file.display()))
    })?;
    Ok(missing_resolutions(&xml, sizes).is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = "<?xml version='1.0' encoding='utf-8'?>
<vdd_settings>
    <monitors>
        <count>1</count>
    </monitors>
    <resolutions>
        <resolution>
            <width>1920</width>
            <height>1080</height>
            <refresh_rate>60</refresh_rate>
        </resolution>
    </resolutions>
</vdd_settings>";

    const fn size(width: u32, height: u32) -> Size {
        Size { width, height }
    }

    #[test]
    fn finds_which_resolutions_the_driver_does_not_offer_yet() {
        assert_eq!(missing_resolutions(XML, &[size(1920, 1080), size(2360, 1640)]), vec![size(2360, 1640)]);
        assert_eq!(missing_resolutions(XML, &[size(1920, 1080)]), vec![]);
        assert_eq!(missing_resolutions(XML, &[size(590, 1278), size(1278, 590)]), vec![size(1278, 590)], "never one it can't show");
    }

    const BASE: &str = "<vdd_settings>
	<global>
		<g_refresh_rate>60</g_refresh_rate>
		<g_refresh_rate>244</g_refresh_rate>
	</global>
    <resolutions>
        <resolution>
            <width>800</width>
            <height>600</height>
            <refresh_rate>30</refresh_rate>
        </resolution>
    </resolutions>
    <options><HardwareCursor>true</HardwareCursor></options>
</vdd_settings>";

    #[test]
    fn settings_offer_exactly_the_given_sizes_at_60_hz_and_keep_the_options() {
        let xml = settings_for(BASE, &[size(2360, 1640), size(1640, 2360), size(2360, 1640), size(590, 1278)]).unwrap();
        assert_eq!(present_resolutions(&xml), vec![(2360, 1640), (1640, 2360)], "once each, never one it can't show");
        assert_eq!(global_rates(&xml), vec![60]);
        assert_eq!(xml.matches("<refresh_rate>60</refresh_rate>").count(), 2);
        assert!(xml.contains("<HardwareCursor>true</HardwareCursor>"));
        assert!(settings_match(&xml, &[size(1640, 2360), size(2360, 1640)]), "order doesn't matter");
        assert!(!settings_match(&xml, &[size(2360, 1640)]));
        assert!(!settings_match(BASE, &[size(800, 600)]), "the default's many global rates don't match");
    }

    #[test]
    fn settings_never_offer_more_than_the_driver_can_take() {
        let many: Vec<Size> = (0..100).map(|i| size(1000 + i * 2, 800)).collect();
        let xml = settings_for(BASE, &many).unwrap();
        assert_eq!(present_resolutions(&xml).len(), MAX_SIZES);
        assert_eq!(present_resolutions(&xml)[0], (1000, 800), "the first ones are kept");
        assert!(settings_match(&xml, &many));
    }

    #[test]
    fn refuses_a_settings_file_without_its_lists() {
        assert!(settings_for("<vdd_settings/>", &[size(1920, 1080)]).unwrap_err().message().contains("global"));
        assert!(settings_for("<global></global>", &[size(1920, 1080)]).unwrap_err().message().contains("resolutions"));
    }

    #[test]
    fn parsing_tolerates_whitespace_and_skips_malformed_entries() {
        let xml = "<resolution><width> 4 </width>
<height>3</height></resolution><resolution><width>x</width></resolution>";
        assert_eq!(present_resolutions(xml), vec![(4, 3)]);
    }

    #[test]
    fn reads_whether_the_settings_file_offers_sizes() {
        let dir = std::env::temp_dir().join(format!("xiro-display-vdd-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = settings_file(&dir);
        fs::write(&file, XML).unwrap();
        assert!(offers(&file, &[size(1920, 1080)]).unwrap());
        assert!(!offers(&file, &[size(1920, 1080), size(2360, 1640)]).unwrap());
        fs::remove_dir_all(&dir).unwrap();
        assert!(offers(&file, &[size(1920, 1080)]).is_err());
    }
}
