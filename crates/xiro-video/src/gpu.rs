//! The Direct3D 11 device that captures, converts and encodes, created on the GPU
//! that drives the streamed monitor so frames never leave video memory.

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11Multithread,
};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1, IDXGIOutput1};
use windows::core::{Interface, Result};

/// Highest GPU scheduling priority a normal process may ask for: keeps the stream smooth while a game loads the GPU.
const GPU_THREAD_PRIORITY: i32 = 7;

pub struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub adapter_name: String,
}

pub fn wide_to_string(wide: &[u16]) -> String {
    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..end])
}

/// The adapter and output that show a monitor, by GDI name (e.g. `\\.\DISPLAY7`).
pub fn find_output(gdi_name: &str) -> Result<Option<(IDXGIAdapter1, IDXGIOutput1)>> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                let desc = output.GetDesc()?;
                if wide_to_string(&desc.DeviceName).eq_ignore_ascii_case(gdi_name) {
                    return Ok(Some((adapter, output.cast()?)));
                }
                o += 1;
            }
            a += 1;
        }
        Ok(None)
    }
}

/// GDI names of every monitor Desktop Duplication can capture.
pub fn capturable_outputs() -> Result<Vec<String>> {
    let mut names = Vec::new();
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                names.push(wide_to_string(&output.GetDesc()?.DeviceName));
                o += 1;
            }
            a += 1;
        }
    }
    Ok(names)
}

pub fn create_device(adapter: &IDXGIAdapter1) -> Result<Gpu> {
    let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0];
    let mut device = None;
    let mut context = None;
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
        let device: ID3D11Device = device.expect("D3D11CreateDevice returned no device");
        // The encoder's worker threads use the same device.
        if let Ok(mt) = device.cast::<ID3D11Multithread>() {
            let _ = mt.SetMultithreadProtected(true);
        }
        if let Ok(dxgi) = device.cast::<IDXGIDevice>() {
            let _ = dxgi.SetGPUThreadPriority(GPU_THREAD_PRIORITY);
        }
        let adapter_name = wide_to_string(&adapter.GetDesc1()?.Description);
        Ok(Gpu { device, context: context.expect("D3D11CreateDevice returned no context"), adapter_name })
    }
}

#[cfg(test)]
mod tests {
    use super::wide_to_string;

    #[test]
    fn wide_strings_stop_at_the_first_nul() {
        let mut buf = [0u16; 8];
        for (i, c) in "AB".encode_utf16().enumerate() {
            buf[i] = c;
        }
        buf[3] = 'x' as u16;
        assert_eq!(wide_to_string(&buf), "AB");
        assert_eq!(wide_to_string(&['Z' as u16]), "Z");
    }
}
