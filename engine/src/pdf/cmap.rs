//! CMaps: code to CID mapping for composite fonts, and `/ToUnicode` for text
//! extraction (ISO 32000-1 clause 9.7.5 and 9.10.3).
//!
//! CMap programs are PostScript, but only a handful of operators carry data, so
//! the object lexer plus keyword dispatch is enough.

use std::collections::HashMap;

use super::lexer::Lexer;
use super::object::Obj;

/// A codespace range: how many bytes the code occupies and its bounds.
#[derive(Clone, Copy, Debug)]
pub struct CodeSpace {
    pub bytes: usize,
    pub low: u32,
    pub high: u32,
}

#[derive(Clone, Debug, Default)]
pub struct CMap {
    pub codespace: Vec<CodeSpace>,
    single: HashMap<u32, u32>,
    /// (low, high, first_cid)
    ranges: Vec<(u32, u32, u32)>,
    pub vertical: bool,
}

impl CMap {
    /// `Identity-H` / `Identity-V`: two-byte codes, CID equal to the code.
    pub fn identity(vertical: bool) -> CMap {
        CMap {
            codespace: vec![CodeSpace { bytes: 2, low: 0, high: 0xFFFF }],
            single: HashMap::new(),
            ranges: vec![(0, 0xFFFF, 0)],
            vertical,
        }
    }

