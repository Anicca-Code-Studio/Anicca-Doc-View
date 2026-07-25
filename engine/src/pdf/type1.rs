//! Type 1 font programs: eexec decryption and the Type 1 charstring
//! interpreter (Adobe Type 1 Font Format).
//!
//! Produces glyph outlines in font units together with the font's built-in
//! encoding, which a symbolic font relies on when the PDF supplies no
//! `/Encoding`.

use std::collections::HashMap;

use crate::raster::{Path, Transform};

/// Charstring interpreter limits.
const MAX_SUBR_DEPTH: usize = 16;
const MAX_OPS: usize = 200_000;

pub struct Type1Font {
    pub charstrings: HashMap<String, Vec<u8>>,
    pub subrs: Vec<Vec<u8>>,
    /// Built-in encoding: code to glyph name.
    pub encoding: Vec<Option<String>>,
    /// `/FontMatrix`, normally `[0.001 0 0 0.001 0 0]`.
    pub font_matrix: Transform,
}

impl Type1Font {
    pub fn glyph_names(&self) -> impl Iterator<Item = &String> {
        self.charstrings.keys()
    }

    pub fn has_glyph(&self, name: &str) -> bool {
        self.charstrings.contains_key(name)
    }

    /// Outlines a glyph by name. Returns the path in font units and the
    /// advance width from `hsbw`/`sbw`.
    pub fn outline(&self, name: &str) -> Option<(Path, f64)> {
        let cs = self.charstrings.get(name)?;
        let mut st = CharStringState::new(self);
        st.run(cs, 0);
        st.flush();
        Some((st.path, st.advance))
    }

    /// Parses a Type 1 font program. `clear_len` is `/Length1` when known; the
    /// `eexec` marker is searched for when it is not.
    pub fn parse(data: &[u8], clear_len: Option<usize>) -> Option<Type1Font> {
        // A PFB wrapper prefixes each segment with 0x80 <type> <len32le>.
        let data = strip_pfb(data);

        let eexec_at = match clear_len {
            Some(l) if l <= data.len() && find(&data[..l.min(data.len())], b"eexec").is_some() => {
                find(&data[..l], b"eexec").map(|i| i + 5)
            }
            _ => find(&data, b"eexec").map(|i| i + 5),
        };
        let eexec_at = eexec_at?;

        // Skip the EOL after `eexec`.
        let mut enc_start = eexec_at;
        while enc_start < data.len() && matches!(data[enc_start], b'\r' | b'\n' | b' ' | b'\t') {
            enc_start += 1;
        }
        let encrypted_raw = &data[enc_start.min(data.len())..];

        // PFA files hex-encode the private portion.
        let encrypted = if looks_hex(encrypted_raw) {
            super::filter::ascii_hex_decode(encrypted_raw)
        } else {
            encrypted_raw.to_vec()
        };
        let private = eexec_decrypt(&encrypted, 55665, 4);

        let clear = &data[..eexec_at.min(data.len())];
        let font_matrix = parse_font_matrix(clear).unwrap_or(Transform::new(0.001, 0.0, 0.0, 0.001, 0.0, 0.0));
        let encoding = parse_builtin_encoding(clear);

        let len_iv = parse_len_iv(&private).unwrap_or(4);
        let subrs = parse_subrs(&private, len_iv);
        let charstrings = parse_charstrings(&private, len_iv);
        if charstrings.is_empty() {
            return None;
        }

        Some(Type1Font { charstrings, subrs, encoding, font_matrix })
    }
}

// ── container handling ───────────────────────────────────────────────────────

fn strip_pfb(data: &[u8]) -> Vec<u8> {
    if data.first() != Some(&0x80) {
        return data.to_vec();
    }
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 6 <= data.len() && data[i] == 0x80 {
        let kind = data[i + 1];
        if kind == 3 {
            break; // end-of-file segment
        }
        let len = u32::from_le_bytes([data[i + 2], data[i + 3], data[i + 4], data[i + 5]]) as usize;
        let start = i + 6;
        let end = (start + len).min(data.len());
        out.extend_from_slice(&data[start..end]);
        i = end;
    }
    if out.is_empty() {
        data.to_vec()
    } else {
        out
    }
}

