//! Simple-font encodings and glyph-name to Unicode mapping
//! (ISO 32000-1 Annex D).
//!
//! Codes 32..126 are the same in all three Latin encodings except for two
//! slots, so the shared run lives in `ASCII_NAMES` and each table only lists
//! its differences and its high range.

/// Names for codes 32..=126 as used by WinAnsi and MacRoman.
/// `StandardEncoding` overrides code 39 and 96.
const ASCII_NAMES: [&str; 95] = [
    "space", "exclam", "quotedbl", "numbersign", "dollar", "percent", "ampersand", "quotesingle",
    "parenleft", "parenright", "asterisk", "plus", "comma", "hyphen", "period", "slash",
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
    "colon", "semicolon", "less", "equal", "greater", "question", "at",
    "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M",
    "N", "O", "P", "Q", "R", "S", "T", "U", "V", "W", "X", "Y", "Z",
    "bracketleft", "backslash", "bracketright", "asciicircum", "underscore", "grave",
    "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m",
    "n", "o", "p", "q", "r", "s", "t", "u", "v", "w", "x", "y", "z",
    "braceleft", "bar", "braceright", "asciitilde",
];

/// `StandardEncoding` high range, and its two ASCII differences.
const STANDARD_HIGH: &[(u8, &str)] = &[
    (39, "quoteright"),
    (96, "quoteleft"),
    (161, "exclamdown"), (162, "cent"), (163, "sterling"), (164, "fraction"),
    (165, "yen"), (166, "florin"), (167, "section"), (168, "currency"),
    (169, "quotesingle"), (170, "quotedblleft"), (171, "guillemotleft"),
    (172, "guilsinglleft"), (173, "guilsinglright"), (174, "fi"), (175, "fl"),
    (177, "endash"), (178, "dagger"), (179, "daggerdbl"), (180, "periodcentered"),
    (182, "paragraph"), (183, "bullet"), (184, "quotesinglbase"), (185, "quotedblbase"),
    (186, "quotedblright"), (187, "guillemotright"), (188, "ellipsis"), (189, "perthousand"),
    (191, "questiondown"), (193, "grave"), (194, "acute"), (195, "circumflex"),
    (196, "tilde"), (197, "macron"), (198, "breve"), (199, "dotaccent"),
    (200, "dieresis"), (202, "ring"), (203, "cedilla"), (205, "hungarumlaut"),
    (206, "ogonek"), (207, "caron"), (208, "emdash"), (225, "AE"),
    (227, "ordfeminine"), (232, "Lslash"), (233, "Oslash"), (234, "OE"),
    (235, "ordmasculine"), (241, "ae"), (245, "dotlessi"), (248, "lslash"),
    (249, "oslash"), (250, "oe"), (251, "germandbls"),
];

