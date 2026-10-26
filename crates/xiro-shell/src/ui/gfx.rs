//! Direct2D + DirectWrite drawing: antialiased shapes, typography and a pixel-exact QR code.
//! Coordinates are DIPs; the target is given the window's DPI.

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_CAP_STYLE_ROUND, D2D1_DASH_STYLE_CUSTOM,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_LINE_JOIN_ROUND, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES,
    D2D1CreateFactory, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1SolidColorBrush, ID2D1StrokeStyle,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE,
    DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP,
    DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, IDWriteTextFormat, IDWriteTextLayout, IDWriteTextLayout1,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::core::{BOOL, HSTRING, Interface};
use windows_numerics::Vector2;

use super::controls::Rect;
use super::theme::{Color, Family, Font};

/// Direct2D asks for a new render target after the display device changed.
const D2DERR_RECREATE_TARGET: i32 = 0x8899_000C_u32 as i32;
const BASE_DPI: f32 = 96.0;
/// Dash and gap lengths, in line widths, of a dashed outline.
const DASHES: [f32; 2] = [2.0, 3.0];

fn fail(what: &'static str) -> impl Fn(windows::core::Error) -> String {
    move |e| format!("{what}: {}", e.message())
}

fn color(c: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r, g: c.g, b: c.b, a: c.a }
}

fn rect(r: Rect) -> D2D_RECT_F {
    D2D_RECT_F { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() }
}

fn rounded(r: Rect, radius: f32) -> D2D1_ROUNDED_RECT {
    let radius = radius.min(r.w / 2.0).min(r.h / 2.0);
    D2D1_ROUNDED_RECT { rect: rect(r), radiusX: radius, radiusY: radius }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Align {
    Leading,
    Center,
}

/// How text sits in its box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Flow {
    pub align: Align,
    /// Vertically centred in the box (else top-aligned).
    pub middle: bool,
    /// Wraps onto more lines (else one line, ending in … when too long).
    pub wrap: bool,
}

pub(crate) const PARAGRAPH: Flow = Flow { align: Align::Leading, middle: false, wrap: true };
pub(crate) const LINE: Flow = Flow { align: Align::Leading, middle: true, wrap: false };
pub(crate) const CENTERED: Flow = Flow { align: Align::Center, middle: true, wrap: false };
pub(crate) const CENTERED_PARAGRAPH: Flow = Flow { align: Align::Center, middle: false, wrap: true };

/// A laid-out piece of text.
pub(crate) struct Text(IDWriteTextLayout);

impl Text {
    /// Width and height of the laid-out text in DIPs.
    pub(crate) fn size(&self) -> (f32, f32) {
        let mut m = DWRITE_TEXT_METRICS::default();
        // SAFETY: `m` is a valid out-parameter.
        let _ = unsafe { self.0.GetMetrics(&mut m) };
        (m.widthIncludingTrailingWhitespace, m.height)
    }
}

/// What makes two text formats the same.
type FormatKey = (Family, u32, i32, Flow);

pub(crate) struct Gfx {
    d2d: ID2D1Factory,
    dw: IDWriteFactory,
    families: [HSTRING; 4],
    formats: std::cell::RefCell<Vec<(FormatKey, IDWriteTextFormat)>>,
}

fn has_family(collection: &IDWriteFontCollection, name: &str) -> bool {
    let (mut index, mut exists) = (0u32, BOOL::default());
    // SAFETY: out-parameters are valid for the call.
    unsafe { collection.FindFamilyName(&HSTRING::from(name), &mut index, &mut exists) }.is_ok() && exists.as_bool()
}