fn looks_hex(data: &[u8]) -> bool {
    // The first four bytes of the encrypted portion decide: hex-encoded PFA
    // files start with four hex digits.
    data.iter()
        .filter(|b| !matches!(**b, b'\r' | b'\n' | b' ' | b'\t'))
        .take(4)
        .all(|b| b.is_ascii_hexdigit())
}

/// The eexec / charstring decryption from the Type 1 specification.
pub fn eexec_decrypt(data: &[u8], key: u16, skip: usize) -> Vec<u8> {
    const C1: u16 = 52845;
    const C2: u16 = 22719;
    let mut r = key;
    let mut out = Vec::with_capacity(data.len().saturating_sub(skip));
    for (i, &c) in data.iter().enumerate() {
        let p = c ^ (r >> 8) as u8;
        r = (c as u16).wrapping_add(r).wrapping_mul(C1).wrapping_add(C2);
        if i >= skip {
            out.push(p);
        }
    }
    out
}

// ── cleartext parsing ────────────────────────────────────────────────────────

fn parse_font_matrix(clear: &[u8]) -> Option<Transform> {
    let at = find(clear, b"/FontMatrix")?;
    let rest = &clear[at..];
    let open = find(rest, b"[")?;
    let close = find(rest, b"]")?;
    if close <= open {
        return None;
    }
    let text = std::str::from_utf8(&rest[open + 1..close]).ok()?;
    let v: Vec<f64> = text
        .split_whitespace()
        .filter_map(|t| t.parse::<f64>().ok())
        .collect();
    if v.len() < 6 {
        return None;
    }
    Some(Transform::new(v[0], v[1], v[2], v[3], v[4], v[5]))
}

/// Reads `dup <code> /<name> put` entries, or notes the standard encoding.
fn parse_builtin_encoding(clear: &[u8]) -> Vec<Option<String>> {
    let mut enc: Vec<Option<String>> = vec![None; 256];
    let at = match find(clear, b"/Encoding") {
        Some(a) => a,
        None => return enc,
    };
    let rest = &clear[at..];
    if find(&rest[..rest.len().min(64)], b"StandardEncoding").is_some() {
        for (code, name) in super::encoding::base_table(super::encoding::BaseEncoding::Standard)
            .iter()
            .enumerate()
        {
            enc[code] = name.map(|s| s.to_string());
        }
        return enc;
    }

    let mut i = 0usize;
    // Stop at `readonly def` / `def`, which closes the encoding array.
    let limit = find(rest, b" def").unwrap_or(rest.len());
    let section = &rest[..limit];
    while let Some(d) = find(&section[i..], b"dup ") {
        let mut p = i + d + 4;
        // code
        while p < section.len() && section[p].is_ascii_whitespace() {
            p += 1;
        }
        let num_start = p;
        while p < section.len() && section[p].is_ascii_digit() {
            p += 1;
        }
        let code: usize = match std::str::from_utf8(&section[num_start..p]).ok().and_then(|s| s.parse().ok()) {
            Some(c) => c,
            None => {
                i = p.max(i + d + 4);
                continue;
            }
        };
        while p < section.len() && section[p].is_ascii_whitespace() {
            p += 1;
        }
        if section.get(p) != Some(&b'/') {
            i = p.max(i + d + 4);
            continue;
        }
        p += 1;
        let name_start = p;
        while p < section.len() && super::lexer::is_regular(section[p]) {
            p += 1;
        }
        if code < 256 {
            enc[code] = std::str::from_utf8(&section[name_start..p]).ok().map(|s| s.to_string());
        }
        i = p;
    }
    enc
}

// ── private dict parsing ─────────────────────────────────────────────────────

fn parse_len_iv(private: &[u8]) -> Option<usize> {
    let at = find(private, b"/lenIV")?;
    let rest = &private[at + 6..];
    let text = std::str::from_utf8(&rest[..rest.len().min(8)]).ok()?;
    text.split_whitespace().next()?.parse().ok()
}