const WINANSI_HIGH: &[(u8, &str)] = &[
    (128, "Euro"), (130, "quotesinglbase"), (131, "florin"), (132, "quotedblbase"),
    (133, "ellipsis"), (134, "dagger"), (135, "daggerdbl"), (136, "circumflex"),
    (137, "perthousand"), (138, "Scaron"), (139, "guilsinglleft"), (140, "OE"),
    (142, "Zcaron"), (145, "quoteleft"), (146, "quoteright"), (147, "quotedblleft"),
    (148, "quotedblright"), (149, "bullet"), (150, "endash"), (151, "emdash"),
    (152, "tilde"), (153, "trademark"), (154, "scaron"), (155, "guilsinglright"),
    (156, "oe"), (158, "zcaron"), (159, "Ydieresis"), (160, "space"),
    (161, "exclamdown"), (162, "cent"), (163, "sterling"), (164, "currency"),
    (165, "yen"), (166, "brokenbar"), (167, "section"), (168, "dieresis"),
    (169, "copyright"), (170, "ordfeminine"), (171, "guillemotleft"), (172, "logicalnot"),
    (173, "hyphen"), (174, "registered"), (175, "macron"), (176, "degree"),
    (177, "plusminus"), (178, "twosuperior"), (179, "threesuperior"), (180, "acute"),
    (181, "mu"), (182, "paragraph"), (183, "periodcentered"), (184, "cedilla"),
    (185, "onesuperior"), (186, "ordmasculine"), (187, "guillemotright"), (188, "onequarter"),
    (189, "onehalf"), (190, "threequarters"), (191, "questiondown"), (192, "Agrave"),
    (193, "Aacute"), (194, "Acircumflex"), (195, "Atilde"), (196, "Adieresis"),
    (197, "Aring"), (198, "AE"), (199, "Ccedilla"), (200, "Egrave"),
    (201, "Eacute"), (202, "Ecircumflex"), (203, "Edieresis"), (204, "Igrave"),
    (205, "Iacute"), (206, "Icircumflex"), (207, "Idieresis"), (208, "Eth"),
    (209, "Ntilde"), (210, "Ograve"), (211, "Oacute"), (212, "Ocircumflex"),
    (213, "Otilde"), (214, "Odieresis"), (215, "multiply"), (216, "Oslash"),
    (217, "Ugrave"), (218, "Uacute"), (219, "Ucircumflex"), (220, "Udieresis"),
    (221, "Yacute"), (222, "Thorn"), (223, "germandbls"), (224, "agrave"),
    (225, "aacute"), (226, "acircumflex"), (227, "atilde"), (228, "adieresis"),
    (229, "aring"), (230, "ae"), (231, "ccedilla"), (232, "egrave"),
    (233, "eacute"), (234, "ecircumflex"), (235, "edieresis"), (236, "igrave"),
    (237, "iacute"), (238, "icircumflex"), (239, "idieresis"), (240, "eth"),
    (241, "ntilde"), (242, "ograve"), (243, "oacute"), (244, "ocircumflex"),
    (245, "otilde"), (246, "odieresis"), (247, "divide"), (248, "oslash"),
    (249, "ugrave"), (250, "uacute"), (251, "ucircumflex"), (252, "udieresis"),
    (253, "yacute"), (254, "thorn"), (255, "ydieresis"),
];

