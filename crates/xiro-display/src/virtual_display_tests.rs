//! VirtualDisplay against a fake Windows display stack.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use super::VirtualDisplay;
use crate::ccd::{Applied, Config, Query};
use crate::error::Result;
use crate::gdi::{Bounds, CurrentMode, Position};
use crate::platform::Platform;
use crate::topology::tests::path_keyed;
use crate::topology::{DisplayPath, Luid, Source};
use crate::{Mode, Size};

const VDD_KEY: &str = r"\\?\DISPLAY#MTT1337#1&28a6823a&0&UID256";
const PANEL_KEY: &str = r"\\?\DISPLAY#LGD045C#4&1";
const GDI: &str = r"\\.\DISPLAY6";
const IPAD: Size = Size { width: 2360, height: 1640 };
const NATIVE_SCALE: u32 = 200;
const HALF_SCALE: u32 = 100;
const PANEL_ADAPTER: Luid = Luid { high: 0, low: 1 };

struct State {
    vdd_adapter: Luid,
    vdd_present: bool,
    panel_active: bool,
    attached: bool,
    no120: bool,
    mode: CurrentMode,
    applied: Vec<Vec<DisplayPath>>,
    modes_set: Vec<(Mode, Option<Position>)>,
    scale: Option<u32>,
    /// The sizes the driver's settings offer.
    offered: Vec<Size>,
}

#[derive(Clone)]
struct Fake(Arc<Mutex<State>>);

impl Fake {
    fn new() -> Self {
        Fake(Arc::new(Mutex::new(State {
            vdd_adapter: Luid { high: 0, low: 500 },
            vdd_present: true,
            panel_active: true,
            attached: false,
            no120: false,
            mode: CurrentMode { width: 800, height: 600, hz: 60, x: -800, y: 0 },
            applied: vec![],
            modes_set: vec![],
            scale: None,
            offered: vec![],
        })))
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }

    fn display(&self, name: &str) -> Harness {
        let layout = std::env::temp_dir().join(format!("xiro-display-vd-{}-{name}.json", std::process::id()));
        let _ = std::fs::remove_file(&layout);
        Harness { vd: VirtualDisplay::with_platform(Box::new(self.clone()), layout.clone()), layout }
    }
}

/// The display under test; deletes its layout file afterwards.
struct Harness {
    vd: VirtualDisplay,
    layout: PathBuf,
}

impl std::ops::Deref for Harness {
    type Target = VirtualDisplay;
    fn deref(&self) -> &VirtualDisplay {
        &self.vd
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.layout);
    }
}

impl Platform for Fake {
    fn query_config(&self, query: Query) -> Result<Config> {
        let s = self.state();
        let mut panel = path_keyed(PANEL_ADAPTER, 0, PANEL_ADAPTER, PANEL_KEY, 1, s.panel_active);
        panel.target.mode_idx = 1;
        let mut all = vec![panel];
        if s.vdd_present {
            all.push(path_keyed(s.vdd_adapter, 0, s.vdd_adapter, VDD_KEY, 1, s.attached));
        }
        let paths = if query == Query::Active { all.into_iter().filter(|p| p.active).collect() } else { all };
        Ok(Config { paths, ..Config::default() })
    }

    fn apply_paths(&self, _: &Config, plan: &[DisplayPath], _: Option<&[DisplayPath]>) -> Result<Applied> {
        let mut s = self.state();
        s.attached = plan.iter().any(|p| p.target.key == VDD_KEY);
        s.applied.push(plan.to_vec());
        Ok(Applied::Restored)
    }

    fn source_gdi_name(&self, _: &Source) -> Option<String> {
        Some(GDI.to_owned())
    }

    fn set_scale(&self, _: &Source, percent: u32) -> Option<u32> {
        self.state().scale = Some(percent);
        Some(percent)
    }

    fn current_mode(&self, _: &str) -> Option<CurrentMode> {
        let s = self.state();
        s.attached.then_some(s.mode)
    }

    fn set_mode(&self, _: &str, mode: Mode, position: Option<Position>) -> i32 {
        let mut s = self.state();
        s.modes_set.push((mode, position));
        if mode.hz == 120 && s.no120 {
            return -2;
        }
        let p = position.unwrap_or(Position { x: s.mode.x, y: s.mode.y });
        s.mode = CurrentMode { width: mode.width, height: mode.height, hz: mode.hz, x: p.x, y: p.y };
        0
    }

    fn primary_bounds(&self) -> Option<Bounds> {
        Some(Bounds { left: 0, top: 0, width: 1366 })
    }

    fn offers_sizes(&self, sizes: &[Size]) -> Result<bool> {
        let s = self.state();
        Ok(sizes.iter().all(|size| s.offered.contains(size)))
    }
}

