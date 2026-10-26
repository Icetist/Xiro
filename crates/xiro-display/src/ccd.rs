//! Windows display configuration (CCD): paths, device names and per-monitor scaling.
//! Works from a non-elevated process.

use std::collections::HashMap;

use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_DEVICE_INFO_TYPE, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
    DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo, DisplayConfigSetDeviceInfo, GetDisplayConfigBufferSizes,
    QUERY_DISPLAY_CONFIG_FLAGS, QueryDisplayConfig, SDC_ALLOW_CHANGES, SDC_ALLOW_PATH_ORDER_CHANGES, SDC_APPLY,
    SDC_SAVE_TO_DATABASE, SDC_TOPOLOGY_SUPPLIED, SDC_USE_SUPPLIED_DISPLAY_CONFIG, SetDisplayConfig,
};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, LUID};

use crate::error::{DisplayError, Result};
use crate::topology::{DisplayPath, Luid, Source, Target};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Query {
    /// Every possible source -> target pair (QDC_ALL_PATHS): needed to find a free source.
    All,
    /// What is shown right now (QDC_ONLY_ACTIVE_PATHS).
    Active,
}

/// The OS records a query returned; planned paths point back into them by index.
#[derive(Clone, Default)]
pub(crate) struct NativeConfig {
    paths: Vec<DISPLAYCONFIG_PATH_INFO>,
    modes: Vec<DISPLAYCONFIG_MODE_INFO>,
}

/// A QueryDisplayConfig result: the planner's view plus the native records.
#[derive(Clone, Default)]
pub(crate) struct Config {
    pub paths: Vec<DisplayPath>,
    pub native: NativeConfig,
}

/// How [`apply_paths`] got the topology in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Applied {
    /// Windows' saved arrangement for this set of monitors was restored.
    Restored,
    /// Windows laid it out from the supplied modes and saved it.
    New,
}

const PATH_ACTIVE: u32 = 0x1;
/// A monitor can appear between sizing the buffers and filling them; retry a few times.
const QUERY_ATTEMPTS: u32 = 5;
// Undocumented but stable since Windows 10 1607; the Settings app uses them for "Scale".
const INFO_GET_DPI_SCALE: DISPLAYCONFIG_DEVICE_INFO_TYPE = DISPLAYCONFIG_DEVICE_INFO_TYPE(-3);
const INFO_SET_DPI_SCALE: DISPLAYCONFIG_DEVICE_INFO_TYPE = DISPLAYCONFIG_DEVICE_INFO_TYPE(-4);
/// Scale steps Windows offers, in percent. Relative scale values index into this list.
pub(crate) const SCALE_STEPS: [u32; 12] = [100, 125, 150, 175, 200, 225, 250, 300, 350, 400, 450, 500];

impl Query {
    fn flags(self) -> QUERY_DISPLAY_CONFIG_FLAGS {
        use windows::Win32::Devices::Display::{QDC_ALL_PATHS, QDC_ONLY_ACTIVE_PATHS};
        match self {
            Query::All => QDC_ALL_PATHS,
            Query::Active => QDC_ONLY_ACTIVE_PATHS,
        }
    }
}

fn luid(l: LUID) -> Luid {
    Luid { high: l.HighPart, low: l.LowPart }
}

fn native_luid(l: Luid) -> LUID {
    LUID { LowPart: l.low, HighPart: l.high }
}

pub(crate) fn query_config(query: Query) -> Result<Config> {
    for _ in 0..QUERY_ATTEMPTS {
        let (mut n_paths, mut n_modes) = (0u32, 0u32);
        // SAFETY: both out-pointers are valid for the call.
        let rc = unsafe { GetDisplayConfigBufferSizes(query.flags(), &mut n_paths, &mut n_modes) };
        if rc != ERROR_SUCCESS {
            return Err(DisplayError::new(format!("GetDisplayConfigBufferSizes failed ({})", rc.0)));
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths.max(1) as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes.max(1) as usize];
        // SAFETY: the buffers hold n_paths / n_modes elements and outlive the call.
        let rc = unsafe {
            QueryDisplayConfig(query.flags(), &mut n_paths, paths.as_mut_ptr(), &mut n_modes, modes.as_mut_ptr(), None)
        };
        if rc == ERROR_INSUFFICIENT_BUFFER {
            continue; // a monitor appeared in between
        }
        if rc != ERROR_SUCCESS {
            return Err(DisplayError::new(format!("QueryDisplayConfig failed ({})", rc.0)));
        }
        paths.truncate(n_paths as usize);
        modes.truncate(n_modes as usize);
        let planned = describe(&paths);
        return Ok(Config { paths: planned, native: NativeConfig { paths, modes } });
    }
    Err(DisplayError::new("QueryDisplayConfig kept changing"))
}

