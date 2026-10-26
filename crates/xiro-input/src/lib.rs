//! Remote input for Xiro: the iPad's touches, pen, mouse and keys, injected on the streamed
//! monitor. Only the `win32` module calls into user32; the rest is unit-tested.

mod displays;
mod geometry;
mod handle;
mod injector;
mod keymap;
mod pointer_records;
mod protocol;
mod records;
mod sink;
mod win32;

pub use displays::{Monitor, MonitorChoice, OutputSize, is_virtual_adapter, match_output_index, pick_monitor};
pub use geometry::Rect;
pub use handle::InputHandle;
pub use keymap::{ScanCode, scan_code_for};
pub use protocol::{MSG_POINTER, MSG_WHEEL, POINTER_BYTES, Phase, Pointer, PointerKind, WHEEL_BYTES, Wheel};
pub use win32::{enable_dpi_awareness, list_monitors};
