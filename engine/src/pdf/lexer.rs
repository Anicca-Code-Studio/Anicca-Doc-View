//! PDF syntax tokenizer and direct-object parser (ISO 32000-1 clause 7.2/7.3).
//!
//! The lexer never panics on malformed input: every read is bounds-checked and
//! anything unrecognizable yields `None` so callers can fall back to recovery.

use std::collections::HashMap;
use std::rc::Rc;

use super::object::{Dict, Obj};

/// Nesting cap for arrays/dictionaries. Deep nesting is always either broken
/// input or an attack; real documents stay far below this.
const MAX_DEPTH: usize = 64;

#[inline]
pub fn is_ws(b: u8) -> bool {
    matches!(b, 0x00 | 0x09 | 0x0a | 0x0c | 0x0d | 0x20)
}

#[inline]
pub fn is_delim(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

#[inline]
pub fn is_regular(b: u8) -> bool {
    !is_ws(b) && !is_delim(b)
}

#[inline]
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub struct Lexer<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8]) -> Lexer<'a> {
        Lexer { data, pos: 0 }
    }

    pub fn at(data: &'a [u8], pos: usize) -> Lexer<'a> {
        Lexer { data, pos: pos.min(data.len()) }
    }

    #[inline]
    pub fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    #[inline]
    pub fn peek_at(&self, off: usize) -> Option<u8> {
        self.data.get(self.pos + off).copied()
    }

    #[inline]
    pub fn eof(&self) -> bool {
        self.pos >= self.data.len()
    }

    /// Skips whitespace and `%` comments.
    pub fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if is_ws(b) {
                self.pos += 1;
            } else if b == b'%' {
                while let Some(c) = self.peek() {
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    /// Advances past a single end-of-line marker (CR, LF, or CRLF).
    pub fn skip_eol(&mut self) {
        if self.peek() == Some(b'\r') {
            self.pos += 1;
        }
        if self.peek() == Some(b'\n') {
            self.pos += 1;
        }
    }

    /// Reads a run of regular characters (a keyword or a bare number).
    pub fn read_keyword(&mut self) -> &'a [u8] {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if is_regular(b) {
                self.pos += 1;
            } else {
                break;
            }
        }
        &self.data[start..self.pos]
    }

    /// Consumes `kw` if it is the next token. Returns false and leaves the
    /// position untouched otherwise.
    pub fn expect_keyword(&mut self, kw: &[u8]) -> bool {
        let save = self.pos;
        self.skip_ws();
        if self.read_keyword() == kw {
            true
        } else {
            self.pos = save;
            false
        }
    }

    /// Reads `/Name`, decoding `#xx` escapes. Assumes the `/` is next.
    fn read_name(&mut self) -> Obj {
        self.pos += 1; // '/'
        let mut out = Vec::new();
        while let Some(b) = self.peek() {
            if !is_regular(b) {
                break;
            }
            self.pos += 1;
            if b == b'#' {
                let hi = self.peek().and_then(hex_val);
                let lo = self.peek_at(1).and_then(hex_val);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push(h * 16 + l);
                    self.pos += 2;
                    continue;
                }
            }
            out.push(b);
        }
        Obj::Name(Rc::new(String::from_utf8_lossy(&out).into_owned()))
    }

    /// Reads a `(...)` literal string with balanced parens and escapes.
    fn read_literal_string(&mut self) -> Obj {
        self.pos += 1; // '('
        let mut out: Vec<u8> = Vec::new();
        let mut depth = 1usize;
        while let Some(b) = self.peek() {
            self.pos += 1;
            match b {
                b'(' => {
                    depth += 1;
                    out.push(b);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(b);
                }
                b'\\' => {
                    let e = match self.peek() {
                        Some(e) => e,
                        None => break,
                    };
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'(' | b')' | b'\\' => out.push(e),
                        b'\r' => {
                            // Line continuation: swallow the EOL.
                            if self.peek() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        b'0'..=b'7' => {
                            let mut v = (e - b'0') as u32;
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + (d - b'0') as u32;
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push((v & 0xff) as u8);
                        }
                        // Unknown escape: the backslash is dropped.
                        _ => out.push(e),
                    }
                }
                b'\r' => {
                    // A bare EOL inside a string means LF.
                    if self.peek() == Some(b'\n') {
                        self.pos += 1;
                    }
                    out.push(b'\n');
                }
                _ => out.push(b),
            }
        }
        Obj::Str(Rc::new(out))
    }

    /// Reads a `<...>` hex string. Assumes `<` is next and not `<<`.
    fn read_hex_string(&mut self) -> Obj {
        self.pos += 1; // '<'
        let mut out: Vec<u8> = Vec::new();
        let mut hi: Option<u8> = None;
        while let Some(b) = self.peek() {
            self.pos += 1;
            if b == b'>' {
                break;
            }
            let v = match hex_val(b) {
                Some(v) => v,
                None => continue, // whitespace and junk are ignored
            };
            match hi {
                None => hi = Some(v),
                Some(h) => {
                    out.push(h * 16 + v);
                    hi = None;
                }
            }
        }
        // An odd trailing digit is padded with zero.
        if let Some(h) = hi {
            out.push(h * 16);
        }
        Obj::Str(Rc::new(out))
    }

    /// Parses a number token. Lenient about the malformed forms real-world
    /// producers emit (`--5`, `4.`, `.5`, `6.-2`).
    fn read_number(&mut self) -> Obj {
        let start = self.pos;
        let mut is_real = false;
        while let Some(b) = self.peek() {
            match b {
                b'0'..=b'9' | b'+' | b'-' => self.pos += 1,
                b'.' => {
                    is_real = true;
                    self.pos += 1;
                }
                _ => break,
            }
        }
        let tok = &self.data[start..self.pos];
        parse_number(tok, is_real)
    }

    /// Parses one direct object. Returns `None` at a keyword that is not a
    /// value (`endobj`, `stream`, an operator, …) without consuming it.
    pub fn parse_obj(&mut self) -> Option<Obj> {
        self.parse_obj_depth(0)
    }

    fn parse_obj_depth(&mut self, depth: usize) -> Option<Obj> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.skip_ws();
        let b = self.peek()?;
        match b {
            b'/' => Some(self.read_name()),
            b'(' => Some(self.read_literal_string()),
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    match self.peek() {
                        None => break,
                        Some(b']') => {
                            self.pos += 1;
                            break;
                        }
                        _ => {}
                    }
                    let before = self.pos;
                    match self.parse_obj_depth(depth + 1) {
                        Some(o) => items.push(o),
                        None => {
                            // Junk inside an array: skip one token and retry so
                            // a single bad entry cannot hang the parse.
                            if self.pos == before {
                                self.pos += 1;
                            }
                        }
                    }
                }
                Some(Obj::Array(Rc::new(items)))
            }
            b'<' => {
                if self.peek_at(1) == Some(b'<') {
                    self.pos += 2;
                    let d = self.parse_dict_body(depth + 1);
                    Some(Obj::Dict(Rc::new(d)))
                } else {
                    Some(self.read_hex_string())
                }
            }
            b'>' | b']' | b'}' | b')' => None,
            b'0'..=b'9' | b'+' | b'-' | b'.' => {
                let save = self.pos;
                let num = self.read_number();
                // `int int R` is an indirect reference.
                if let Obj::Int(n) = num {
                    if n >= 0 {
                        let after_num = self.pos;
                        self.skip_ws();
                        if matches!(self.peek(), Some(b'0'..=b'9')) {
                            let gen_start = self.pos;
                            let gen = self.read_number();
                            if let Obj::Int(g) = gen {
                                if (0..=65535).contains(&g) {
                                    self.skip_ws();
                                    let kw_start = self.pos;
                                    if self.read_keyword() == b"R" {
                                        return Some(Obj::Ref(n as u32, g as u16));
                                    }
                                    self.pos = kw_start;
                                }
                            }
                            self.pos = gen_start;
                        }
                        self.pos = after_num;
                    }
                }
                let _ = save;
                Some(num)
            }
            b'{' | b'}' => {
                // PostScript calculator braces; not a value here.
                None
            }
            _ => {
                let save = self.pos;
                let kw = self.read_keyword();
                match kw {
                    b"true" => Some(Obj::Bool(true)),
                    b"false" => Some(Obj::Bool(false)),
                    b"null" => Some(Obj::Null),
                    _ => {
                        self.pos = save;
                        None
                    }
                }
            }
        }
    }

    /// Parses dictionary entries up to and including the closing `>>`.
    /// Assumes the opening `<<` is already consumed.
    pub fn parse_dict_body(&mut self, depth: usize) -> Dict {
        let mut dict: Dict = HashMap::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => break,
                Some(b'>') => {
                    self.pos += 1;
                    if self.peek() == Some(b'>') {
                        self.pos += 1;
                    }
                    break;
                }
                Some(b'/') => {}
                Some(_) => {
                    // Key slot holds something that is not a name: skip a token
                    // rather than spin.
                    let before = self.pos;
                    if self.parse_obj_depth(depth + 1).is_none() && self.pos == before {
                        self.pos += 1;
                    }
                    continue;
                }
            }
            let key = match self.read_name() {
                Obj::Name(n) => n,
                _ => continue,
            };
            let before = self.pos;
            match self.parse_obj_depth(depth + 1) {
                Some(v) => {
                    dict.insert(key.as_ref().clone(), v);
                }
                None => {
                    if self.pos == before {
                        // Value slot is a stray keyword like `endobj`; stop.
                        break;
                    }
                }
            }
        }
        dict
    }
}