    /// Resolves a predefined CMap name.
    ///
    /// The Identity maps are exact. The registry's CJK CMaps (UniJIS-UCS2-H and
    /// friends) need lookup tables we do not carry, so they degrade to a
    /// two-byte identity: the code stream is still segmented correctly, which
    /// keeps glyph positions and advances right for the common case of
    /// Unicode-ordered fonts.
    pub fn predefined(name: &str) -> CMap {
        let vertical = name.ends_with("-V") || name.ends_with('V');
        match name {
            "Identity-H" => CMap::identity(false),
            "Identity-V" => CMap::identity(true),
            _ => {
                let mut m = CMap::identity(vertical);
                m.vertical = vertical;
                m
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.single.is_empty() && self.ranges.is_empty()
    }

    /// Reads the next character code starting at `pos`.
    /// Returns the code and how many bytes it consumed (never zero).
    pub fn next_code(&self, bytes: &[u8], pos: usize) -> (u32, usize) {
        if pos >= bytes.len() {
            return (0, 1);
        }
        // Try increasing byte counts, preferring a codespace range that both
        // has that length and contains the value.
        let mut acc: u32 = 0;
        let mut fallback: Option<(u32, usize)> = None;
        for len in 1..=4usize {
            if pos + len > bytes.len() {
                break;
            }
            acc = (acc << 8) | bytes[pos + len - 1] as u32;
            let mut len_exists = false;
            for cs in &self.codespace {
                if cs.bytes != len {
                    continue;
                }
                len_exists = true;
                if acc >= cs.low && acc <= cs.high {
                    return (acc, len);
                }
            }
            if len_exists && fallback.is_none() {
                // Right length, out of range: the spec says use this length.
                fallback = Some((acc, len));
            }
        }
        if let Some(f) = fallback {
            return f;
        }
        // No codespace at all: single-byte if the map looks single-byte,
        // otherwise two bytes (the composite-font default).
        let default_len = self
            .codespace
            .iter()
            .map(|c| c.bytes)
            .min()
            .unwrap_or(2)
            .clamp(1, 4);
        let mut code: u32 = 0;
        let n = default_len.min(bytes.len() - pos);
        for i in 0..n.max(1) {
            code = (code << 8) | bytes.get(pos + i).copied().unwrap_or(0) as u32;
        }
        (code, n.max(1))
    }

    pub fn cid(&self, code: u32) -> u32 {
        if let Some(c) = self.single.get(&code) {
            return *c;
        }
        for (lo, hi, first) in &self.ranges {
            if code >= *lo && code <= *hi {
                return first + (code - lo);
            }
        }
        0
    }

    /// Parses an embedded CMap stream.
    pub fn parse(data: &[u8]) -> CMap {
        let mut m = CMap::default();
        let mut lx = Lexer::new(data);
        let mut operands: Vec<Obj> = Vec::new();

        loop {
            lx.skip_ws();
            if lx.eof() {
                break;
            }
            let before = lx.pos;
            if let Some(o) = lx.parse_obj() {
                if lx.pos == before {
                    lx.pos += 1;
                    continue;
                }
                if operands.len() < 16 {
                    operands.push(o);
                }
                continue;
            }
            let kw = lx.read_keyword().to_vec();
            if kw.is_empty() {
                lx.pos += 1;
                operands.clear();
                continue;
            }
            match kw.as_slice() {
                b"begincodespacerange" => {
                    while let Some((lo, hi)) = read_pair(&mut lx, b"endcodespacerange") {
                        let bytes = lo.len().clamp(1, 4);
                        m.codespace.push(CodeSpace {
                            bytes,
                            low: be_u32(&lo),
                            high: be_u32(&hi),
                        });
                    }
                }
                b"begincidrange" => {
                    while let Some((lo, hi, cid)) = read_range_with_int(&mut lx, b"endcidrange") {
                        m.ranges.push((be_u32(&lo), be_u32(&hi), cid));
                    }
                }
                b"begincidchar" => {
                    while let Some((code, cid)) = read_char_with_int(&mut lx, b"endcidchar") {
                        m.single.insert(be_u32(&code), cid);
                    }
                }
                b"usecmap" => {
                    // `/Identity-H usecmap` and friends: inherit the referenced
                    // map's codespace so segmentation stays correct.
                    if let Some(name) = operands.last().and_then(|o| o.as_name()) {
                        let base = CMap::predefined(name);
                        if m.codespace.is_empty() {
                            m.codespace = base.codespace.clone();
                        }
                        if m.is_empty() {
                            m.ranges = base.ranges.clone();
                        }
                        m.vertical |= base.vertical;
                    }
                }
                b"endcmap" => break,
                _ => {}
            }
            operands.clear();
        }
        if m.codespace.is_empty() {
            m.codespace.push(CodeSpace { bytes: 2, low: 0, high: 0xFFFF });
        }
        m
    }
}

/// `/ToUnicode` CMap: character code to a Unicode string.
#[derive(Clone, Debug, Default)]
pub struct ToUnicode {
    single: HashMap<u32, String>,
    /// (low, high, first code point) for ranges with contiguous targets.
    ranges: Vec<(u32, u32, u32)>,
}

impl ToUnicode {
    pub fn parse(data: &[u8]) -> ToUnicode {
        let mut m = ToUnicode::default();
        let mut lx = Lexer::new(data);

        loop {
            lx.skip_ws();
            if lx.eof() {
                break;
            }
            let before = lx.pos;
            if lx.parse_obj().is_some() {
                if lx.pos == before {
                    lx.pos += 1;
                }
                continue;
            }
            let kw = lx.read_keyword().to_vec();
            if kw.is_empty() {
                lx.pos += 1;
                continue;
            }
            match kw.as_slice() {
                b"beginbfchar" => {
                    while let Some((code, dst)) = read_bf_char(&mut lx) {
                        let s = utf16be_to_string(&dst);
                        if !s.is_empty() {
                            m.single.insert(be_u32(&code), s);
                        }
                    }
                }
                b"beginbfrange" => m.read_bf_range(&mut lx),
                b"endcmap" => break,
                _ => {}
            }
        }
        m
    }

    fn read_bf_range(&mut self, lx: &mut Lexer) {
        loop {
            lx.skip_ws();
            if lx.eof() {
                return;
            }
            let save = lx.pos;
            // `endbfrange` terminates the section.
            if lx.peek() != Some(b'<') && lx.peek() != Some(b'[') {
                let kw = lx.read_keyword();
                if kw == b"endbfrange" || kw.is_empty() {
                    return;
                }
                if lx.pos == save {
                    lx.pos += 1;
                }
                continue;
            }
            let lo = match lx.parse_obj().and_then(|o| o.as_str_bytes().map(|b| b.to_vec())) {
                Some(v) => v,
                None => return,
            };
            let hi = match lx.parse_obj().and_then(|o| o.as_str_bytes().map(|b| b.to_vec())) {
                Some(v) => v,
                None => return,
            };
            let lo_c = be_u32(&lo);
            let hi_c = be_u32(&hi).max(lo_c);
            lx.skip_ws();
            match lx.parse_obj() {
                Some(Obj::Str(dst)) => {
                    let s = utf16be_to_string(&dst);
                    let chars: Vec<char> = s.chars().collect();
                    if chars.len() == 1 {
                        // Contiguous run from one starting code point.
                        self.ranges.push((lo_c, hi_c, chars[0] as u32));
                    } else if !chars.is_empty() {
                        // Multi-char target: only the first code maps cleanly;
                        // increment the final char for the rest, as the spec says.
                        for (i, code) in (lo_c..=hi_c).enumerate() {
                            let mut t: Vec<char> = chars.clone();
                            if let Some(last) = t.last_mut() {
                                if let Some(c) = char::from_u32(*last as u32 + i as u32) {
                                    *last = c;
                                }
                            }
                            self.single.insert(code, t.into_iter().collect());
                        }
                    }
                }
                Some(Obj::Array(items)) => {
                    for (i, item) in items.iter().enumerate() {
                        if let Some(b) = item.as_str_bytes() {
                            let s = utf16be_to_string(b);
                            if !s.is_empty() {
                                self.single.insert(lo_c + i as u32, s);
                            }
                        }
                    }
                }
                _ => return,
            }
        }
    }

    pub fn lookup(&self, code: u32) -> Option<&str> {
        if let Some(s) = self.single.get(&code) {
            return Some(s.as_str());
        }
        None
    }

    /// Full lookup, including ranges (which need to build a `String`).
    pub fn get(&self, code: u32) -> Option<String> {
        if let Some(s) = self.single.get(&code) {
            return Some(s.clone());
        }
        for (lo, hi, first) in &self.ranges {
            if code >= *lo && code <= *hi {
                let target = first + (code - lo);
                if let Some(c) = char::from_u32(target) {
                    return Some(c.to_string());
                }
            }
        }
        None
    }

    pub fn is_empty(&self) -> bool {
        self.single.is_empty() && self.ranges.is_empty()
    }
}

// ── section readers ──────────────────────────────────────────────────────────

fn read_pair(lx: &mut Lexer, end: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    lx.skip_ws();
    if lx.peek() != Some(b'<') {
        let save = lx.pos;
        let kw = lx.read_keyword();
        if kw == end || kw.is_empty() {
            return None;
        }
        if lx.pos == save {
            lx.pos += 1;
        }
        return read_pair(lx, end);
    }
    let a = lx.parse_obj()?.as_str_bytes()?.to_vec();
    let b = lx.parse_obj()?.as_str_bytes()?.to_vec();
    Some((a, b))
}

fn read_range_with_int(lx: &mut Lexer, end: &[u8]) -> Option<(Vec<u8>, Vec<u8>, u32)> {
    let (a, b) = read_pair(lx, end)?;
    lx.skip_ws();
    let cid = lx.parse_obj()?.as_i64()? as u32;
    Some((a, b, cid))
}

fn read_char_with_int(lx: &mut Lexer, end: &[u8]) -> Option<(Vec<u8>, u32)> {
    lx.skip_ws();
    if lx.peek() != Some(b'<') {
        let save = lx.pos;
        let kw = lx.read_keyword();
        if kw == end || kw.is_empty() {
            return None;
        }
        if lx.pos == save {
            lx.pos += 1;
        }
        return read_char_with_int(lx, end);
    }
    let code = lx.parse_obj()?.as_str_bytes()?.to_vec();
    lx.skip_ws();
    let cid = lx.parse_obj()?.as_i64()? as u32;
    Some((code, cid))
}

fn read_bf_char(lx: &mut Lexer) -> Option<(Vec<u8>, Vec<u8>)> {
    lx.skip_ws();
    if lx.peek() != Some(b'<') {
        let save = lx.pos;
        let kw = lx.read_keyword();
        if kw == b"endbfchar" || kw.is_empty() {
            return None;
        }
        if lx.pos == save {
            lx.pos += 1;
        }
        return read_bf_char(lx);
    }
    let code = lx.parse_obj()?.as_str_bytes()?.to_vec();
    lx.skip_ws();
    // The destination is normally a hex string; a name is also legal.
    match lx.parse_obj()? {
        Obj::Str(s) => Some((code, s.as_ref().clone())),
        Obj::Name(n) => {
            let ch = super::encoding::glyph_name_to_unicode(&n)?;
            let mut buf = [0u16; 2];
            let units = ch.encode_utf16(&mut buf);
            let mut bytes = Vec::new();
            for u in units {
                bytes.extend_from_slice(&u.to_be_bytes());
            }
            Some((code, bytes))
        }
        _ => Some((code, Vec::new())),
    }
}

fn be_u32(bytes: &[u8]) -> u32 {
    let mut v: u32 = 0;
    for &b in bytes.iter().take(4) {
        v = (v << 8) | b as u32;
    }
    v
}

/// Decodes a UTF-16BE byte string, tolerating an odd trailing byte and lone
/// surrogates (both occur in hand-written ToUnicode maps).
fn utf16be_to_string(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    if units.is_empty() {
        // A single byte is sometimes written for a Latin-1 character.
        return bytes
            .first()
            .and_then(|b| char::from_u32(*b as u32))
            .map(|c| c.to_string())
            .unwrap_or_default();
    }
    char::decode_utf16(units.into_iter())
        .map(|r| r.unwrap_or('\u{FFFD}'))
        .filter(|c| *c != '\u{0}')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_h_segments_two_bytes() {
        let m = CMap::identity(false);
        let (code, len) = m.next_code(&[0x00, 0x41, 0x00, 0x42], 0);
        assert_eq!((code, len), (0x0041, 2));
        assert_eq!(m.cid(0x0041), 0x0041);
    }

    #[test]
    fn mixed_codespace_segmentation() {
        // One-byte codes 00..80 plus two-byte codes 8140..9FFC (Shift-JIS shape).
        let src = b"begincodespacerange\n<00> <80>\n<8140> <9ffc>\nendcodespacerange\n";
        let m = CMap::parse(src);
        assert_eq!(m.codespace.len(), 2);
        assert_eq!(m.next_code(&[0x41], 0), (0x41, 1));
        assert_eq!(m.next_code(&[0x81, 0x40], 0), (0x8140, 2));
    }

    #[test]
    fn cid_ranges_and_chars() {
        let src = b"1 begincidrange\n<0020> <007e> 1\nendcidrange\n\
                    1 begincidchar\n<00ff> 500\nendcidchar\n";
        let m = CMap::parse(src);
        assert_eq!(m.cid(0x0020), 1);
        assert_eq!(m.cid(0x0021), 2);
        assert_eq!(m.cid(0x007e), 95);
        assert_eq!(m.cid(0x00ff), 500);
        assert_eq!(m.cid(0x1000), 0);
    }

    #[test]
    fn tounicode_bfchar_and_bfrange() {
        let src = b"2 beginbfchar\n<01> <0041>\n<02> <00660069>\nendbfchar\n\
                    2 beginbfrange\n<10> <12> <0061>\n<20> <21> [<0058> <0059>]\nendbfrange\n";
        let m = ToUnicode::parse(src);
        assert_eq!(m.get(0x01).as_deref(), Some("A"));
        assert_eq!(m.get(0x02).as_deref(), Some("fi"));
        assert_eq!(m.get(0x10).as_deref(), Some("a"));
        assert_eq!(m.get(0x12).as_deref(), Some("c"));
        assert_eq!(m.get(0x20).as_deref(), Some("X"));
        assert_eq!(m.get(0x21).as_deref(), Some("Y"));
        assert_eq!(m.get(0x99), None);
    }

    #[test]
    fn tounicode_surrogate_pair() {
        let src = b"1 beginbfchar\n<01> <D83DDE00>\nendbfchar\n";
        let m = ToUnicode::parse(src);
        assert_eq!(m.get(0x01).as_deref(), Some("\u{1F600}"));
    }

    #[test]
    fn usecmap_inherits_codespace() {
        let src = b"/Identity-H usecmap\n1 begincidchar\n<0005> 9\nendcidchar\n";
        let m = CMap::parse(src);
        assert_eq!(m.next_code(&[0x00, 0x05], 0), (0x0005, 2));
        assert_eq!(m.cid(0x0005), 9);
    }
}