/// Reads `dup <i> <len> RD <bytes> NP` entries from `/Subrs`.
fn parse_subrs(private: &[u8], len_iv: usize) -> Vec<Vec<u8>> {
    let at = match find(private, b"/Subrs") {
        Some(a) => a,
        None => return Vec::new(),
    };
    let count = read_int_after(private, at + 6).unwrap_or(0).min(65_536) as usize;
    let mut subrs: Vec<Vec<u8>> = vec![Vec::new(); count];

    let mut i = at;
    let mut found = 0usize;
    while found < count {
        let d = match find(&private[i..], b"dup ") {
            Some(d) => i + d + 4,
            None => break,
        };
        let (idx, p) = match read_int_at(private, d) {
            Some(v) => v,
            None => {
                i = d;
                continue;
            }
        };
        let (len, p) = match read_int_at(private, p) {
            Some(v) => v,
            None => {
                i = d;
                continue;
            }
        };
        // Skip the RD/-| token and the single space that follows it.
        let data_start = match skip_rd_token(private, p) {
            Some(s) => s,
            None => {
                i = d;
                continue;
            }
        };
        let end = (data_start + len as usize).min(private.len());
        if (idx as usize) < subrs.len() {
            subrs[idx as usize] = eexec_decrypt(&private[data_start..end], 4330, len_iv);
        }
        found += 1;
        i = end;
    }
    subrs
}

/// Reads `/<name> <len> RD <bytes> ND` entries from `/CharStrings`.
fn parse_charstrings(private: &[u8], len_iv: usize) -> HashMap<String, Vec<u8>> {
    let mut out = HashMap::new();
    let start = match find(private, b"/CharStrings") {
        Some(a) => a + 12,
        None => return out,
    };
    let mut i = start;
    while i < private.len() {
        // Find the next name token.
        let slash = match private[i..].iter().position(|b| *b == b'/') {
            Some(p) => i + p,
            None => break,
        };
        let mut p = slash + 1;
        let name_start = p;
        while p < private.len() && super::lexer::is_regular(private[p]) {
            p += 1;
        }
        let name = match std::str::from_utf8(&private[name_start..p]) {
            Ok(s) if !s.is_empty() => s.to_string(),
            _ => {
                i = p.max(slash + 1);
                continue;
            }
        };
        let (len, p) = match read_int_at(private, p) {
            Some(v) => v,
            None => {
                i = p.max(slash + 1);
                continue;
            }
        };
        let data_start = match skip_rd_token(private, p) {
            Some(s) => s,
            None => {
                i = p.max(slash + 1);
                continue;
            }
        };
        let end = (data_start + len as usize).min(private.len());
        out.insert(name, eexec_decrypt(&private[data_start..end], 4330, len_iv));
        i = end;
        if out.len() > 20_000 {
            break;
        }
    }
    out
}

/// Skips whitespace, the `RD`/`-|` token, and exactly one following space.
fn skip_rd_token(data: &[u8], mut p: usize) -> Option<usize> {
    while p < data.len() && data[p].is_ascii_whitespace() {
        p += 1;
    }
    let tok_start = p;
    while p < data.len() && !data[p].is_ascii_whitespace() {
        p += 1;
    }
    if p == tok_start || p >= data.len() {
        return None;
    }
    Some(p + 1)
}

fn read_int_at(data: &[u8], mut p: usize) -> Option<(i64, usize)> {
    while p < data.len() && data[p].is_ascii_whitespace() {
        p += 1;
    }
    let start = p;
    if p < data.len() && (data[p] == b'-' || data[p] == b'+') {
        p += 1;
    }
    while p < data.len() && data[p].is_ascii_digit() {
        p += 1;
    }
    if p == start {
        return None;
    }
    let v: i64 = std::str::from_utf8(&data[start..p]).ok()?.parse().ok()?;
    Some((v, p))
}