impl Gfx {
    pub(crate) fn new() -> Result<Self, String> {
        // SAFETY: plain factory creation.
        let d2d: ID2D1Factory = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }.map_err(fail("Direct2D"))?;
        // SAFETY: plain factory creation.
        let dw: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.map_err(fail("DirectWrite"))?;
        let mut collection = None;
        // SAFETY: `collection` receives the system font collection.
        unsafe { dw.GetSystemFontCollection(&mut collection, false) }.map_err(fail("Fonts"))?;
        let collection = collection.ok_or("no system fonts")?;
        let families = Family::ALL.map(|family| {
            let [preferred, fallback] = family.candidates();
            HSTRING::from(if has_family(&collection, preferred) { preferred } else { fallback })
        });
        Ok(Self { d2d, dw, families, formats: std::cell::RefCell::new(Vec::new()) })
    }

    fn family(&self, family: Family) -> &HSTRING {
        &self.families[Family::ALL.iter().position(|f| *f == family).unwrap_or(0)]
    }

    pub(crate) fn target(&self, hwnd: HWND, dpi: u32, width_px: u32, height_px: u32) -> Result<Target, String> {
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: dpi as f32,
            dpiY: dpi as f32,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let window = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd,
            pixelSize: D2D_SIZE_U { width: width_px.max(1), height: height_px.max(1) },
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };
        // SAFETY: both property structs outlive the call.
        let rt = unsafe { self.d2d.CreateHwndRenderTarget(&props, &window) }.map_err(fail("Render target"))?;
        // SAFETY: creating a brush on the new target.
        let brush = unsafe { rt.CreateSolidColorBrush(&color(Color::hex(0)), None) }.map_err(fail("Brush"))?;
        let dashes = D2D1_STROKE_STYLE_PROPERTIES {
            startCap: D2D1_CAP_STYLE_ROUND,
            endCap: D2D1_CAP_STYLE_ROUND,
            dashCap: D2D1_CAP_STYLE_ROUND,
            lineJoin: D2D1_LINE_JOIN_ROUND,
            dashStyle: D2D1_DASH_STYLE_CUSTOM,
            ..Default::default()
        };
        // SAFETY: the properties and dash lengths (in line widths) outlive the call.
        let dashed = unsafe { self.d2d.CreateStrokeStyle(&dashes, Some(&DASHES)) }.map_err(fail("Stroke style"))?;
        Ok(Target { rt, brush, dashed, scale: dpi as f32 / BASE_DPI })
    }

    /// Cached text format for `font` and `flow`; painting asks for the same few many times a second.
    fn format(&self, font: Font, flow: Flow) -> Result<IDWriteTextFormat, String> {
        let key = (font.family, font.size.to_bits(), font.weight, flow);
        if let Some((_, format)) = self.formats.borrow().iter().find(|(k, _)| *k == key) {
            return Ok(format.clone());
        }
        // SAFETY: every argument is a valid value or string for the call.
        let format = unsafe {
            self.dw.CreateTextFormat(
                self.family(font.family),
                None,
                DWRITE_FONT_WEIGHT(font.weight),
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                font.size,
                &HSTRING::from("en-us"),
            )
        }
        .map_err(fail("Text format"))?;
        let align = match flow.align {
            Align::Leading => DWRITE_TEXT_ALIGNMENT_LEADING,
            Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
        };
        let paragraph = if flow.middle { DWRITE_PARAGRAPH_ALIGNMENT_CENTER } else { DWRITE_PARAGRAPH_ALIGNMENT_NEAR };
        let wrapping = if flow.wrap { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP };
        // SAFETY: plain setters on the format just created; the trimming sign is built from it.
        unsafe {
            let _ = format.SetTextAlignment(align);
            let _ = format.SetParagraphAlignment(paragraph);
            let _ = format.SetWordWrapping(wrapping);
            if !flow.wrap {
                let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                if let Ok(sign) = self.dw.CreateEllipsisTrimmingSign(&format) {
                    let _ = format.SetTrimming(&trimming, &sign);
                }
            }
        }
        self.formats.borrow_mut().push((key, format.clone()));
        Ok(format)
    }

    /// Lays out `text` in a `width` x `height` box.
    pub(crate) fn text(&self, text: &str, font: Font, width: f32, height: f32, flow: Flow) -> Result<Text, String> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let format = self.format(font, flow)?;
        // SAFETY: `wide` and `format` outlive the call.
        let layout = unsafe { self.dw.CreateTextLayout(&wide, &format, width.max(1.0), height.max(1.0)) }.map_err(fail("Text layout"))?;
        if font.tracking != 0.0 {
            if let Ok(spaced) = layout.cast::<IDWriteTextLayout1>() {
                let all = DWRITE_TEXT_RANGE { startPosition: 0, length: wide.len() as u32 };
                // SAFETY: the range covers the layout's own text.
                let _ = unsafe { spaced.SetCharacterSpacing(0.0, font.tracking, 0.0, all) };
            }
        }
        Ok(Text(layout))
    }
}

