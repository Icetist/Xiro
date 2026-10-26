//! Realtime settings that only make sense while encoding (they cost battery when idle).

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows::Win32::System::Threading::{AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW};
use windows::core::w;

/// Encoder drivers wait on the system timer; the default 15.6 ms tick halves their throughput.
const TIMER_RESOLUTION_MS: u32 = 1;

/// 1 ms system timer and MMCSS "Capture" scheduling for the calling thread, until dropped.
pub struct Realtime {
    mmcss: Option<HANDLE>,
}

impl Realtime {
    pub fn enter() -> Self {
        unsafe {
            timeBeginPeriod(TIMER_RESOLUTION_MS);
            let mut index = 0;
            Self { mmcss: AvSetMmThreadCharacteristicsW(w!("Capture"), &mut index).ok() }
        }
    }
}

impl Drop for Realtime {
    fn drop(&mut self) {
        unsafe {
            if let Some(handle) = self.mmcss.take() {
                let _ = AvRevertMmThreadCharacteristics(handle);
            }
            timeEndPeriod(TIMER_RESOLUTION_MS);
        }
    }
}
