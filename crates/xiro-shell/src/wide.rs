//! UTF-16 helpers: NUL-terminated strings and fixed `[u16; N]` fields truncated safely.

/// `text` as a NUL-terminated UTF-16 buffer for `PCWSTR` arguments.
pub(crate) fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Copies into a fixed Win32 field, leaving room for NUL and never splitting a surrogate pair.
pub(crate) fn copy_truncated(dst: &mut [u16], text: &str) {
    let Some(capacity) = dst.len().checked_sub(1) else { return };
    let mut len = 0;
    for ch in text.chars() {
        let mut units = [0u16; 2];
        let encoded = ch.encode_utf16(&mut units);
        if len + encoded.len() > capacity {
            break;
        }
        dst[len..len + encoded.len()].copy_from_slice(encoded);
        len += encoded.len();
    }
    dst[len..].fill(0);
}

/// Text from a buffer up to its first NUL (or the whole buffer).
#[cfg(test)]
pub(crate) fn from_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_strings_are_nul_terminated() {
        assert_eq!(to_wide("ab"), vec![u16::from(b'a'), u16::from(b'b'), 0]);
        assert_eq!(from_wide(&to_wide("Xiro · 60 fps")), "Xiro · 60 fps");
    }

    #[test]
    fn truncation_keeps_the_nul_and_whole_characters() {
        let mut field = [0xFFFFu16; 4];
        copy_truncated(&mut field, "abcdef");
        assert_eq!(from_wide(&field), "abc");
        assert_eq!(field[3], 0);

        // "a" + an astral emoji (2 units) + "b": the emoji does not fit after "a" in 3 slots.
        let mut small = [0xFFFFu16; 3];
        copy_truncated(&mut small, "a\u{1F600}b");
        assert_eq!(from_wide(&small), "a");

        let mut empty: [u16; 0] = [];
        copy_truncated(&mut empty, "x");
    }
}