/// A window's render target and its one reusable solid brush.
pub(crate) struct Target {
    rt: ID2D1HwndRenderTarget,
    brush: ID2D1SolidColorBrush,
    dashed: ID2D1StrokeStyle,
    /// Physical pixels per DIP.
    scale: f32,
}

impl Target {
    pub(crate) fn resize(&self, width_px: u32, height_px: u32) {
        // SAFETY: resizing our own target.
        let _ = unsafe { self.rt.Resize(&D2D_SIZE_U { width: width_px.max(1), height: height_px.max(1) }) };
    }

    pub(crate) fn set_dpi(&mut self, dpi: u32) {
        self.scale = dpi as f32 / BASE_DPI;
        // SAFETY: plain setter.
        unsafe { self.rt.SetDpi(dpi as f32, dpi as f32) };
    }

    /// Runs one frame. False when the target must be recreated (the GPU changed).
    pub(crate) fn frame(&self, paint: impl FnOnce(&Self)) -> bool {
        // SAFETY: BeginDraw/EndDraw pair around the painting.
        unsafe { self.rt.BeginDraw() };
        paint(self);
        // SAFETY: closes the BeginDraw above.
        let ended = unsafe { self.rt.EndDraw(None, None) };
        !matches!(ended, Err(e) if e.code().0 == D2DERR_RECREATE_TARGET)
    }

    fn solid(&self, c: Color) -> &ID2D1SolidColorBrush {
        // SAFETY: plain setter on our brush.
        unsafe { self.brush.SetColor(&color(c)) };
        &self.brush
    }

    pub(crate) fn clear(&self, c: Color) {
        // SAFETY: called inside a frame.
        unsafe { self.rt.Clear(Some(&color(c))) };
    }

    pub(crate) fn fill(&self, r: Rect, radius: f32, c: Color) {
        // SAFETY: called inside a frame; the brush is ours.
        unsafe { self.rt.FillRoundedRectangle(&rounded(r, radius), self.solid(c)) };
    }

    pub(crate) fn stroke(&self, r: Rect, radius: f32, c: Color, width: f32) {
        let r = r.inset(width / 2.0);
        // SAFETY: as above.
        unsafe { self.rt.DrawRoundedRectangle(&rounded(r, radius), self.solid(c), width, None) };
    }

    /// A dashed outline: a place where something will appear.
    pub(crate) fn dashed(&self, r: Rect, radius: f32, c: Color, width: f32) {
        let r = r.inset(width / 2.0);
        // SAFETY: as above.
        unsafe { self.rt.DrawRoundedRectangle(&rounded(r, radius), self.solid(c), width, &self.dashed) };
    }

    pub(crate) fn circle(&self, cx: f32, cy: f32, radius: f32, c: Color) {
        let e = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: radius, radiusY: radius };
        // SAFETY: as above.
        unsafe { self.rt.FillEllipse(&e, self.solid(c)) };
    }

    pub(crate) fn text(&self, text: &Text, x: f32, y: f32, c: Color) {
        // SAFETY: as above.
        unsafe { self.rt.DrawTextLayout(Vector2 { X: x, Y: y }, &text.0, self.solid(c), D2D1_DRAW_TEXT_OPTIONS_NONE) };
    }

    /// The QR code centred in `square` with whole-pixel modules so cameras lock on quickly.
    /// `modules` excludes the quiet zone (the caller's card provides it).
    pub(crate) fn qr(&self, modules: &[Vec<bool>], square: Rect, c: Color) {
        let n = modules.len();
        if n == 0 {
            return;
        }
        let module_px = ((square.w.min(square.h) * self.scale) / n as f32).floor().max(1.0);
        let side = module_px * n as f32 / self.scale;
        let snap = |v: f32| (v * self.scale).round() / self.scale;
        let (x0, y0) = (snap(square.x + (square.w - side) / 2.0), snap(square.y + (square.h - side) / 2.0));
        let m = module_px / self.scale;
        let brush = self.solid(c);
        // SAFETY: drawn inside a frame; aliased so neighbouring modules meet without seams.
        unsafe {
            self.rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
            for (y, row) in modules.iter().enumerate() {
                for (x, _) in row.iter().enumerate().filter(|(_, dark)| **dark) {
                    let (left, top) = (x0 + x as f32 * m, y0 + y as f32 * m);
                    let cell = D2D_RECT_F { left, top, right: left + m, bottom: top + m };
                    self.rt.FillRectangle(&cell, brush);
                }
            }
            self.rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        }
    }
}
