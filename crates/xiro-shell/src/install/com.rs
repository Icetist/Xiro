//! COM for one operation. RPC_E_CHANGED_MODE (thread already in the other apartment) is fine
//! for our in-process objects and just means "do not uninitialise".

use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize};

pub(crate) struct ComScope {
    owns: bool,
}

impl ComScope {
    pub(crate) fn enter() -> Result<Self, String> {
        // SAFETY: balanced by CoUninitialize in Drop when this call initialised COM.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        if hr == RPC_E_CHANGED_MODE {
            return Ok(Self { owns: false });
        }
        hr.ok().map_err(|e| format!("COM is unavailable: {}", e.message()))?;
        Ok(Self { owns: true }) // S_OK and S_FALSE both need a matching uninitialise
    }
}

impl Drop for ComScope {
    fn drop(&mut self) {
        if self.owns {
            // SAFETY: pairs with the successful CoInitializeEx in `enter` on this thread.
            unsafe { CoUninitialize() };
        }
    }
}
