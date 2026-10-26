//! Pure CCD planning: which display paths to keep active so only the virtual monitor
//! changes; every other monitor keeps its source, mode and position.

use crate::error::{DisplayError, Result};

/// "No mode": lets Windows pick (or look up) the mode for a path.
pub(crate) const MODE_IDX_INVALID: u32 = 0xffff_ffff;

/// A graphics adapter id (LUID). Changes when the adapter is re-created, e.g.
/// after a driver reload, which is how a finished reload is detected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Luid {
    pub high: i32,
    pub low: u32,
}

/// The source (desktop surface) end of a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Source {
    pub adapter: Luid,
    pub id: u32,
    pub mode_idx: u32,
}

/// The target (monitor) end of a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    /// Device path, e.g. `\\?\DISPLAY#MTT1337#...`. Match monitors by this, never adapter + id:
    /// Windows may report a mirrored monitor under another adapter.
    pub key: String,
    pub adapter: Luid,
    pub id: u32,
    pub mode_idx: u32,
}

/// One source -> target path, as returned by QueryDisplayConfig.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DisplayPath {
    pub source: Source,
    pub target: Target,
    pub active: bool,
    /// Index into the query result; the backend rewrites that record so unmodelled fields survive.
    pub index: usize,
}

impl DisplayPath {
    fn source_key(&self) -> (Luid, u32) {
        (self.source.adapter, self.source.id)
    }

    fn is_target(&self, key: &str) -> bool {
        self.target.key == key
    }

    fn without_mode(&self) -> DisplayPath {
        let mut path = self.clone();
        path.source.mode_idx = MODE_IDX_INVALID;
        path.target.mode_idx = MODE_IDX_INVALID;
        path
    }
}

/// True when the target shows its own desktop (extended), not a copy of another monitor's.
pub(crate) fn is_extended(paths: &[DisplayPath], key: &str) -> bool {
    let active: Vec<&DisplayPath> = paths.iter().filter(|p| p.active).collect();
    let Some(own) = active.iter().find(|p| p.is_target(key)) else {
        return false;
    };
    active.iter().filter(|p| p.source_key() == own.source_key()).count() == 1
}

/// Active path set that shows the target as an extended monitor.
/// `all_paths` comes from QDC_ALL_PATHS. `None` when it is already extended.
pub(crate) fn plan_attach(all_paths: &[DisplayPath], key: &str) -> Result<Option<Vec<DisplayPath>>> {
    if is_extended(all_paths, key) {
        return Ok(None);
    }
    // Drop the target from any mirror it is part of; everything else stays as it is.
    let kept: Vec<DisplayPath> = all_paths.iter().filter(|p| p.active && !p.is_target(key)).cloned().collect();
    let used: Vec<(Luid, u32)> = kept.iter().map(DisplayPath::source_key).collect();
    let free: Vec<&DisplayPath> = all_paths
        .iter()
        .filter(|p| p.is_target(key) && !used.contains(&p.source_key()))
        .collect();
    // Prefer a source on the monitor's own adapter: that is the driver's real output.
    let candidate = free
        .iter()
        .find(|p| p.source.adapter == p.target.adapter)
        .or_else(|| free.first())
        .ok_or_else(|| DisplayError::new("No free display source for the virtual monitor"))?;
    let mut added = candidate.without_mode();
    added.active = true;
    let mut plan = kept;
    plan.push(added);
    Ok(Some(plan))
}

/// Active path set without the target (`active_paths` from QDC_ONLY_ACTIVE_PATHS); `None` when
/// not active. May be empty: the caller must never switch off the last screen.
pub(crate) fn plan_detach(active_paths: &[DisplayPath], key: &str) -> Option<Vec<DisplayPath>> {
    let active: Vec<&DisplayPath> = active_paths.iter().filter(|p| p.active).collect();
    if !active.iter().any(|p| p.is_target(key)) {
        return None;
    }
    Some(active.into_iter().filter(|p| !p.is_target(key)).cloned().collect())
}