/// The planner's view of native paths. Monitor keys are looked up once per target.
fn describe(paths: &[DISPLAYCONFIG_PATH_INFO]) -> Vec<DisplayPath> {
    let mut names: HashMap<(Luid, u32), String> = HashMap::new();
    paths
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            let (src, tgt) = (&raw.sourceInfo, &raw.targetInfo);
            let adapter = luid(tgt.adapterId);
            let key = names
                .entry((adapter, tgt.id))
                .or_insert_with(|| {
                    target_device_path(adapter, tgt.id).unwrap_or_else(|| format!("{}:{}/{}", adapter.high, adapter.low, tgt.id))
                })
                .clone();
            // SAFETY: without DISPLAYCONFIG_PATH_SUPPORT_VIRTUAL_MODE both unions hold modeInfoIdx.
            let (src_mode, tgt_mode) = unsafe { (src.Anonymous.modeInfoIdx, tgt.Anonymous.modeInfoIdx) };
            DisplayPath {
                source: Source { adapter: luid(src.adapterId), id: src.id, mode_idx: src_mode },
                target: Target { key, adapter, id: tgt.id, mode_idx: tgt_mode },
                active: raw.flags & PATH_ACTIVE != 0,
                index,
            }
        })
        .collect()
}

/// Writes planned paths back into the native layout, starting from each path's original record.
fn serialize(config: &Config, plan: &[DisplayPath]) -> Result<Vec<DISPLAYCONFIG_PATH_INFO>> {
    plan.iter()
        .map(|p| {
            let mut raw = *config
                .native
                .paths
                .get(p.index)
                .ok_or_else(|| DisplayError::new("Display plan does not match the queried configuration"))?;
            raw.sourceInfo.Anonymous.modeInfoIdx = p.source.mode_idx;
            raw.targetInfo.Anonymous.modeInfoIdx = p.target.mode_idx;
            raw.flags = if p.active { raw.flags | PATH_ACTIVE } else { raw.flags & !PATH_ACTIVE };
            Ok(raw)
        })
        .collect()
}

/// Tries Windows' saved arrangement for these monitors first (keeps the user's layout), then lets Windows choose.
pub(crate) fn apply_paths(config: &Config, plan: &[DisplayPath], saved: Option<&[DisplayPath]>) -> Result<Applied> {
    if let Some(saved) = saved {
        let paths = serialize(config, saved)?;
        let flags = SDC_APPLY | SDC_TOPOLOGY_SUPPLIED | SDC_ALLOW_PATH_ORDER_CHANGES;
        // SAFETY: `paths` is a valid slice for the duration of the call.
        if unsafe { SetDisplayConfig(Some(&paths), None, flags) } == 0 {
            return Ok(Applied::Restored);
        }
    }
    let paths = serialize(config, plan)?;
    let modes = &config.native.modes;
    let modes = (!modes.is_empty()).then_some(modes.as_slice());
    let flags = SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES | SDC_SAVE_TO_DATABASE;
    // SAFETY: both slices are valid for the duration of the call.
    let rc = unsafe { SetDisplayConfig(Some(&paths), modes, flags) };
    if rc != 0 {
        return Err(DisplayError::new(format!("SetDisplayConfig failed ({rc})")));
    }
    Ok(Applied::New)
}

fn header(kind: DISPLAYCONFIG_DEVICE_INFO_TYPE, size: usize, adapter: Luid, id: u32) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type: kind,
        // Packets are a few hundred bytes at most.
        size: u32::try_from(size).unwrap_or(u32::MAX),
        adapterId: native_luid(adapter),
        id,
    }
}

fn wide(chars: &[u16]) -> String {
    let end = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
    String::from_utf16_lossy(&chars[..end])
}