const fn mode(width: u32, height: u32, hz: u32) -> Mode {
    Mode { width, height, hz }
}

fn last_mode_set(fake: &Fake) -> (Mode, Option<Position>) {
    *fake.state().modes_set.last().unwrap()
}

#[test]
fn attach_turns_the_virtual_monitor_on_beside_the_main_screen_in_the_ipad_mode() {
    let fake = Fake::new();
    let vd = fake.display("beside");
    assert!(vd.available().unwrap());
    let result = vd.attach(mode(2360, 1640, 120), NATIVE_SCALE).unwrap();
    assert_eq!(result.gdi_name, GDI);
    assert_eq!(result.hz, 120);
    assert!(result.changed);
    assert!(fake.state().attached);
    assert_eq!(last_mode_set(&fake), (mode(2360, 1640, 120), Some(Position { x: 1366, y: 0 })));
    assert_eq!(fake.state().scale, Some(200));
}

#[test]
fn attach_falls_back_to_60_hz_when_120_is_refused_and_skips_no_op_mode_changes() {
    let fake = Fake::new();
    fake.state().no120 = true;
    let vd = fake.display("fallback");
    assert_eq!(vd.attach(mode(2360, 1640, 120), NATIVE_SCALE).unwrap().hz, 60);
    let before = fake.state().modes_set.len();
    let again = vd.attach(mode(2360, 1640, 60), NATIVE_SCALE).unwrap();
    assert_eq!(fake.state().modes_set.len(), before, "same mode: no flicker");
    assert!(!again.changed);
}

#[test]
fn detach_remembers_where_the_user_put_the_monitor_and_restores_it_next_time() {
    let fake = Fake::new();
    let vd = fake.display("remember");
    vd.attach(mode(2360, 1640, 60), NATIVE_SCALE).unwrap();
    {
        let mut s = fake.state();
        s.mode.x = -2360; // user dragged it to the left in Settings
        s.mode.y = 100;
    }
    assert!(vd.detach().unwrap());
    assert!(!fake.state().attached);
    assert!(!vd.detach().unwrap(), "already off");

    vd.attach(mode(1180, 820, 60), HALF_SCALE).unwrap();
    assert_eq!(last_mode_set(&fake), (mode(1180, 820, 60), Some(Position { x: -2360, y: 100 })));
}

#[test]
fn detach_never_switches_off_the_last_screen() {
    let fake = Fake::new();
    let vd = fake.display("last");
    vd.attach(mode(2360, 1640, 60), NATIVE_SCALE).unwrap();
    fake.state().panel_active = false; // e.g. the laptop lid was closed
    let applied = fake.state().applied.len();
    assert!(!vd.detach().unwrap());
    assert!(fake.state().attached, "the only screen stays on");
    assert_eq!(fake.state().applied.len(), applied, "nothing was applied");
}

#[test]
fn prepare_checks_that_the_driver_offers_the_device_in_both_orientations() {
    let fake = Fake::new();
    let vd = fake.display("prepare");
    assert_eq!(vd.prepare(IPAD).unwrap_err().message(), super::SIZES_NOT_SET_UP);
    fake.state().offered = vec![IPAD, Size { width: 1640, height: 2360 }, Size { width: 1180, height: 820 }];
    assert!(vd.prepare(IPAD).is_err(), "the half size in portrait is missing");
    fake.state().offered.push(Size { width: 820, height: 1180 });
    assert!(vd.prepare(IPAD).is_ok());
}

#[test]
fn a_rotation_keeps_the_monitor_where_it_is_turning_on_uses_the_saved_spot() {
    let fake = Fake::new();
    let vd = fake.display("rotate");
    assert!(vd.attach(mode(2360, 1640, 60), NATIVE_SCALE).unwrap().changed);
    {
        let mut s = fake.state();
        s.mode.x = 1366; // dragged during the session
        s.mode.y = 400;
    }
    let rotated = vd.attach(mode(1640, 2360, 60), NATIVE_SCALE).unwrap();
    assert!(rotated.changed);
    assert_eq!(last_mode_set(&fake), (mode(1640, 2360, 60), Some(Position { x: 1366, y: 400 })));
    assert!(!vd.attach(mode(1640, 2360, 60), NATIVE_SCALE).unwrap().changed);
}

#[test]
fn a_missing_driver_is_reported_clearly() {
    let fake = Fake::new();
    fake.state().vdd_present = false;
    let vd = fake.display("missing");
    assert!(!vd.available().unwrap());
    assert_eq!(vd.attach(mode(2360, 1640, 60), NATIVE_SCALE).unwrap_err().message(), "Virtual Display Driver not found");
    assert!(!vd.detach().unwrap());
}
