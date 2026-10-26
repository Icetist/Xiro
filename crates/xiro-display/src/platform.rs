//! Everything [`crate::VirtualDisplay`] needs from Windows, behind one trait so the
//! policy (positions, retries, reload detection) can be tested against a fake.

use std::path::PathBuf;
use std::sync::Mutex;

use crate::ccd::{self, Applied, Config, Query};
use crate::error::Result;
use crate::gdi::{self, Bounds, CurrentMode, Position};
use crate::topology::{DisplayPath, Source};
use crate::vdd;
use crate::{Mode, Size};

/// The Windows display stack, the driver and the clock.
pub(crate) trait Platform: Send + Sync {
    fn query_config(&self, query: Query) -> Result<Config>;
    fn apply_paths(&self, config: &Config, plan: &[DisplayPath], saved: Option<&[DisplayPath]>) -> Result<Applied>;
    fn source_gdi_name(&self, source: &Source) -> Option<String>;
    /// The percentage applied, or `None` if Windows refused.
    fn set_scale(&self, source: &Source, percent: u32) -> Option<u32>;
    fn current_mode(&self, gdi_name: &str) -> Option<CurrentMode>;
    /// A DISP_CHANGE_* code; 0 is success.
    fn set_mode(&self, gdi_name: &str, mode: Mode, position: Option<Position>) -> i32;
    fn primary_bounds(&self) -> Option<Bounds>;

    /// Whether the driver's settings offer every one of these sizes.
    fn offers_sizes(&self, sizes: &[Size]) -> Result<bool>;
}

pub(crate) struct WindowsPlatform {
    /// Resolved lazily so `new` stays infallible.
    settings: Mutex<Option<PathBuf>>,
}

impl WindowsPlatform {
    pub(crate) fn new() -> Self {
        Self { settings: Mutex::new(None) }
    }

    fn settings_file(&self) -> PathBuf {
        let mut cached = self.settings.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        cached.get_or_insert_with(|| vdd::settings_file(&vdd::settings_dir())).clone()
    }
}

impl Platform for WindowsPlatform {
    fn query_config(&self, query: Query) -> Result<Config> {
        ccd::query_config(query)
    }

    fn apply_paths(&self, config: &Config, plan: &[DisplayPath], saved: Option<&[DisplayPath]>) -> Result<Applied> {
        ccd::apply_paths(config, plan, saved)
    }

    fn source_gdi_name(&self, source: &Source) -> Option<String> {
        ccd::source_gdi_name(source)
    }

    fn set_scale(&self, source: &Source, percent: u32) -> Option<u32> {
        ccd::set_scale(source, percent)
    }

    fn current_mode(&self, gdi_name: &str) -> Option<CurrentMode> {
        gdi::current_mode(gdi_name)
    }

    fn set_mode(&self, gdi_name: &str, mode: Mode, position: Option<Position>) -> i32 {
        gdi::set_mode(gdi_name, mode, position)
    }

    fn primary_bounds(&self) -> Option<Bounds> {
        gdi::primary_bounds()
    }

    fn offers_sizes(&self, sizes: &[Size]) -> Result<bool> {
        vdd::offers(&self.settings_file(), sizes)
    }
}
