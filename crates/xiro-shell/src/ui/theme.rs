//! Palette (dark and light, following the Windows app theme) and type ramp.
//! Neutral greys; colour only signals state (connected, destructive).

use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::core::w;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub(crate) const fn hex(rgb: u32) -> Self {
        Self {
            r: ((rgb >> 16) & 0xff) as f32 / 255.0,
            g: ((rgb >> 8) & 0xff) as f32 / 255.0,
            b: (rgb & 0xff) as f32 / 255.0,
            a: 1.0,
        }
    }

    pub(crate) const fn alpha(self, a: f32) -> Self {
        Self { a: self.a * a, ..self }
    }

    /// `t` = 0 is `self`, 1 is `other`.
    pub(crate) fn mix(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let lerp = |a: f32, b: f32| a + (b - a) * t;
        Self { r: lerp(self.r, other.r), g: lerp(self.g, other.g), b: lerp(self.b, other.b), a: lerp(self.a, other.a) }
    }

    /// Win32 COLORREF (0x00BBGGRR), for the title bar.
    pub(crate) fn colorref(self) -> u32 {
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        byte(self.r) | byte(self.g) << 8 | byte(self.b) << 16
    }
}

pub(crate) struct Palette {
    pub dark: bool,
    /// The window's background.
    pub ink: Color,
    pub surface: Color,
    pub raised: Color,
    pub hairline: Color,
    /// Outlines that must read as edges (the screens in the diagram).
    pub edge: Color,
    pub text: Color,
    pub secondary: Color,
    pub tertiary: Color,
    pub success: Color,
    pub danger: Color,
}

const WHITE: Color = Color::hex(0xFFFFFF);
const BLACK: Color = Color::hex(0x000000);

/// Neutral greys, no tint: the colour in this window is the person's own screen content.
pub(crate) const DARK: Palette = Palette {
    dark: true,
    ink: Color::hex(0x0E0E0F),
    surface: Color::hex(0x161617),
    raised: Color::hex(0x1F1F21),
    hairline: WHITE.alpha(0.08),
    edge: WHITE.alpha(0.22),
    text: Color::hex(0xEDEDEE),
    secondary: Color::hex(0xA0A0A5),
    tertiary: Color::hex(0x6B6B70),
    success: Color::hex(0x5BD08D),
    danger: Color::hex(0xF2555A),
};

pub(crate) const LIGHT: Palette = Palette {
    dark: false,
    ink: Color::hex(0xF6F6F4),
    surface: Color::hex(0xFFFFFF),
    raised: Color::hex(0xECECE9),
    hairline: BLACK.alpha(0.09),
    edge: BLACK.alpha(0.26),
    text: Color::hex(0x121213),
    secondary: Color::hex(0x5C5C61),
    tertiary: Color::hex(0x8D8D92),
    success: Color::hex(0x1E9E5A),
    danger: Color::hex(0xD93036),
};

/// Follows Settings → Personalization → Colors → "Choose your app mode".
pub(crate) fn current() -> &'static Palette {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: reads one DWORD into `value`; both pointers outlive the call.
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    if rc.is_ok() && value == 1 { &LIGHT } else { &DARK }
}

/// The font families Xiro draws with; each falls back to an older Windows font.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Family {
    Display,
    Text,
    Mono,
    Icons,
}

impl Family {
    pub(crate) const ALL: [Family; 4] = [Family::Display, Family::Text, Family::Mono, Family::Icons];

    /// Preferred first.
    pub(crate) const fn candidates(self) -> [&'static str; 2] {
        match self {
            Self::Display => ["Segoe UI Variable Display", "Segoe UI"],
            Self::Text => ["Segoe UI Variable Text", "Segoe UI"],
            Self::Mono => ["Cascadia Mono", "Consolas"],
            Self::Icons => ["Segoe Fluent Icons", "Segoe MDL2 Assets"],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Font {
    pub family: Family,
    pub size: f32,
    pub weight: i32,
    /// Extra space per character in DIPs: negative tightens display type, positive opens labels.
    pub tracking: f32,
}

pub(crate) const TITLE: Font = Font { family: Family::Display, size: 20.0, weight: 600, tracking: -0.2 };
pub(crate) const HEADING: Font = Font { family: Family::Display, size: 15.0, weight: 600, tracking: -0.1 };
pub(crate) const BODY: Font = Font { family: Family::Text, size: 13.5, weight: 400, tracking: 0.0 };
pub(crate) const STRONG: Font = Font { family: Family::Text, size: 13.5, weight: 600, tracking: 0.0 };
pub(crate) const SMALL: Font = Font { family: Family::Text, size: 12.0, weight: 400, tracking: 0.0 };
pub(crate) const MONO: Font = Font { family: Family::Mono, size: 12.0, weight: 400, tracking: 0.0 };
/// Big numbers: the device's resolution once it is connected.
pub(crate) const FIGURE: Font = Font { family: Family::Display, size: 22.0, weight: 600, tracking: -0.4 };
pub(crate) const WORDMARK: Font = Font { family: Family::Display, size: 20.0, weight: 700, tracking: -0.4 };
pub(crate) const ICON: Font = Font { family: Family::Icons, size: 14.0, weight: 400, tracking: 0.0 };

/// Segoe Fluent Icons code points (the same in Segoe MDL2 Assets).
pub(crate) mod glyph {
    pub const COPY: char = '\u{E8C8}';
    pub const CHECK: char = '\u{E73E}';
    pub const UNLINK: char = '\u{F3B1}';
    pub const DELETE: char = '\u{E74D}';
    pub const SHIELD: char = '\u{EA18}';
    pub const BACK: char = '\u{E72B}';
    pub const DEVICE: char = '\u{E8EA}';
}

pub(crate) mod radius {
    pub const CONTROL: f32 = 7.0;
    pub const SCREEN: f32 = 10.0;
    pub const TABLET: f32 = 18.0;
    pub const PILL: f32 = 999.0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colours_and_colorrefs_round_trip() {
        let c = Color::hex(0x7DE3FF);
        assert!((c.r - 0x7D as f32 / 255.0).abs() < 1e-6);
        assert_eq!(c.colorref(), 0x00FF_E37D);
        assert_eq!(c.alpha(0.5).a, 0.5);
    }

    #[test]
    fn both_palettes_keep_text_readable() {
        // Rough luminance contrast: text far from the background in both themes.
        let lum = |c: Color| 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
        for p in [&DARK, &LIGHT] {
            assert!((lum(p.text) - lum(p.ink)).abs() > 0.8);
            assert!((lum(p.secondary) - lum(p.ink)).abs() > 0.3);
        }
    }
}
