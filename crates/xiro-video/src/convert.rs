//! GPU colour conversion (and scaling) from the desktop's BGRA to the NV12 every
//! H.264 encoder takes, using the fixed-function D3D11 video processor.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BIND_VIDEO_ENCODER, D3D11_SUBRESOURCE_DATA, D3D11_VIDEO_PROCESSOR_CAPS, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_COLOR_SPACE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11Device,
    ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
    ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::core::{Interface, Result};

use super::capture::Pointer;
use super::cursor::CursorImage;
use super::gpu::Gpu;

/// Output textures in flight: one being encoded, one being written, one spare.
const RING: usize = 3;
/// Colour space bitfield: Usage(1) RGB_Range(1) YCbCr_Matrix(1) YCbCr_xvYCC(1) Nominal_Range(2).
const RGB_FULL_RANGE: u32 = 0;
const YCBCR_709_STUDIO: u32 = (1 << 2) | (1 << 4);

struct Output {
    texture: ID3D11Texture2D,
    view: ID3D11VideoProcessorOutputView,
}

struct CursorLayer {
    version: u64,
    _texture: ID3D11Texture2D,
    view: ID3D11VideoProcessorInputView,
    width: u32,
    height: u32,
}

pub struct Converter {
    device: ID3D11Device,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    outputs: Vec<Output>,
    next: usize,
    input: Option<(usize, ID3D11VideoProcessorInputView)>,
    /// The driver can blend a second stream (the pointer) on top.
    overlay: bool,
    cursor: Option<CursorLayer>,
    source: (u32, u32),
    pub width: u32,
    pub height: u32,
}

/// NV12 needs even dimensions.
pub fn even(n: u32) -> u32 {
    (n & !1).max(2)
}

impl Converter {
    pub fn new(gpu: &Gpu, src: (u32, u32), dst: (u32, u32), fps: u32) -> Result<Self> {
        let (width, height) = (even(dst.0), even(dst.1));
        unsafe {
            let video_device: ID3D11VideoDevice = gpu.device.cast()?;
            let video_context: ID3D11VideoContext = gpu.context.cast()?;
            let rate = DXGI_RATIONAL { Numerator: fps, Denominator: 1 };
            let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: rate,
                InputWidth: src.0,
                InputHeight: src.1,
                OutputFrameRate: rate,
                OutputWidth: width,
                OutputHeight: height,
                Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
            };
            let enumerator = video_device.CreateVideoProcessorEnumerator(&desc)?;
            let processor = video_device.CreateVideoProcessor(&enumerator, 0)?;
            let mut caps = D3D11_VIDEO_PROCESSOR_CAPS::default();
            let overlay = enumerator.GetVideoProcessorCaps(&mut caps).is_ok() && caps.MaxInputStreams >= 2;
            if overlay {
                // Stream 1 is the pointer: sRGB with its own alpha, never "enhanced".
                video_context.VideoProcessorSetStreamColorSpace(&processor, 1, &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: RGB_FULL_RANGE });
                video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 1, false);
                video_context.VideoProcessorSetStreamFrameFormat(&processor, 1, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
                video_context.VideoProcessorSetStreamAlpha(&processor, 1, true, 1.0);
            }
            video_context.VideoProcessorSetStreamColorSpace(&processor, 0, &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: RGB_FULL_RANGE });
            video_context.VideoProcessorSetOutputColorSpace(&processor, &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: YCBCR_709_STUDIO });
            // No denoise / edge enhancement guesses: text must stay crisp.
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            let full = RECT { left: 0, top: 0, right: src.0 as i32, bottom: src.1 as i32 };
            video_context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&full));
            let out = RECT { left: 0, top: 0, right: width as i32, bottom: height as i32 };
            video_context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&out));
            video_context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&out));

            let mut converter = Self {
                device: gpu.device.clone(),
                video_device,
                video_context,
                enumerator,
                processor,
                outputs: Vec::with_capacity(RING),
                next: 0,
                input: None,
                overlay,
                cursor: None,
                source: src,
                width,
                height,
            };
            for _ in 0..RING {
                let output = converter.create_output()?;
                converter.outputs.push(output);
            }
            Ok(converter)
        }
    }

    fn create_output(&self) -> Result<Output> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: self.width,
            Height: self.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_VIDEO_ENCODER.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture = None;
        unsafe {
            // Some drivers reject the encoder bind flag; render target alone works everywhere.
            if self.device.CreateTexture2D(&desc, None, Some(&mut texture)).is_err() {
                let plain = D3D11_TEXTURE2D_DESC { BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32, ..desc };
                self.device.CreateTexture2D(&plain, None, Some(&mut texture))?;
            }
            let texture = texture.expect("CreateTexture2D returned no texture");
            let view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
            };
            let mut view = None;
            self.video_device.CreateVideoProcessorOutputView(&texture, &self.enumerator, &view_desc, Some(&mut view))?;
            Ok(Output { texture, view: view.expect("no output view") })
        }
    }

    fn input_view(&mut self, src: &ID3D11Texture2D) -> Result<ID3D11VideoProcessorInputView> {
        let key = src.as_raw() as usize;
        if let Some((cached, view)) = &self.input {
            if *cached == key {
                return Ok(view.clone());
            }
        }
        let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
        };
        let mut view = None;
        unsafe { self.video_device.CreateVideoProcessorInputView(src, &self.enumerator, &desc, Some(&mut view))? };
        let view = view.expect("no input view");
        self.input = Some((key, view.clone()));
        Ok(view)
    }

    /// The pointer's texture, rebuilt only when its shape changes.
    fn cursor_view(&mut self, image: &CursorImage, version: u64) -> Result<(ID3D11VideoProcessorInputView, u32, u32)> {
        if let Some(layer) = self.cursor.as_ref().filter(|l| l.version == version) {
            return Ok((layer.view.clone(), layer.width, layer.height));
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: image.width,
            Height: image.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let data = D3D11_SUBRESOURCE_DATA { pSysMem: image.bgra.as_ptr().cast(), SysMemPitch: image.width * 4, SysMemSlicePitch: 0 };
        let mut texture = None;
        unsafe { self.device.CreateTexture2D(&desc, Some(&data), Some(&mut texture))? };
        let texture = texture.expect("CreateTexture2D returned no texture");
        let view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
        };
        let mut view = None;
        unsafe { self.video_device.CreateVideoProcessorInputView(&texture, &self.enumerator, &view_desc, Some(&mut view))? };
        let view = view.expect("no input view");
        self.cursor = Some(CursorLayer { version, _texture: texture, view: view.clone(), width: image.width, height: image.height });
        Ok((view, image.width, image.height))
    }

    /// Places the pointer layer (desktop pixels scaled to the output); None when it can't be shown.
    fn pointer_stream(&mut self, pointer: Option<&Pointer>) -> Option<ID3D11VideoProcessorInputView> {
        let pointer = pointer.filter(|p| self.overlay && p.state.visible)?;
        let (view, w, h) = self.cursor_view(pointer.image?, pointer.version).ok()?;
        let (source, dest) = place_pointer((pointer.state.x, pointer.state.y), (w, h), self.source, (self.width, self.height))?;
        unsafe {
            self.video_context.VideoProcessorSetStreamSourceRect(&self.processor, 1, true, Some(&source));
            self.video_context.VideoProcessorSetStreamDestRect(&self.processor, 1, true, Some(&dest));
        }
        Some(view)
    }

    /// Converts `src` (with the pointer on top, when visible) into the next NV12 texture of the ring.
    pub fn convert(&mut self, src: &ID3D11Texture2D, pointer: Option<&Pointer>) -> Result<ID3D11Texture2D> {
        let input = self.input_view(src)?;
        let cursor = self.pointer_stream(pointer);
        let output = &self.outputs[self.next];
        self.next = (self.next + 1) % self.outputs.len();
        let stream = |view| D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: std::mem::ManuallyDrop::new(Some(view)),
            ..Default::default()
        };
        let blt = |views: Vec<ID3D11VideoProcessorInputView>| {
            let mut streams: Vec<D3D11_VIDEO_PROCESSOR_STREAM> = views.into_iter().map(stream).collect();
            let result = unsafe { self.video_context.VideoProcessorBlt(&self.processor, &output.view, 0, &streams) };
            // The stream structs hold references to the input views; give them back.
            for s in &mut streams {
                unsafe { std::mem::ManuallyDrop::drop(&mut s.pInputSurface) };
            }
            result
        };
        let with_pointer = cursor.is_some();
        let result = blt(std::iter::once(input.clone()).chain(cursor).collect());
        // A driver that rejects the pointer layer still gets the picture: the pointer is optional.
        let result = if result.is_err() && with_pointer { blt(vec![input]) } else { result };
        result.map(|()| output.texture.clone())
    }
}