/// Without modes, SDC_TOPOLOGY_SUPPLIED restores the user's arrangement from Windows' database.
pub(crate) fn without_modes(paths: &[DisplayPath]) -> Vec<DisplayPath> {
    paths.iter().map(DisplayPath::without_mode).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const GPU: Luid = Luid { high: 0, low: 100 };
    pub const VDD: Luid = Luid { high: 0, low: 200 };

    pub fn path(src: Luid, src_id: u32, tgt: Luid, tgt_id: u32, active: bool) -> DisplayPath {
        path_keyed(src, src_id, tgt, &format!("mon-{tgt_id}"), tgt_id, active)
    }

    pub fn path_keyed(src: Luid, src_id: u32, tgt: Luid, key: &str, tgt_id: u32, active: bool) -> DisplayPath {
        let mode_idx = if active { 0 } else { MODE_IDX_INVALID };
        DisplayPath {
            source: Source { adapter: src, id: src_id, mode_idx },
            target: Target { key: key.to_owned(), adapter: tgt, id: tgt_id, mode_idx },
            active,
            index: 0,
        }
    }

    const PANEL: &str = "mon-1";
    const VIRTUAL: &str = "mon-7";

    #[test]
    fn attaching_a_mirrored_virtual_monitor_moves_it_onto_its_own_source() {
        // Clone: the GPU's source 0 drives both the laptop panel and the virtual monitor.
        let all = vec![
            path(GPU, 0, GPU, 1, true),
            path(GPU, 0, VDD, 7, true),
            path(VDD, 0, VDD, 7, false),
            path(VDD, 1, VDD, 7, false),
        ];
        assert!(!is_extended(&all, VIRTUAL));

        let plan = plan_attach(&all, VIRTUAL).unwrap().unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0], all[0], "the panel path is untouched");
        assert_eq!(plan[1].source.adapter, VDD, "the virtual monitor gets a source on its own adapter");
        assert!(plan[1].active);
        assert_eq!(plan[1].source.mode_idx, MODE_IDX_INVALID, "Windows picks the mode for the new path");
        assert!(!all[2].active, "input paths are not mutated");
    }

    #[test]
    fn attaching_never_steals_a_source_another_monitor_is_using() {
        let all = vec![
            path(GPU, 0, GPU, 1, true),
            path(VDD, 0, VDD, 3, true), // another virtual monitor already uses VDD source 0
            path(VDD, 0, VDD, 7, false),
            path(VDD, 1, VDD, 7, false),
        ];
        let plan = plan_attach(&all, VIRTUAL).unwrap().unwrap();
        assert_eq!(plan.last().unwrap().source.id, 1);
        assert_eq!(plan.len(), 3, "other monitors stay on");
    }

    #[test]
    fn attaching_is_a_no_op_when_the_virtual_monitor_is_already_extended() {
        let all = vec![path(GPU, 0, GPU, 1, true), path(VDD, 0, VDD, 7, true)];
        assert!(is_extended(&all, VIRTUAL));
        assert_eq!(plan_attach(&all, VIRTUAL).unwrap(), None);
    }

    #[test]
    fn attaching_fails_clearly_when_no_source_is_free() {
        let all = vec![path(GPU, 0, GPU, 1, true), path(GPU, 0, VDD, 7, false)];
        let err = plan_attach(&all, VIRTUAL).unwrap_err();
        assert!(err.message().contains("No free display source"));
    }

    #[test]
    fn detaching_removes_only_the_virtual_monitor() {
        let active = vec![path(GPU, 0, GPU, 1, true), path(GPU, 1, GPU, 2, true), path(VDD, 0, VDD, 7, true)];
        let plan = plan_detach(&active, VIRTUAL).unwrap();
        assert_eq!(plan, active[..2].to_vec());
        assert_eq!(plan_detach(&plan, VIRTUAL), None, "nothing to do when already detached");
        assert_eq!(plan_detach(&[path(GPU, 0, GPU, 1, true)], PANEL), Some(vec![]), "can produce an empty set (caller guards)");
    }

    #[test]
    fn without_modes_asks_windows_to_restore_the_saved_arrangement() {
        let plan = without_modes(&[path(GPU, 0, GPU, 1, true)]);
        assert_eq!(plan[0].source.mode_idx, MODE_IDX_INVALID);
        assert_eq!(plan[0].target.mode_idx, MODE_IDX_INVALID);
    }
}
