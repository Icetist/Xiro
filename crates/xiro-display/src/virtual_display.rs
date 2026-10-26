//! Sidecar behaviour on Windows: a real extended monitor, shaped like the iPad,
//! that exists only while the iPad is connected.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::ccd::Query;
use crate::error::{DisplayError, Result};
use crate::gdi::{DISP_CHANGE_BADMODE, DISP_CHANGE_SUCCESSFUL, Position};
use crate::layout::LayoutFile;
use crate::platform::{Platform, WindowsPlatform};
use crate::topology::{DisplayPath, plan_attach, plan_detach, without_modes};
use crate::vdd;
use crate::{Attached, Mode, Size, stream_sizes, with_both_orientations};

/// The error `prepare` gives for a device whose sizes the driver doesn't offer yet.
pub const SIZES_NOT_SET_UP: &str = "The second screen isn't set up for this device yet. On the PC, click Fix in Xiro (one time).";
/// Every display mode set supports 60 Hz; used when a higher rate is refused.
const FALLBACK_HZ: u32 = 60;

/// The driver's monitor.
struct VddTarget {
    key: String,
}

/// The Sidecar-like virtual monitor. All methods block; call them from a worker thread.
pub struct VirtualDisplay {
    platform: Box<dyn Platform>,
    layout: LayoutFile,
    /// Attach and detach rewrite the whole topology; never interleave them.
    topology: Mutex<()>,
}

fn locked(m: &Mutex<()>) -> MutexGuard<'_, ()> {
    // The guarded data is `()`: a panic elsewhere cannot leave it inconsistent.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl VirtualDisplay {
    /// `layout_file`: JSON `{"x":i32,"y":i32}` remembering where the user put the monitor.
    pub fn new(layout_file: PathBuf) -> VirtualDisplay {
        Self::with_platform(Box::new(WindowsPlatform::new()), layout_file)
    }

    pub(crate) fn with_platform(platform: Box<dyn Platform>, layout_file: PathBuf) -> VirtualDisplay {
        VirtualDisplay { platform, layout: LayoutFile::new(layout_file), topology: Mutex::new(()) }
    }

    fn find_target(paths: &[DisplayPath]) -> Option<VddTarget> {
        let first = paths.iter().find(|p| vdd::is_vdd_monitor(&p.target.key))?;
        Some(VddTarget { key: first.target.key.clone() })
    }

    fn query_target(&self) -> Result<Option<VddTarget>> {
        Ok(Self::find_target(&self.platform.query_config(Query::All)?.paths))
    }

    /// Is the Virtual Display Driver's monitor known to Windows (on or off)?
    pub fn available(&self) -> Result<bool, DisplayError> {
        Ok(self.query_target()?.is_some())
    }

    /// Errors with [`SIZES_NOT_SET_UP`] when the driver doesn't offer this device's sizes yet.
    pub fn prepare(&self, panel: Size) -> Result<(), DisplayError> {
        let sizes = with_both_orientations(&stream_sizes(panel));
        if self.platform.offers_sizes(&sizes)? { Ok(()) } else { Err(DisplayError::new(SIZES_NOT_SET_UP)) }
    }

    /// Shows the virtual monitor as an extended display in the given mode.
    pub fn attach(&self, mode: Mode, scale_percent: u32) -> Result<Attached, DisplayError> {
        let _guard = locked(&self.topology);
        let all = self.platform.query_config(Query::All)?;
        let target = Self::find_target(&all.paths).ok_or_else(|| DisplayError::new("Virtual Display Driver not found"))?;
        let plan = plan_attach(&all.paths, &target.key)?;
        if let Some(plan) = &plan {
            self.platform.apply_paths(&all, plan, Some(&without_modes(plan)))?;
        }
        let turned_on = plan.is_some();

        let active = self.platform.query_config(Query::Active)?;
        let own = active
            .paths
            .iter()
            .find(|p| p.target.key == target.key)
            .ok_or_else(|| DisplayError::new("Windows did not turn on the virtual display"))?;
        let gdi_name = self
            .platform
            .source_gdi_name(&own.source)
            .ok_or_else(|| DisplayError::new("Windows did not name the virtual display"))?;
        let (hz, mode_changed) = self.apply_mode(&gdi_name, mode, turned_on)?;
        // Best effort, like Settings: a refused scale still leaves a usable monitor.
        let _ = self.platform.set_scale(&own.source, scale_percent);
        Ok(Attached { gdi_name, hz, changed: turned_on || mode_changed })
    }

    /// Returns (refresh rate in use, whether the mode was changed).
    fn apply_mode(&self, gdi_name: &str, mode: Mode, turned_on: bool) -> Result<(u32, bool)> {
        let current = self.platform.current_mode(gdi_name);
        if let Some(c) = current {
            if (c.width, c.height, c.hz) == (mode.width, mode.height, mode.hz) {
                return Ok((mode.hz, false)); // same mode: no flicker
            }
        }
        // Just turned on: where the user left it last time (or beside the main screen).
        // Already on (the iPad rotated): stay exactly where it is now.
        let position = match current {
            Some(c) if !turned_on => Some(Position { x: c.x, y: c.y }),
            _ => self.layout.get().or_else(|| self.beside_primary()),
        };
        let mut rc = self.platform.set_mode(gdi_name, mode, position);
        if rc == DISP_CHANGE_BADMODE && mode.hz != FALLBACK_HZ {
            rc = self.platform.set_mode(gdi_name, Mode { hz: FALLBACK_HZ, ..mode }, position);
            if rc == DISP_CHANGE_SUCCESSFUL {
                return Ok((FALLBACK_HZ, true));
            }
        }
        if rc != DISP_CHANGE_SUCCESSFUL {
            return Err(DisplayError::new(format!(
                "Windows refused {}x{} at {} Hz on the virtual display (code {rc})",
                mode.width, mode.height, mode.hz
            )));
        }
        Ok((mode.hz, true))
    }

    /// Right of the main screen, like Sidecar.
    fn beside_primary(&self) -> Option<Position> {
        self.platform.primary_bounds().map(|b| Position { x: b.left + b.width, y: b.top })
    }

    /// Turns the monitor off, saving its position first; never switches off the last screen.
    /// Returns true when it was on and is now off.
    pub fn detach(&self) -> Result<bool, DisplayError> {
        let _guard = locked(&self.topology);
        let Some(target) = self.query_target()? else {
            return Ok(false);
        };
        let active = self.platform.query_config(Query::Active)?;
        if let Some(own) = active.paths.iter().find(|p| p.target.key == target.key) {
            let mode = self.platform.source_gdi_name(&own.source).and_then(|name| self.platform.current_mode(&name));
            if let Some(m) = mode {
                // A failed save only means the default spot next time; it must never keep the monitor on.
                let _ = self.layout.set(Position { x: m.x, y: m.y });
            }
        }
        let Some(plan) = plan_detach(&active.paths, &target.key).filter(|p| !p.is_empty()) else {
            return Ok(false);
        };
        self.platform.apply_paths(&active, &plan, Some(&without_modes(&plan)))?;
        Ok(true)
    }
}

#[cfg(test)]
#[path = "virtual_display_tests.rs"]
mod tests;