/// Monitor device path, e.g. `\\?\DISPLAY#MTT1337#...`.
fn target_device_path(adapter: Luid, id: u32) -> Option<String> {
    let mut packet = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: header(DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(), adapter, id),
        ..Default::default()
    };
    // SAFETY: the packet starts with its header and its size is stated in it.
    let rc = unsafe { DisplayConfigGetDeviceInfo(&mut packet.header) };
    let path = wide(&packet.monitorDevicePath);
    (rc == 0 && !path.is_empty()).then_some(path)
}

/// GDI name of a source, e.g. `\\.\DISPLAY6`.
pub(crate) fn source_gdi_name(source: &Source) -> Option<String> {
    let size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>();
    let mut packet = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
        header: header(DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, size, source.adapter, source.id),
        ..Default::default()
    };
    // SAFETY: the packet starts with its header and its size is stated in it.
    let rc = unsafe { DisplayConfigGetDeviceInfo(&mut packet.header) };
    let name = wide(&packet.viewGdiDeviceName);
    (rc == 0 && !name.is_empty()).then_some(name)
}

/// DISPLAYCONFIG_SOURCE_DPI_SCALE_GET: scales relative to the recommended one.
#[repr(C)]
struct DpiScaleGet {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    min_relative: i32,
    /// Part of the packet layout only.
    _current_relative: i32,
    max_relative: i32,
}

#[repr(C)]
struct DpiScaleSet {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    relative: i32,
}

/// Windows reports `min` as minus the recommended step's index, so step = |min| + relative.
/// Returns (relative value to set, percent it means).
pub(crate) fn relative_scale(min: i32, max: i32, percent: u32) -> Option<(i32, u32)> {
    let recommended = i64::from(min).abs();
    let wanted = (0..SCALE_STEPS.len())
        .min_by_key(|&i| SCALE_STEPS[i].abs_diff(percent))
        .and_then(|i| i64::try_from(i).ok())?;
    let relative = (wanted - recommended).clamp(i64::from(min), i64::from(max));
    let step = usize::try_from(recommended + relative).ok()?;
    Some((i32::try_from(relative).ok()?, *SCALE_STEPS.get(step)?))
}

/// Snaps to the nearest allowed step; returns the percent applied, or `None` if Windows refused.
pub(crate) fn set_scale(source: &Source, percent: u32) -> Option<u32> {
    let mut get = DpiScaleGet {
        header: header(INFO_GET_DPI_SCALE, size_of::<DpiScaleGet>(), source.adapter, source.id),
        min_relative: 0,
        _current_relative: 0,
        max_relative: 0,
    };
    // SAFETY: the packet is repr(C), starts with its header and states its size.
    if unsafe { DisplayConfigGetDeviceInfo(&mut get.header) } != 0 {
        return None;
    }
    let (relative, applied) = relative_scale(get.min_relative, get.max_relative, percent)?;
    let set = DpiScaleSet { header: header(INFO_SET_DPI_SCALE, size_of::<DpiScaleSet>(), source.adapter, source.id), relative };
    // SAFETY: as above.
    (unsafe { DisplayConfigSetDeviceInfo(&set.header) } == 0).then_some(applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_snaps_to_the_nearest_step_relative_to_the_recommended_one() {
        // Recommended 150 % (index 2), range 100..=300 %.
        assert_eq!(relative_scale(-2, 5, 200), Some((2, 200)));
        assert_eq!(relative_scale(-2, 5, 190), Some((2, 200)));
        assert_eq!(relative_scale(-2, 5, 100), Some((-2, 100)));
        // Out of range: clamped to what the monitor allows.
        assert_eq!(relative_scale(-2, 5, 500), Some((5, 300)));
        assert_eq!(relative_scale(0, 1, 100), Some((0, 100)));
    }

    #[test]
    fn packets_have_the_documented_sizes() {
        assert_eq!(size_of::<DpiScaleGet>(), 32);
        assert_eq!(size_of::<DpiScaleSet>(), 24);
        assert_eq!(size_of::<DISPLAYCONFIG_PATH_INFO>(), 72);
        assert_eq!(size_of::<DISPLAYCONFIG_MODE_INFO>(), 64);
    }
}
