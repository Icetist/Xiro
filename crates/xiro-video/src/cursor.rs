//! The mouse pointer: Desktop Duplication leaves it out of the image, so its shape is
//! turned into a small BGRA image that the video processor blends on top.

/// DXGI_OUTDUPL_POINTER_SHAPE_TYPE values.
pub const SHAPE_MONOCHROME: u32 = 1;
pub const SHAPE_COLOR: u32 = 2;
pub const SHAPE_MASKED_COLOR: u32 = 4;
const OPAQUE_BLACK: [u8; 4] = [0, 0, 0, 255];
const OPAQUE_WHITE: [u8; 4] = [255, 255, 255, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorImage {
    pub width: u32,
    pub height: u32,
    /// BGRA, straight (not premultiplied) alpha, tightly packed rows.
    pub bgra: Vec<u8>,
}

/// Where the pointer is drawn: top-left of its image in desktop pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CursorState {
    pub x: i32,
    pub y: i32,
    pub visible: bool,
}

fn bit(row: &[u8], x: usize) -> bool {
    row.get(x / 8).is_some_and(|b| b & (0x80 >> (x % 8)) != 0)
}

/// Screen-inverting pixels (the text I-beam) can't be expressed as alpha; they are drawn
/// black, which reads well on the light backgrounds where they usually appear.
pub fn to_bgra(kind: u32, width: u32, height: u32, pitch: u32, data: &[u8]) -> Option<CursorImage> {
    let (w, pitch) = (width as usize, pitch as usize);
    let rows = if kind == SHAPE_MONOCHROME { height as usize / 2 } else { height as usize };
    if w == 0 || rows == 0 || data.len() < pitch * height as usize {
        return None;
    }
    let mut bgra = Vec::with_capacity(w * rows * 4);
    for y in 0..rows {
        for x in 0..w {
            let pixel = match kind {
                SHAPE_MONOCHROME => {
                    let and = bit(&data[y * pitch..], x);
                    let xor = bit(&data[(y + rows) * pitch..], x);
                    match (and, xor) {
                        (true, false) => CLEAR,
                        (false, false) => OPAQUE_BLACK,
                        (false, true) => OPAQUE_WHITE,
                        (true, true) => OPAQUE_BLACK,
                    }
                }
                SHAPE_COLOR => {
                    let p = &data[y * pitch + x * 4..y * pitch + x * 4 + 4];
                    [p[0], p[1], p[2], p[3]]
                }
                SHAPE_MASKED_COLOR => {
                    let p = &data[y * pitch + x * 4..y * pitch + x * 4 + 4];
                    // Mask 0: replace the screen with the colour. Mask 0xFF: XOR it (invisible when black).
                    match (p[3], [p[0], p[1], p[2]]) {
                        (0, rgb) => [rgb[0], rgb[1], rgb[2], 255],
                        (_, [0, 0, 0]) => CLEAR,
                        _ => OPAQUE_BLACK,
                    }
                }
                _ => return None,
            };
            bgra.extend_from_slice(&pixel);
        }
    }
    Some(CursorImage { width, height: rows as u32, bgra })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monochrome_masks_become_alpha() {
        // 2x1 pointer: AND row then XOR row, one byte each (pitch 1).
        // x0: AND=1 XOR=0 -> clear; x1: AND=0 XOR=1 -> white.
        let data = [0b1000_0000, 0b0100_0000];
        let img = to_bgra(SHAPE_MONOCHROME, 2, 2, 1, &data).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(&img.bgra[..4], &CLEAR);
        assert_eq!(&img.bgra[4..], &OPAQUE_WHITE);
    }

    #[test]
    fn colour_shapes_keep_their_alpha() {
        let data = [1, 2, 3, 128, 9, 9, 9, 0];
        let img = to_bgra(SHAPE_COLOR, 2, 1, 8, &data).unwrap();
        assert_eq!(img.bgra, data.to_vec());
    }

    #[test]
    fn masked_colour_replaces_or_inverts() {
        let data = [10, 20, 30, 0, 0, 0, 0, 255, 5, 5, 5, 255];
        let img = to_bgra(SHAPE_MASKED_COLOR, 3, 1, 12, &data).unwrap();
        assert_eq!(&img.bgra[..4], &[10, 20, 30, 255]);
        assert_eq!(&img.bgra[4..8], &CLEAR);
        assert_eq!(&img.bgra[8..], &OPAQUE_BLACK);
    }

    #[test]
    fn rejects_truncated_or_unknown_shapes() {
        assert!(to_bgra(SHAPE_COLOR, 4, 4, 16, &[0; 10]).is_none());
        assert!(to_bgra(9, 1, 1, 4, &[0; 4]).is_none());
        assert!(to_bgra(SHAPE_MONOCHROME, 8, 1, 1, &[0; 1]).is_none());
    }
}