/// The visible part of the pointer: the source rectangle within its image and the
/// destination in the output (desktop pixels scaled to the stream). None when it is off-screen.
pub fn place_pointer(at: (i32, i32), size: (u32, u32), desktop: (u32, u32), output: (u32, u32)) -> Option<(RECT, RECT)> {
    let (x, y, w, h) = (at.0, at.1, size.0 as i32, size.1 as i32);
    let (left, top) = (x.max(0), y.max(0));
    let (right, bottom) = ((x + w).min(desktop.0 as i32), (y + h).min(desktop.1 as i32));
    if left >= right || top >= bottom {
        return None;
    }
    let scale = |v: i32, from: u32, to: u32| (v as i64 * to as i64 / from.max(1) as i64) as i32;
    let source = RECT { left: left - x, top: top - y, right: right - x, bottom: bottom - y };
    let dest = RECT {
        left: scale(left, desktop.0, output.0),
        top: scale(top, desktop.1, output.1),
        right: scale(right, desktop.0, output.0),
        bottom: scale(bottom, desktop.1, output.1),
    };
    (dest.left < dest.right && dest.top < dest.bottom).then_some((source, dest))
}

#[cfg(test)]
mod tests {
    use super::{RECT, even, place_pointer};

    #[test]
    fn pointer_is_clipped_to_the_screen() {
        let r = |l, t, rr, b| RECT { left: l, top: t, right: rr, bottom: b };
        assert_eq!(place_pointer((10, 20), (32, 32), (1000, 800), (1000, 800)), Some((r(0, 0, 32, 32), r(10, 20, 42, 52))));
        assert_eq!(place_pointer((-5, 790), (32, 32), (1000, 800), (500, 400)), Some((r(5, 0, 32, 10), r(0, 395, 13, 400))));
        assert_eq!(place_pointer((1000, 10), (32, 32), (1000, 800), (1000, 800)), None);
        assert_eq!(place_pointer((-40, 10), (32, 32), (1000, 800), (1000, 800)), None);
    }

    #[test]
    fn rounds_down_to_even_dimensions() {
        assert_eq!(even(1181), 1180);
        assert_eq!(even(820), 820);
        assert_eq!(even(1), 2);
    }
}