const MACROMAN_HIGH: &[(u8, &str)] = &[
    (128, "Adieresis"), (129, "Aring"), (130, "Ccedilla"), (131, "Eacute"),
    (132, "Ntilde"), (133, "Odieresis"), (134, "Udieresis"), (135, "aacute"),
    (136, "agrave"), (137, "acircumflex"), (138, "adieresis"), (139, "atilde"),
    (140, "aring"), (141, "ccedilla"), (142, "eacute"), (143, "egrave"),
    (144, "ecircumflex"), (145, "edieresis"), (146, "iacute"), (147, "igrave"),
    (148, "icircumflex"), (149, "idieresis"), (150, "ntilde"), (151, "oacute"),
    (152, "ograve"), (153, "ocircumflex"), (154, "odieresis"), (155, "otilde"),
    (156, "uacute"), (157, "ugrave"), (158, "ucircumflex"), (159, "udieresis"),
    (160, "dagger"), (161, "degree"), (162, "cent"), (163, "sterling"),
    (164, "section"), (165, "bullet"), (166, "paragraph"), (167, "germandbls"),
    (168, "registered"), (169, "copyright"), (170, "trademark"), (171, "acute"),
    (172, "dieresis"), (173, "notequal"), (174, "AE"), (175, "Oslash"),
    (176, "infinity"), (177, "plusminus"), (178, "lessequal"), (179, "greaterequal"),
    (180, "yen"), (181, "mu"), (182, "partialdiff"), (183, "summation"),
    (184, "product"), (185, "pi"), (186, "integral"), (187, "ordfeminine"),
    (188, "ordmasculine"), (189, "Omega"), (190, "ae"), (191, "oslash"),
    (192, "questiondown"), (193, "exclamdown"), (194, "logicalnot"), (195, "radical"),
    (196, "florin"), (197, "approxequal"), (198, "Delta"), (199, "guillemotleft"),
    (200, "guillemotright"), (201, "ellipsis"), (202, "space"), (203, "Agrave"),
    (204, "Atilde"), (205, "Otilde"), (206, "OE"), (207, "oe"),
    (208, "endash"), (209, "emdash"), (210, "quotedblleft"), (211, "quotedblright"),
    (212, "quoteleft"), (213, "quoteright"), (214, "divide"), (215, "lozenge"),
    (216, "ydieresis"), (217, "Ydieresis"), (218, "fraction"), (219, "currency"),
    (220, "guilsinglleft"), (221, "guilsinglright"), (222, "fi"), (223, "fl"),
    (224, "daggerdbl"), (225, "periodcentered"), (226, "quotesinglbase"), (227, "quotedblbase"),
    (228, "perthousand"), (229, "Acircumflex"), (230, "Ecircumflex"), (231, "Aacute"),
    (232, "Edieresis"), (233, "Egrave"), (234, "Iacute"), (235, "Icircumflex"),
    (236, "Idieresis"), (237, "Igrave"), (238, "Oacute"), (239, "Ocircumflex"),
    (240, "apple"), (241, "Ograve"), (242, "Uacute"), (243, "Ucircumflex"),
    (244, "Ugrave"), (245, "dotlessi"), (246, "circumflex"), (247, "tilde"),
    (248, "macron"), (249, "breve"), (250, "dotaccent"), (251, "ring"),
    (252, "cedilla"), (253, "hungarumlaut"), (254, "ogonek"), (255, "caron"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseEncoding {
    Standard,
    WinAnsi,
    MacRoman,
    /// `MacExpertEncoding` is not a Latin text encoding; treated as Standard
    /// since we have no expert glyphs to map to.
    MacExpert,
}

impl BaseEncoding {
    pub fn from_name(name: &str) -> Option<BaseEncoding> {
        match name {
            "WinAnsiEncoding" => Some(BaseEncoding::WinAnsi),
            "MacRomanEncoding" => Some(BaseEncoding::MacRoman),
            "StandardEncoding" => Some(BaseEncoding::Standard),
            "MacExpertEncoding" => Some(BaseEncoding::MacExpert),
            _ => None,
        }
    }
}

/// Builds a code to glyph-name table for one of the base encodings.
pub fn base_table(enc: BaseEncoding) -> [Option<&'static str>; 256] {
    let mut t: [Option<&'static str>; 256] = [None; 256];
    for (i, name) in ASCII_NAMES.iter().enumerate() {
        t[32 + i] = Some(name);
    }
    let high = match enc {
        BaseEncoding::WinAnsi => WINANSI_HIGH,
        BaseEncoding::MacRoman => MACROMAN_HIGH,
        BaseEncoding::Standard | BaseEncoding::MacExpert => STANDARD_HIGH,
    };
    for (code, name) in high {
        t[*code as usize] = Some(name);
    }
    t
}

/// Glyph names that are not plain ASCII, paired with their Unicode value.
/// Together with the ASCII names this covers every name the three Latin
/// encodings can produce, which is what `/Differences` arrays use in practice.
const GLYPH_UNICODE: &[(&str, u32)] = &[
    ("quoteright", 0x2019), ("quoteleft", 0x2018), ("quotedblleft", 0x201C),
    ("quotedblright", 0x201D), ("quotesinglbase", 0x201A), ("quotedblbase", 0x201E),
    ("exclamdown", 0x00A1), ("cent", 0x00A2), ("sterling", 0x00A3), ("fraction", 0x2044),
    ("yen", 0x00A5), ("florin", 0x0192), ("section", 0x00A7), ("currency", 0x00A4),
    ("guillemotleft", 0x00AB), ("guillemotright", 0x00BB),
    ("guilsinglleft", 0x2039), ("guilsinglright", 0x203A),
    ("fi", 0xFB01), ("fl", 0xFB02), ("ff", 0xFB00), ("ffi", 0xFB03), ("ffl", 0xFB04),
    ("endash", 0x2013), ("emdash", 0x2014), ("dagger", 0x2020), ("daggerdbl", 0x2021),
    ("periodcentered", 0x00B7), ("paragraph", 0x00B6), ("bullet", 0x2022),
    ("ellipsis", 0x2026), ("perthousand", 0x2030), ("questiondown", 0x00BF),
    ("grave", 0x0060), ("acute", 0x00B4), ("circumflex", 0x02C6), ("tilde", 0x02DC),
    ("macron", 0x00AF), ("breve", 0x02D8), ("dotaccent", 0x02D9), ("dieresis", 0x00A8),
    ("ring", 0x02DA), ("cedilla", 0x00B8), ("hungarumlaut", 0x02DD), ("ogonek", 0x02DB),
    ("caron", 0x02C7),
    ("AE", 0x00C6), ("ae", 0x00E6), ("OE", 0x0152), ("oe", 0x0153),
    ("Oslash", 0x00D8), ("oslash", 0x00F8), ("Lslash", 0x0141), ("lslash", 0x0142),
    ("ordfeminine", 0x00AA), ("ordmasculine", 0x00BA), ("dotlessi", 0x0131),
    ("germandbls", 0x00DF),
    ("Euro", 0x20AC), ("Scaron", 0x0160), ("scaron", 0x0161),
    ("Zcaron", 0x017D), ("zcaron", 0x017E), ("Ydieresis", 0x0178),
    ("trademark", 0x2122), ("brokenbar", 0x00A6), ("copyright", 0x00A9),
    ("logicalnot", 0x00AC), ("registered", 0x00AE), ("degree", 0x00B0),
    ("plusminus", 0x00B1), ("twosuperior", 0x00B2), ("threesuperior", 0x00B3),
    ("mu", 0x00B5), ("onesuperior", 0x00B9), ("onequarter", 0x00BC),
    ("onehalf", 0x00BD), ("threequarters", 0x00BE), ("multiply", 0x00D7),
    ("divide", 0x00F7),
    ("Agrave", 0x00C0), ("Aacute", 0x00C1), ("Acircumflex", 0x00C2), ("Atilde", 0x00C3),
    ("Adieresis", 0x00C4), ("Aring", 0x00C5), ("Ccedilla", 0x00C7),
    ("Egrave", 0x00C8), ("Eacute", 0x00C9), ("Ecircumflex", 0x00CA), ("Edieresis", 0x00CB),
    ("Igrave", 0x00CC), ("Iacute", 0x00CD), ("Icircumflex", 0x00CE), ("Idieresis", 0x00CF),
    ("Eth", 0x00D0), ("Ntilde", 0x00D1),
    ("Ograve", 0x00D2), ("Oacute", 0x00D3), ("Ocircumflex", 0x00D4), ("Otilde", 0x00D5),
    ("Odieresis", 0x00D6),
    ("Ugrave", 0x00D9), ("Uacute", 0x00DA), ("Ucircumflex", 0x00DB), ("Udieresis", 0x00DC),
    ("Yacute", 0x00DD), ("Thorn", 0x00DE),
    ("agrave", 0x00E0), ("aacute", 0x00E1), ("acircumflex", 0x00E2), ("atilde", 0x00E3),
    ("adieresis", 0x00E4), ("aring", 0x00E5), ("ccedilla", 0x00E7),
    ("egrave", 0x00E8), ("eacute", 0x00E9), ("ecircumflex", 0x00EA), ("edieresis", 0x00EB),
    ("igrave", 0x00EC), ("iacute", 0x00ED), ("icircumflex", 0x00EE), ("idieresis", 0x00EF),
    ("eth", 0x00F0), ("ntilde", 0x00F1),
    ("ograve", 0x00F2), ("oacute", 0x00F3), ("ocircumflex", 0x00F4), ("otilde", 0x00F5),
    ("odieresis", 0x00F6),
    ("ugrave", 0x00F9), ("uacute", 0x00FA), ("ucircumflex", 0x00FB), ("udieresis", 0x00FC),
    ("yacute", 0x00FD), ("thorn", 0x00FE), ("ydieresis", 0x00FF),
    // MacRoman extras.
    ("notequal", 0x2260), ("infinity", 0x221E), ("lessequal", 0x2264),
    ("greaterequal", 0x2265), ("partialdiff", 0x2202), ("summation", 0x2211),
    ("product", 0x220F), ("pi", 0x03C0), ("integral", 0x222B), ("Omega", 0x2126),
    ("radical", 0x221A), ("approxequal", 0x2248), ("Delta", 0x2206),
    ("lozenge", 0x25CA), ("apple", 0xF8FF),
    // Frequently seen in Symbol-ish differences arrays.
    ("minus", 0x2212), ("nbspace", 0x00A0), ("Euro1", 0x20AC),
    ("nonbreakingspace", 0x00A0), ("softhyphen", 0x00AD), ("hyphensoft", 0x00AD),
    ("zerowidthspace", 0x200B),
];

/// Maps a PostScript glyph name to a Unicode scalar.
///
/// Covers the Latin encodings' names plus the algorithmic conventions that
/// subsetters emit: `uniXXXX`, `uXXXX[XX]`, `gNN`/`cidNN`/`GNN` (no Unicode),
/// and names with a suffix such as `a.sc` or `f_i`.
pub fn glyph_name_to_unicode(name: &str) -> Option<char> {
    if name.is_empty() {
        return None;
    }
    // Single ASCII letter/digit names map to themselves.
    if name.len() == 1 {
        let c = name.chars().next()?;
        if c.is_ascii_graphic() {
            return Some(c);
        }
    }
    if let Some(i) = ASCII_NAMES.iter().position(|n| *n == name) {
        return char::from_u32((32 + i) as u32);
    }
    if let Some((_, u)) = GLYPH_UNICODE.iter().find(|(n, _)| *n == name) {
        return char::from_u32(*u);
    }
    // uniXXXX (possibly several, we take the first).
    if let Some(hex) = name.strip_prefix("uni") {
        if hex.len() >= 4 {
            if let Ok(v) = u32::from_str_radix(&hex[..4], 16) {
                return char::from_u32(v);
            }
        }
    }
    // uXXXX .. uXXXXXX
    if let Some(hex) = name.strip_prefix('u') {
        if (4..=6).contains(&hex.len()) && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            if let Ok(v) = u32::from_str_radix(hex, 16) {
                return char::from_u32(v);
            }
        }
    }
    // A suffix like `.sc` or `.alt` does not change the character.
    if let Some(base) = name.split('.').next() {
        if base != name && !base.is_empty() {
            return glyph_name_to_unicode(base);
        }
    }
    // Ligature names joined with underscores: take the first component.
    if let Some(first) = name.split('_').next() {
        if first != name && !first.is_empty() {
            return glyph_name_to_unicode(first);
        }
    }
    None
}

/// True for names that identify a glyph by index rather than by character
/// (`g12`, `cid34`, `G7`, `index99`): these carry no Unicode meaning.
pub fn glyph_name_index(name: &str) -> Option<u16> {
    for prefix in ["cid", "glyph", "index", "g", "G"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                return rest.parse().ok();
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_run_is_complete() {
        let t = base_table(BaseEncoding::WinAnsi);
        assert_eq!(t[32], Some("space"));
        assert_eq!(t[65], Some("A"));
        assert_eq!(t[97], Some("a"));
        assert_eq!(t[126], Some("asciitilde"));
        assert_eq!(t[39], Some("quotesingle"));
        assert_eq!(t[96], Some("grave"));
    }

    #[test]
    fn standard_encoding_quote_slots_differ() {
        let s = base_table(BaseEncoding::Standard);
        assert_eq!(s[39], Some("quoteright"));
        assert_eq!(s[96], Some("quoteleft"));
    }

    #[test]
    fn winansi_high_range() {
        let t = base_table(BaseEncoding::WinAnsi);
        assert_eq!(t[128], Some("Euro"));
        assert_eq!(t[233], Some("eacute"));
        assert_eq!(t[255], Some("ydieresis"));
        assert_eq!(t[127], None);
    }

    #[test]
    fn macroman_high_range() {
        let t = base_table(BaseEncoding::MacRoman);
        assert_eq!(t[128], Some("Adieresis"));
        assert_eq!(t[213], Some("quoteright"));
        assert_eq!(t[255], Some("caron"));
    }

    #[test]
    fn names_to_unicode() {
        assert_eq!(glyph_name_to_unicode("A"), Some('A'));
        assert_eq!(glyph_name_to_unicode("space"), Some(' '));
        assert_eq!(glyph_name_to_unicode("eacute"), Some('é'));
        assert_eq!(glyph_name_to_unicode("emdash"), Some('\u{2014}'));
        assert_eq!(glyph_name_to_unicode("uni00E9"), Some('é'));
        assert_eq!(glyph_name_to_unicode("u1F600"), Some('\u{1F600}'));
        assert_eq!(glyph_name_to_unicode("a.sc"), Some('a'));
        assert_eq!(glyph_name_to_unicode("f_i"), Some('f'));
        assert_eq!(glyph_name_to_unicode("nosuchglyph"), None);
    }

    #[test]
    fn index_names() {
        assert_eq!(glyph_name_index("g42"), Some(42));
        assert_eq!(glyph_name_index("cid7"), Some(7));
        assert_eq!(glyph_name_index("G0"), Some(0));
        assert_eq!(glyph_name_index("eacute"), None);
    }

    #[test]
    fn every_encoding_name_resolves_to_unicode() {
        // A missing entry here would silently break text extraction, so make it
        // a hard failure.
        for enc in [BaseEncoding::Standard, BaseEncoding::WinAnsi, BaseEncoding::MacRoman] {
            for (code, name) in base_table(enc).iter().enumerate() {
                if let Some(n) = name {
                    assert!(
                        glyph_name_to_unicode(n).is_some(),
                        "no unicode for glyph name {n:?} (code {code}, {enc:?})"
                    );
                }
            }
        }
    }
}