fn parse_number(tok: &[u8], is_real: bool) -> Obj {
    let s = String::from_utf8_lossy(tok);
    if !is_real {
        if let Ok(v) = s.parse::<i64>() {
            return Obj::Int(v);
        }
    }
    if let Ok(v) = s.parse::<f64>() {
        return if is_real { Obj::Real(v) } else { Obj::Int(v as i64) };
    }
    // Salvage forms like `--5`, `4.`, `.5`, `6.-2`. Leading sign characters are
    // collapsed (Acrobat reads `--5` as -5); digits and the first dot are kept;
    // everything after the first trailing junk character is dropped.
    let mut cleaned = String::new();
    let mut neg = false;
    let mut seen_digit = false;
    let mut seen_dot = false;
    for c in s.chars() {
        match c {
            '-' if !seen_digit && !seen_dot => neg = true,
            '+' if !seen_digit && !seen_dot => {}
            '0'..='9' => {
                seen_digit = true;
                cleaned.push(c);
            }
            '.' if !seen_dot => {
                seen_dot = true;
                cleaned.push('.');
            }
            _ => break,
        }
    }
    if cleaned.is_empty() || cleaned == "." {
        return Obj::Int(0);
    }
    match cleaned.parse::<f64>() {
        Ok(v) => {
            let v = if neg { -v } else { v };
            if is_real || seen_dot {
                Obj::Real(v)
            } else {
                Obj::Int(v as i64)
            }
        }
        Err(_) => Obj::Int(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Option<Obj> {
        Lexer::new(src.as_bytes()).parse_obj()
    }

    #[test]
    fn numbers() {
        assert!(matches!(parse("42"), Some(Obj::Int(42))));
        assert!(matches!(parse("-3.5"), Some(Obj::Real(v)) if (v + 3.5).abs() < 1e-9));
        assert!(matches!(parse(".5"), Some(Obj::Real(v)) if (v - 0.5).abs() < 1e-9));
        // Malformed but seen in the wild.
        assert!(matches!(parse("--5"), Some(Obj::Int(-5)) | Some(Obj::Real(_))));
    }

    #[test]
    fn reference_vs_integers() {
        assert!(matches!(parse("12 0 R"), Some(Obj::Ref(12, 0))));
        let a = parse("[1 2 3]").unwrap();
        assert_eq!(a.as_array().unwrap().len(), 3);
        // `1 2` inside an array must stay two integers, not become a ref.
        let a = parse("[1 0 R 4]").unwrap();
        let items = a.as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert!(matches!(items[0], Obj::Ref(1, 0)));
    }

    #[test]
    fn names_with_escapes() {
        let n = parse("/A#20B").unwrap();
        assert_eq!(n.as_name(), Some("A B"));
    }

    #[test]
    fn literal_string_escapes() {
        let s = parse(r"(a\(b\)c\n\101)").unwrap();
        assert_eq!(s.as_str_bytes(), Some(&b"a(b)c\nA"[..]));
    }

    #[test]
    fn hex_string_odd_digit() {
        let s = parse("<48656C6C6F2>").unwrap();
        assert_eq!(s.as_str_bytes(), Some(&b"Hello "[..]));
    }

    #[test]
    fn nested_dict() {
        let d = parse("<< /Type /Page /MediaBox [0 0 612 792] /Sub << /A 1 >> >>").unwrap();
        assert_eq!(d.get("Type").and_then(|o| o.as_name()), Some("Page"));
        assert_eq!(d.get("Sub").and_then(|o| o.get("A")).and_then(|o| o.as_i64()), Some(1));
    }

    #[test]
    fn comments_are_whitespace() {
        let d = parse("<< /A % comment here\n 5 >>").unwrap();
        assert_eq!(d.get("A").and_then(|o| o.as_i64()), Some(5));
    }
}
