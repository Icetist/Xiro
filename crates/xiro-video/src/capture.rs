//! Desktop Duplication. Frames are converted straight from Windows' surface (a copy costs
//! as much bandwidth as the conversion) and released at once so Windows is never held up.

use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTDUPL_POINTER_SHAPE_INFO, IDXGIOutput1,
    IDXGIOutputDuplication,
};
use windows::core::{Error, Interface, Result};

use super::cursor::{CursorImage, CursorState, to_bgra};
use super::gpu::Gpu;

pub struct Pointer<'a> {
    pub state: CursorState,
    pub image: Option<&'a CursorImage>,
    /// Changes whenever the shape does, so its texture is only rebuilt then.
    pub version: u64,
}

pub struct Duplication {
    duplication: IDXGIOutputDuplication,
    /// The first frame of a duplication is the whole desktop, whatever its present time says.
    seen_first: bool,
    pointer: CursorState,
    shape: Option<CursorImage>,
    shape_version: u64,
    shape_buffer: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// The duplication must be recreated: display mode changed, secure desktop (UAC), or the monitor went away.
pub fn is_access_lost(err: &Error) -> bool {
    err.code() == DXGI_ERROR_ACCESS_LOST
}

impl Duplication {
    pub fn new(gpu: &Gpu, output: &IDXGIOutput1) -> Result<Self> {
        unsafe {
            let duplication = output.DuplicateOutput(&gpu.device)?;
            let desc = duplication.GetDesc();
            Ok(Self {
                duplication,
                seen_first: false,
                pointer: CursorState::default(),
                shape: None,
                shape_version: 0,
                shape_buffer: Vec::new(),
                width: desc.ModeDesc.Width,
                height: desc.ModeDesc.Height,
            })
        }
    }

    /// True when the pointer moved or changed shape.
    fn update_pointer(&mut self, info: &DXGI_OUTDUPL_FRAME_INFO) -> bool {
        let mut changed = false;
        if info.LastMouseUpdateTime != 0 {
            let p = info.PointerPosition;
            let next = CursorState { x: p.Position.x, y: p.Position.y, visible: p.Visible.as_bool() };
            changed |= next != self.pointer;
            self.pointer = next;
        }
        if info.PointerShapeBufferSize > 0 {
            self.shape_buffer.resize(info.PointerShapeBufferSize as usize, 0);
            let mut required = 0;
            let mut shape = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
            let fetched = unsafe {
                self.duplication.GetFramePointerShape(info.PointerShapeBufferSize, self.shape_buffer.as_mut_ptr().cast(), &mut required, &mut shape)
            };
            if fetched.is_ok() {
                self.shape = to_bgra(shape.Type, shape.Width, shape.Height, shape.Pitch, &self.shape_buffer);
                self.shape_version += 1;
                changed = true;
            }
        }
        changed
    }

    /// Waits up to `timeout_ms` for the desktop or the pointer to change and hands the
    /// image to `use_frame` while it is valid. True when something changed.
    pub fn grab(&mut self, timeout_ms: u32, use_frame: impl FnOnce(&ID3D11Texture2D, Pointer) -> Result<()>) -> Result<bool> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        unsafe {
            match self.duplication.AcquireNextFrame(timeout_ms, &mut info, &mut resource) {
                Ok(()) => {}
                Err(err) if err.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(false),
                Err(err) => return Err(err),
            }
        }
        // LastPresentTime is zero when only the pointer changed.
        let desktop = info.LastPresentTime != 0 || !self.seen_first;
        let pointer = self.update_pointer(&info);
        let result = match resource.filter(|_| desktop || pointer) {
            Some(resource) => resource.cast::<ID3D11Texture2D>().and_then(|image| {
                use_frame(&image, Pointer { state: self.pointer, image: self.shape.as_ref(), version: self.shape_version })
            }),
            None => Ok(()),
        };
        // Always hand the frame back, even if using it failed.
        let released = unsafe { self.duplication.ReleaseFrame() };
        result?;
        released?;
        self.seen_first |= desktop;
        Ok(desktop || pointer)
    }
}