fn read_int_after(data: &[u8], p: usize) -> Option<i64> {
    read_int_at(data, p).map(|(v, _)| v)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    super::xref::find(hay, needle)
}

// ── charstring interpreter ───────────────────────────────────────────────────

struct CharStringState<'a> {
    font: &'a Type1Font,
    stack: Vec<f64>,
    /// PostScript operand stack used by `callothersubr` / `pop`.
    ps_stack: Vec<f64>,
    path: Path,
    x: f64,
    y: f64,
    /// Left side bearing from `hsbw`.
    sbx: f64,
    advance: f64,
    open: bool,
    ops: usize,
    /// Flex collection state (OtherSubrs 0..2).
    flex: Option<Vec<(f64, f64)>>,
    done: bool,
}

impl<'a> CharStringState<'a> {
    fn new(font: &'a Type1Font) -> CharStringState<'a> {
        CharStringState {
            font,
            stack: Vec::new(),
            ps_stack: Vec::new(),
            path: Path::new(),
            x: 0.0,
            y: 0.0,
            sbx: 0.0,
            advance: 0.0,
            open: false,
            ops: 0,
            flex: None,
            done: false,
        }
    }

    fn flush(&mut self) {
        if self.open {
            self.path.close();
            self.open = false;
        }
    }

    fn move_to(&mut self, x: f64, y: f64) {
        if let Some(pts) = self.flex.as_mut() {
            pts.push((x, y));
            self.x = x;
            self.y = y;
            return;
        }
        self.flush();
        self.path.move_to(x, y);
        self.open = true;
        self.x = x;
        self.y = y;
    }

    fn line_to(&mut self, x: f64, y: f64) {
        if !self.open {
            self.path.move_to(self.x, self.y);
            self.open = true;
        }
        self.path.line_to(x, y);
        self.x = x;
        self.y = y;
    }

    fn curve_to(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, x3: f64, y3: f64) {
        if !self.open {
            self.path.move_to(self.x, self.y);
            self.open = true;
        }
        self.path.curve_to(x1, y1, x2, y2, x3, y3);
        self.x = x3;
        self.y = y3;
    }

    fn run(&mut self, cs: &[u8], depth: usize) {
        if depth > MAX_SUBR_DEPTH || self.done {
            return;
        }
        let mut i = 0usize;
        while i < cs.len() {
            if self.done {
                return;
            }
            self.ops += 1;
            if self.ops > MAX_OPS {
                self.done = true;
                return;
            }
            let v = cs[i];
            i += 1;
            // Operand encodings.
            if v >= 32 {
                let num = if v <= 246 {
                    v as f64 - 139.0
                } else if v <= 250 {
                    let w = match cs.get(i) {
                        Some(w) => *w as f64,
                        None => return,
                    };
                    i += 1;
                    (v as f64 - 247.0) * 256.0 + w + 108.0
                } else if v <= 254 {
                    let w = match cs.get(i) {
                        Some(w) => *w as f64,
                        None => return,
                    };
                    i += 1;
                    -((v as f64 - 251.0) * 256.0) - w - 108.0
                } else {
                    if i + 4 > cs.len() {
                        return;
                    }
                    let n = i32::from_be_bytes([cs[i], cs[i + 1], cs[i + 2], cs[i + 3]]);
                    i += 4;
                    n as f64
                };
                if self.stack.len() < 48 {
                    self.stack.push(num);
                }
                continue;
            }

            match v {
                // hstem / vstem: hints, no geometry.
                1 | 3 => self.stack.clear(),
                4 => {
                    // vmoveto dy
                    let dy = self.pop_n(1).first().copied().unwrap_or(0.0);
                    let (x, y) = (self.x, self.y + dy);
                    self.move_to(x, y);
                }
                5 => {
                    let a = self.pop_n(2);
                    let (x, y) = (self.x + a[0], self.y + a[1]);
                    self.line_to(x, y);
                }
                6 => {
                    let dx = self.pop_n(1)[0];
                    let (x, y) = (self.x + dx, self.y);
                    self.line_to(x, y);
                }
                7 => {
                    let dy = self.pop_n(1)[0];
                    let (x, y) = (self.x, self.y + dy);
                    self.line_to(x, y);
                }
                8 => {
                    let a = self.pop_n(6);
                    let x1 = self.x + a[0];
                    let y1 = self.y + a[1];
                    let x2 = x1 + a[2];
                    let y2 = y1 + a[3];
                    self.curve_to(x1, y1, x2, y2, x2 + a[4], y2 + a[5]);
                }
                9 => {
                    self.flush();
                    self.stack.clear();
                }
                10 => {
                    // callsubr
                    let idx = self.stack.pop().unwrap_or(-1.0);
                    let idx = idx as i64;
                    if idx >= 0 {
                        if let Some(sub) = self.font.subrs.get(idx as usize) {
                            let sub = sub.clone();
                            self.run(&sub, depth + 1);
                        }
                    }
                }
                11 => return, // return
                13 => {
                    // hsbw sbx wx
                    let a = self.pop_n(2);
                    self.sbx = a[0];
                    self.advance = a[1];
                    self.x = a[0];
                    self.y = 0.0;
                }
                14 => {
                    // endchar
                    self.flush();
                    self.done = true;
                    return;
                }
                21 => {
                    let a = self.pop_n(2);
                    let (x, y) = (self.x + a[0], self.y + a[1]);
                    self.move_to(x, y);
                }
                22 => {
                    let dx = self.pop_n(1)[0];
                    let (x, y) = (self.x + dx, self.y);
                    self.move_to(x, y);
                }
                30 => {
                    // vhcurveto: dy1 dx2 dy2 dx3
                    let a = self.pop_n(4);
                    let x1 = self.x;
                    let y1 = self.y + a[0];
                    let x2 = x1 + a[1];
                    let y2 = y1 + a[2];
                    self.curve_to(x1, y1, x2, y2, x2 + a[3], y2);
                }
                31 => {
                    // hvcurveto: dx1 dx2 dy2 dy3
                    let a = self.pop_n(4);
                    let x1 = self.x + a[0];
                    let y1 = self.y;
                    let x2 = x1 + a[1];
                    let y2 = y1 + a[2];
                    self.curve_to(x1, y1, x2, y2, x2, y2 + a[3]);
                }
                12 => {
                    let v2 = match cs.get(i) {
                        Some(v) => *v,
                        None => return,
                    };
                    i += 1;
                    match v2 {
                        0 => self.stack.clear(),      // dotsection
                        1 | 2 => self.stack.clear(),  // vstem3 / hstem3
                        6 => {
                            // seac: standard-encoding accented character
                            let a = self.pop_n(5);
                            self.seac(a[1], a[2], a[3] as i32, a[4] as i32, depth);
                            self.done = true;
                            return;
                        }
                        7 => {
                            // sbw sbx sby wx wy
                            let a = self.pop_n(4);
                            self.sbx = a[0];
                            self.advance = a[2];
                            self.x = a[0];
                            self.y = a[1];
                        }
                        12 => {
                            // div
                            let b = self.stack.pop().unwrap_or(1.0);
                            let a = self.stack.pop().unwrap_or(0.0);
                            self.stack.push(if b == 0.0 { 0.0 } else { a / b });
                        }
                        16 => self.call_othersubr(),
                        17 => {
                            // pop: take a value the OtherSubr left behind.
                            let v = self.ps_stack.pop().unwrap_or(0.0);
                            self.stack.push(v);
                        }
                        33 => {
                            // setcurrentpoint
                            let a = self.pop_n(2);
                            self.x = a[0];
                            self.y = a[1];
                        }
                        _ => self.stack.clear(),
                    }
                }
                _ => self.stack.clear(),
            }
        }
    }

    /// OtherSubrs 0..3 have standard meanings that the interpreter emulates
    /// rather than executing: flex (0..2) and hint replacement (3).
    fn call_othersubr(&mut self) {
        let othersubr = self.stack.pop().unwrap_or(-1.0) as i64;
        let n = self.stack.pop().unwrap_or(0.0).max(0.0) as usize;
        let n = n.min(self.stack.len());
        let args: Vec<f64> = self.stack.split_off(self.stack.len() - n);

        match othersubr {
            0 => {
                // End of flex: seven collected points make two curves.
                let pts = self.flex.take().unwrap_or_default();
                if pts.len() >= 7 {
                    let p = &pts[pts.len() - 7..];
                    self.curve_to(p[1].0, p[1].1, p[2].0, p[2].1, p[3].0, p[3].1);
                    self.curve_to(p[4].0, p[4].1, p[5].0, p[5].1, p[6].0, p[6].1);
                }
                // The charstring follows with `pop pop setcurrentpoint`.
                self.ps_stack.clear();
                self.ps_stack.push(self.y);
                self.ps_stack.push(self.x);
            }
            1 => self.flex = Some(Vec::new()),
            2 => {}
            3 => {
                // Hint replacement: the following `pop callsubr` needs the subr
                // number back on the stack.
                self.ps_stack.clear();
                self.ps_stack.push(args.first().copied().unwrap_or(3.0));
            }
            _ => {
                // Unknown OtherSubr: the convention is that its arguments come
                // back through `pop`.
                self.ps_stack.clear();
                for a in args.iter().rev() {
                    self.ps_stack.push(*a);
                }
            }
        }
    }

    fn seac(&mut self, adx: f64, ady: f64, bchar: i32, achar: i32, depth: usize) {
        let std = super::encoding::base_table(super::encoding::BaseEncoding::Standard);
        let base_name = std.get(bchar.clamp(0, 255) as usize).copied().flatten();
        let accent_name = std.get(achar.clamp(0, 255) as usize).copied().flatten();

        if let Some(bn) = base_name {
            if let Some(cs) = self.font.charstrings.get(bn) {
                let cs = cs.clone();
                let mut sub = CharStringState::new(self.font);
                sub.run(&cs, depth + 1);
                sub.flush();
                self.path.segs.extend(sub.path.segs);
                if self.advance == 0.0 {
                    self.advance = sub.advance;
                }
            }
        }
        if let Some(an) = accent_name {
            if let Some(cs) = self.font.charstrings.get(an) {
                let cs = cs.clone();
                let mut sub = CharStringState::new(self.font);
                sub.run(&cs, depth + 1);
                sub.flush();
                // The accent shifts by adx/ady, corrected for the difference in
                // side bearings (asb vs the accent's own sbx).
                let shift = Transform::translate(self.sbx - sub.sbx + adx, ady);
                let moved = sub.path.transformed(&shift);
                self.path.segs.extend(moved.segs);
            }
        }
    }

    /// Takes the last `n` operands, padding with zeros when the charstring is
    /// malformed, and clears the stack (Type 1 operators consume everything).
    fn pop_n(&mut self, n: usize) -> Vec<f64> {
        let mut out = vec![0.0; n];
        let have = self.stack.len().min(n);
        for k in 0..have {
            out[n - 1 - k] = self.stack[self.stack.len() - 1 - k];
        }
        self.stack.clear();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The eexec cipher is its own inverse given the encryption routine, so a
    /// hand-rolled encryptor validates the decryptor.
    fn eexec_encrypt(plain: &[u8], key: u16, pad: usize) -> Vec<u8> {
        const C1: u16 = 52845;
        const C2: u16 = 22719;
        let mut r = key;
        let mut out = Vec::new();
        let padded: Vec<u8> = std::iter::repeat(0x00).take(pad).chain(plain.iter().copied()).collect();
        for &p in &padded {
            let c = p ^ (r >> 8) as u8;
            r = (c as u16).wrapping_add(r).wrapping_mul(C1).wrapping_add(C2);
            out.push(c);
        }
        out
    }

    #[test]
    fn eexec_roundtrip() {
        let plain = b"/CharStrings 1 dict dup begin";
        let enc = eexec_encrypt(plain, 55665, 4);
        assert_eq!(eexec_decrypt(&enc, 55665, 4), plain.to_vec());
    }

    /// Builds a minimal Type 1 font with one square glyph and checks that the
    /// parser and interpreter reproduce it.
    #[test]
    fn parses_and_outlines_a_square() {
        // hsbw 0 600, then a 100..500 square via rmoveto/rlineto, closepath, endchar.
        let mut cs: Vec<u8> = Vec::new();
        let num = |v: i32, out: &mut Vec<u8>| {
            // Use the 5-byte form for simplicity and exactness.
            out.push(255);
            out.extend_from_slice(&v.to_be_bytes());
        };
        num(0, &mut cs);
        num(600, &mut cs);
        cs.push(13); // hsbw
        num(100, &mut cs);
        num(100, &mut cs);
        cs.push(21); // rmoveto
        num(400, &mut cs);
        num(0, &mut cs);
        cs.push(5); // rlineto
        num(0, &mut cs);
        num(400, &mut cs);
        cs.push(5);
        num(-400, &mut cs);
        num(0, &mut cs);
        cs.push(5);
        cs.push(9); // closepath
        cs.push(14); // endchar

        let enc_cs = eexec_encrypt(&cs, 4330, 4);
        let mut private: Vec<u8> = Vec::new();
        private.extend_from_slice(b"/lenIV 4 def\n/CharStrings 1 dict dup begin\n/square ");
        private.extend_from_slice(enc_cs.len().to_string().as_bytes());
        private.extend_from_slice(b" RD ");
        private.extend_from_slice(&enc_cs);
        private.extend_from_slice(b" ND\nend\n");

        let mut font: Vec<u8> = Vec::new();
        font.extend_from_slice(b"%!PS-AdobeFont-1.0\n/FontMatrix [0.001 0 0 0.001 0 0] readonly def\n");
        font.extend_from_slice(b"/Encoding 256 array\ndup 97 /square put\nreadonly def\n");
        font.extend_from_slice(b"currentdict end\ncurrentfile eexec\n");
        font.extend_from_slice(&eexec_encrypt(&private, 55665, 4));

        let f = Type1Font::parse(&font, None).expect("parse type1");
        assert!(f.has_glyph("square"), "glyph names: {:?}", f.glyph_names().collect::<Vec<_>>());
        assert_eq!(f.encoding[97].as_deref(), Some("square"));
        assert!((f.font_matrix.a - 0.001).abs() < 1e-12);

        let (path, adv) = f.outline("square").expect("outline");
        assert_eq!(adv, 600.0);
        let (x0, y0, x1, y1) = path.bounds().expect("bounds");
        assert!((x0 - 100.0).abs() < 1e-6, "x0 {x0}");
        assert!((y0 - 100.0).abs() < 1e-6, "y0 {y0}");
        assert!((x1 - 500.0).abs() < 1e-6, "x1 {x1}");
        assert!((y1 - 500.0).abs() < 1e-6, "y1 {y1}");
    }

    #[test]
    fn small_number_encodings() {
        // 139 encodes 0; 247 0 encodes 108; 251 0 encodes -108.
        let mut cs = vec![139u8, 247, 0, 13u8]; // 0 108 hsbw
        cs.push(14);
        let enc_cs = eexec_encrypt(&cs, 4330, 4);
        let mut private: Vec<u8> = Vec::new();
        private.extend_from_slice(b"/CharStrings 1 dict dup begin\n/a ");
        private.extend_from_slice(enc_cs.len().to_string().as_bytes());
        private.extend_from_slice(b" RD ");
        private.extend_from_slice(&enc_cs);
        private.extend_from_slice(b" ND\nend\n");
        let mut font: Vec<u8> = b"%!PS\ncurrentfile eexec\n".to_vec();
        font.extend_from_slice(&eexec_encrypt(&private, 55665, 4));

        let f = Type1Font::parse(&font, None).expect("parse");
        let (_, adv) = f.outline("a").expect("outline");
        assert_eq!(adv, 108.0);
    }
}
