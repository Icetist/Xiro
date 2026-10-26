//! The Xiro logo as window and tray icon, scaled from pixels made by `scripts/make-icons.py`.

use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject, HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, DestroyIcon, HICON, ICONINFO};

/// Straight-alpha BGRA, top row first.
const LARGE: (&[u8], usize) = (include_bytes!("../assets/logo-256.bgra"), 256);
const SMALL: (&[u8], usize) = (include_bytes!("../assets/logo-32.bgra"), 32);

/// `size`x`size` straight-alpha BGRA, top-down; each pixel averages the source pixels it covers.
pub(crate) fn logo_pixels(size: usize) -> Vec<u32> {
    let size = size.max(1);
    let (bytes, side) = if size <= SMALL.1 { SMALL } else { LARGE };
    let mut pixels = Vec::with_capacity(size * size);
    for py in 0..size {
        let (y0, y1) = (py * side / size, ((py + 1) * side).div_ceil(size).max(py * side / size + 1));
        for px in 0..size {
            let (x0, x1) = (px * side / size, ((px + 1) * side).div_ceil(size).max(px * side / size + 1));
            let mut sum = [0u64; 4];
            for y in y0..y1.min(side) {
                for x in x0..x1.min(side) {
                    let at = (y * side + x) * 4;
                    let a = u64::from(bytes[at + 3]);
                    for c in 0..3 {
                        sum[c] += u64::from(bytes[at + c]) * a;
                    }
                    sum[3] += a;
                }
            }
            pixels.push(pack(sum, ((y1.min(side) - y0) * (x1.min(side) - x0)) as u64));
        }
    }
    pixels
}

/// Averages colour weighted by alpha (so transparent edges don't darken it) and alpha itself.
fn pack(sum: [u64; 4], count: u64) -> u32 {
    if sum[3] == 0 || count == 0 {
        return 0;
    }
    let channel = |c: usize| ((sum[c] + sum[3] / 2) / sum[3]) as u32;
    let alpha = ((sum[3] + count / 2) / count) as u32;
    (alpha << 24) | (channel(2) << 16) | (channel(1) << 8) | channel(0)
}

/// An HICON destroyed on drop.
pub(crate) struct OwnedIcon(pub(crate) HICON);

impl Drop for OwnedIcon {
    fn drop(&mut self) {
        // SAFETY: the icon is owned exclusively by this value.
        let _ = unsafe { DestroyIcon(self.0) };
    }
}

pub(crate) fn create_icon(size: u32) -> Result<OwnedIcon, String> {
    let side = size.clamp(1, 256) as usize;
    let pixels = logo_pixels(side);
    // SAFETY: `pixels` holds exactly side*side values.
    unsafe { icon_from_bgra(&pixels, side as i32) }.map(OwnedIcon)
}

/// Builds an alpha icon from a top-down 32-bit DIB plus an all-zero mask.
/// Safety: `pixels` must contain at least `side * side` values.
unsafe fn icon_from_bgra(pixels: &[u32], side: i32) -> Result<HICON, String> {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: side,
            biHeight: -side, // negative: top-down rows, matching `logo_pixels`
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    // SAFETY: `info` describes a valid 32-bit DIB; `bits` receives the section's pixel pointer.
    let color = unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) }.map_err(|e| e.message())?;
    if bits.is_null() {
        // SAFETY: `color` was just created and is not selected anywhere.
        let _ = unsafe { DeleteObject(HGDIOBJ(color.0)) };
        return Err("CreateDIBSection returned no pixels".into());
    }
    let count = (side * side) as usize;
    // SAFETY: the section holds `count` 32-bit pixels and `pixels` has at least as many.
    unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u32>(), count) };
    let mask_bits = vec![0u8; (side as usize).div_ceil(16) * 2 * side as usize];
    // SAFETY: an all-zero monochrome bitmap with word-aligned rows.
    let mask = unsafe { CreateBitmap(side, side, 1, 1, Some(mask_bits.as_ptr().cast())) };
    let icon_info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
    // SAFETY: both bitmaps are valid; CreateIconIndirect copies them.
    let icon = unsafe { CreateIconIndirect(&icon_info) }.map_err(|e| e.message());
    // SAFETY: the bitmaps are no longer needed once the icon owns its copies.
    unsafe {
        let _ = DeleteObject(HGDIOBJ(mask.0));
        let _ = DeleteObject(HGDIOBJ(color.0));
    }
    icon
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(p: u32) -> u32 {
        p >> 24
    }

    #[test]
    fn the_embedded_pixels_are_whole_images() {
        assert_eq!(LARGE.0.len(), LARGE.1 * LARGE.1 * 4);
        assert_eq!(SMALL.0.len(), SMALL.1 * SMALL.1 * 4);
    }

    #[test]
    fn corners_are_transparent_and_the_tile_is_opaque() {
        for size in [16, 20, 24, 32, 48, 256] {
            let pixels = logo_pixels(size);
            assert_eq!(pixels.len(), size * size);
            assert!(alpha(pixels[0]) < 128, "{size}: rounded corner");
            assert!(alpha(pixels[size * size - 1]) < 128);
            assert_eq!(alpha(pixels[size / 2 * size + 1]), 255, "{size}: left edge, mid height");
        }
    }

    #[test]
    fn the_white_mark_shows_even_at_tray_size() {
        let pixels = logo_pixels(16);
        let bright = pixels.iter().filter(|&&p| alpha(p) == 255 && (p & 0xFF) > 0xa0).count();
        assert!(bright >= 4, "{bright} bright pixels");
        let centre = pixels[8 * 16 + 8] & 0xFF;
        assert!(centre > 0x80, "the spark in the middle");
    }
}
