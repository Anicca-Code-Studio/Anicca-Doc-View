//! Bundled font data, shared by the OOXML renderer and the PDF renderer.
//!
//! Kept in one place so the bytes are embedded in the WASM binary exactly once.

pub const ROBOTO_REGULAR: &[u8] = include_bytes!("../fonts/Roboto-Regular.ttf");
pub const ROBOTO_BOLD: &[u8] = include_bytes!("../fonts/Roboto-Bold.ttf");
pub const ROBOTO_ITALIC: &[u8] = include_bytes!("../fonts/Roboto-Italic.ttf");
pub const ROBOTO_BOLD_ITALIC: &[u8] = include_bytes!("../fonts/Roboto-BoldItalic.ttf");

pub const LIBERATION_SANS: &[u8] = include_bytes!("../fonts/LiberationSans-Regular.ttf");
pub const LIBERATION_SANS_BOLD: &[u8] = include_bytes!("../fonts/LiberationSans-Bold.ttf");
pub const LIBERATION_SANS_ITALIC: &[u8] = include_bytes!("../fonts/LiberationSans-Italic.ttf");
pub const LIBERATION_SANS_BOLD_ITALIC: &[u8] =
    include_bytes!("../fonts/LiberationSans-BoldItalic.ttf");

pub const LIBERATION_SERIF: &[u8] = include_bytes!("../fonts/LiberationSerif-Regular.ttf");
pub const LIBERATION_SERIF_BOLD: &[u8] = include_bytes!("../fonts/LiberationSerif-Bold.ttf");
pub const LIBERATION_SERIF_ITALIC: &[u8] = include_bytes!("../fonts/LiberationSerif-Italic.ttf");
pub const LIBERATION_SERIF_BOLD_ITALIC: &[u8] =
    include_bytes!("../fonts/LiberationSerif-BoldItalic.ttf");

pub const TIMES_NEW_ROMAN: &[u8] = include_bytes!("../fonts/TimesNewRoman.ttf");
pub const TIMES_NEW_ROMAN_BOLD: &[u8] = include_bytes!("../fonts/TimesNewRoman-Bold.ttf");
pub const TIMES_NEW_ROMAN_ITALIC: &[u8] = include_bytes!("../fonts/TimesNewRoman-Italic.ttf");
pub const TIMES_NEW_ROMAN_BOLD_ITALIC: &[u8] =
    include_bytes!("../fonts/TimesNewRoman-BoldItalic.ttf");

pub const CARLITO: &[u8] = include_bytes!("../fonts/Carlito-Regular.ttf");
pub const CARLITO_BOLD: &[u8] = include_bytes!("../fonts/Carlito-Bold.ttf");
pub const CARLITO_ITALIC: &[u8] = include_bytes!("../fonts/Carlito-Italic.ttf");
pub const CARLITO_BOLD_ITALIC: &[u8] = include_bytes!("../fonts/Carlito-BoldItalic.ttf");

pub const DEJAVU_SANS: &[u8] = include_bytes!("../fonts/DejaVuSans.ttf");
pub const DEJAVU_SANS_BOLD: &[u8] = include_bytes!("../fonts/DejaVuSans-Bold.ttf");
pub const DEJAVU_SANS_OBLIQUE: &[u8] = include_bytes!("../fonts/DejaVuSans-Oblique.ttf");
pub const DEJAVU_SANS_BOLD_OBLIQUE: &[u8] = include_bytes!("../fonts/DejaVuSans-BoldOblique.ttf");

pub const NOTO_SANS_JP: &[u8] = include_bytes!("../fonts/NotoSansJP-Regular.ttf");
pub const NOTO_SANS_ARABIC: &[u8] = include_bytes!("../fonts/NotoSansArabic-Regular.ttf");
pub const NOTO_SANS_HEBREW: &[u8] = include_bytes!("../fonts/NotoSansHebrew-Regular.ttf");
pub const NOTO_SANS_THAI: &[u8] = include_bytes!("../fonts/NotoSansThai-Regular.ttf");
pub const NOTO_SANS_DEVANAGARI: &[u8] = include_bytes!("../fonts/NotoSansDevanagari-Regular.ttf");

/// Every bundled face, for bulk registration into a font database.
pub const ALL: &[&[u8]] = &[
    ROBOTO_REGULAR,
    ROBOTO_BOLD,
    ROBOTO_ITALIC,
    ROBOTO_BOLD_ITALIC,
    LIBERATION_SANS,
    LIBERATION_SANS_BOLD,
    LIBERATION_SANS_ITALIC,
    LIBERATION_SANS_BOLD_ITALIC,
    LIBERATION_SERIF,
    LIBERATION_SERIF_BOLD,
    LIBERATION_SERIF_ITALIC,
    LIBERATION_SERIF_BOLD_ITALIC,
    TIMES_NEW_ROMAN,
    TIMES_NEW_ROMAN_BOLD,
    TIMES_NEW_ROMAN_ITALIC,
    TIMES_NEW_ROMAN_BOLD_ITALIC,
    CARLITO,
    CARLITO_BOLD,
    CARLITO_ITALIC,
    CARLITO_BOLD_ITALIC,
    DEJAVU_SANS,
    DEJAVU_SANS_BOLD,
    DEJAVU_SANS_OBLIQUE,
    DEJAVU_SANS_BOLD_OBLIQUE,
    NOTO_SANS_JP,
    NOTO_SANS_ARABIC,
    NOTO_SANS_HEBREW,
    NOTO_SANS_THAI,
    NOTO_SANS_DEVANAGARI,
];
