//! Stream filters (ISO 32000-1 clause 7.4) and the predictor post-processing
//! shared by Flate and LZW.
//!
//! Image-only filters (`DCTDecode`, `JPXDecode`, `CCITTFaxDecode`,
//! `JBIG2Decode`) are *not* decoded here: they stay encoded and are handed to
//! the image decoder, which needs the colour-space context to interpret them.

use super::object::{Dict, Obj};

/// A filter that this module leaves for the image decoder to handle.
pub fn is_image_filter(name: &str) -> bool {
    matches!(
        name,
        "DCTDecode" | "DCT" | "JPXDecode" | "CCITTFaxDecode" | "CCF" | "JBIG2Decode"
    )
}

// ── Flate ────────────────────────────────────────────────────────────────────

pub fn flate_decode(data: &[u8]) -> Option<Vec<u8>> {
    // Some producers emit leading whitespace, or a corrupt first byte, before
    // the zlib header. Try the data as-is, then zlib-skipping variants, then
    // raw deflate.
    if let Some(v) = try_zlib(data) {
        return Some(v);
    }
    let trimmed = {
        let mut i = 0usize;
        while i < data.len() && super::lexer::is_ws(data[i]) {
            i += 1;
        }
        &data[i..]
    };
    if trimmed.len() != data.len() {
        if let Some(v) = try_zlib(trimmed) {
            return Some(v);
        }
    }
    if let Some(v) = try_raw_deflate(trimmed) {
        return Some(v);
    }
    // Last resort: a damaged first byte is common; retry one byte in.
    if trimmed.len() > 1 {
        if let Some(v) = try_zlib(&trimmed[1..]) {
            return Some(v);
        }
    }
    None
}

fn try_zlib(data: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    let mut d = flate2::read::ZlibDecoder::new(data);
    match d.read_to_end(&mut out) {
        Ok(_) => Some(out),
        // Truncated streams are common; keep whatever inflated cleanly.
        Err(_) if !out.is_empty() => Some(out),
        Err(_) => None,
    }
}

fn try_raw_deflate(data: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    let mut d = flate2::read::DeflateDecoder::new(data);
    match d.read_to_end(&mut out) {
        Ok(_) => Some(out),
        Err(_) if !out.is_empty() => Some(out),
        Err(_) => None,
    }
}

// ── LZW ──────────────────────────────────────────────────────────────────────

/// LZW as used by PDF: 8-bit input, codes 9..12 bits, 256 = clear, 257 = EOD.
/// `early` (the `/EarlyChange` parameter, default 1) bumps the code width one
/// code sooner.
pub fn lzw_decode(data: &[u8], early: bool) -> Vec<u8> {
    const CLEAR: u16 = 256;
    const EOD: u16 = 257;

    let mut out: Vec<u8> = Vec::new();
    // Dictionary entries above 257; each is (prefix_code, last_byte).
    let mut table: Vec<(u16, u8)> = Vec::with_capacity(4096);
    let mut width = 9u32;
    let mut prev: Option<u16> = None;

    let reset = |table: &mut Vec<(u16, u8)>, width: &mut u32, prev: &mut Option<u16>| {
        table.clear();
        *width = 9;
        *prev = None;
    };

    // Expands a code into bytes by walking the prefix chain backwards.
    fn expand(code: u16, table: &[(u16, u8)], buf: &mut Vec<u8>) -> bool {
        buf.clear();
        let mut c = code;
        let mut guard = 0usize;
        loop {
            if c < 256 {
                buf.push(c as u8);
                break;
            }
            let idx = match (c as usize).checked_sub(258) {
                Some(i) if i < table.len() => i,
                _ => return false,
            };
            let (prefix, last) = table[idx];
            buf.push(last);
            c = prefix;
            guard += 1;
            if guard > 4096 {
                return false;
            }
        }
        buf.reverse();
        true
    }

    let mut bitbuf: u32 = 0;
    let mut bits: u32 = 0;
    let mut pos = 0usize;
    let mut cur: Vec<u8> = Vec::new();
    let mut prev_bytes: Vec<u8> = Vec::new();

    loop {
        while bits < width {
            match data.get(pos) {
                Some(&b) => {
                    bitbuf = (bitbuf << 8) | b as u32;
                    bits += 8;
                    pos += 1;
                }
                None => return out,
            }
        }
        let code = ((bitbuf >> (bits - width)) & ((1u32 << width) - 1)) as u16;
        bits -= width;

        if code == EOD {
            return out;
        }
        if code == CLEAR {
            reset(&mut table, &mut width, &mut prev);
            continue;
        }

        match prev {
            None => {
                if !expand(code, &table, &mut cur) {
                    return out;
                }
                out.extend_from_slice(&cur);
                prev_bytes = cur.clone();
            }
            Some(p) => {
                let known = (code as usize) < 256 || (code as usize).wrapping_sub(258) < table.len();
                if known {
                    if !expand(code, &table, &mut cur) {
                        return out;
                    }
                } else {
                    // KwKwK case: the code is the one about to be defined.
                    cur.clone_from(&prev_bytes);
                    match prev_bytes.first() {
                        Some(&f) => cur.push(f),
                        None => return out,
                    }
                }
                let first = match cur.first() {
                    Some(&f) => f,
                    None => return out,
                };
                if table.len() < 4096 - 258 {
                    table.push((p, first));
                }
                out.extend_from_slice(&cur);
                prev_bytes.clone_from(&cur);
            }
        }
        prev = Some(code);

        // Widen the code as the table fills.
        let next_code = table.len() + 258 + if early { 1 } else { 0 };
        width = match next_code {
            0..=511 => 9,
            512..=1023 => 10,
            1024..=2047 => 11,
            _ => 12,
        };
    }
}

// ── ASCII filters ────────────────────────────────────────────────────────────

pub fn ascii_hex_decode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut hi: Option<u8> = None;
    for &b in data {
        if b == b'>' {
            break;
        }
        let v = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => continue,
        };
        match hi {
            None => hi = Some(v),
            Some(h) => {
                out.push(h * 16 + v);
                hi = None;
            }
        }
    }
    if let Some(h) = hi {
        out.push(h * 16);
    }
    out
}

pub fn ascii85_decode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut tuple = [0u8; 5];
    let mut n = 0usize;
    let mut i = 0usize;
    // An optional `<~` prefix is allowed.
    if data.len() >= 2 && data[0] == b'<' && data[1] == b'~' {
        i = 2;
    }
    while i < data.len() {
        let b = data[i];
        i += 1;
        if b == b'~' {
            break;
        }
        if super::lexer::is_ws(b) {
            continue;
        }
        if b == b'z' && n == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        if !(b'!'..=b'u').contains(&b) {
            continue;
        }
        tuple[n] = b - b'!';
        n += 1;
        if n == 5 {
            let mut v: u32 = 0;
            for &t in &tuple {
                v = v.wrapping_mul(85).wrapping_add(t as u32);
            }
            out.extend_from_slice(&v.to_be_bytes());
            n = 0;
        }
    }
    // Flush a partial group: pad with 'u' (84) and drop the padding bytes.
    if n > 1 {
        let mut v: u32 = 0;
        for j in 0..5 {
            let t = if j < n { tuple[j] } else { 84 };
            v = v.wrapping_mul(85).wrapping_add(t as u32);
        }
        let bytes = v.to_be_bytes();
        out.extend_from_slice(&bytes[..n - 1]);
    }
    out
}

pub fn run_length_decode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        let len = data[i];
        i += 1;
        if len == 128 {
            break;
        }
        if len < 128 {
            let n = len as usize + 1;
            let end = (i + n).min(data.len());
            out.extend_from_slice(&data[i..end]);
            i = end;
        } else {
            let n = 257 - len as usize;
            if let Some(&b) = data.get(i) {
                out.extend(std::iter::repeat(b).take(n));
            }
            i += 1;
        }
    }
    out
}

// ── predictors ───────────────────────────────────────────────────────────────

/// Undoes the `/Predictor` transform applied before Flate/LZW compression.
/// Predictor 1 is a no-op, 2 is the TIFF horizontal differencer, 10..15 are the
/// PNG filters (the per-row filter type byte selects among them).
pub fn apply_predictor(
    data: Vec<u8>,
    predictor: i64,
    colors: usize,
    bpc: usize,
    columns: usize,
) -> Vec<u8> {
    if predictor < 2 {
        return data;
    }
    let colors = colors.max(1);
    let bpc = if bpc == 0 { 8 } else { bpc };
    let columns = columns.max(1);

    if predictor == 2 {
        return tiff_predictor(data, colors, bpc, columns);
    }

    // PNG predictors: each row is preceded by a filter-type byte.
    let bpp = ((colors * bpc) + 7) / 8; // bytes per pixel, min 1
    let bpp = bpp.max(1);
    let row_len = (columns * colors * bpc + 7) / 8;
    if row_len == 0 {
        return data;
    }
    let stride = row_len + 1;
    let rows = data.len() / stride;
    let mut out = Vec::with_capacity(rows * row_len);
    let mut prev_row = vec![0u8; row_len];
    let mut row = vec![0u8; row_len];

    for r in 0..rows {
        let base = r * stride;
        let ft = data[base];
        row.copy_from_slice(&data[base + 1..base + 1 + row_len]);
        match ft {
            0 => {}
            1 => {
                for i in bpp..row_len {
                    row[i] = row[i].wrapping_add(row[i - bpp]);
                }
            }
            2 => {
                for i in 0..row_len {
                    row[i] = row[i].wrapping_add(prev_row[i]);
                }
            }
            3 => {
                for i in 0..row_len {
                    let left = if i >= bpp { row[i - bpp] as u16 } else { 0 };
                    let up = prev_row[i] as u16;
                    row[i] = row[i].wrapping_add(((left + up) / 2) as u8);
                }
            }
            4 => {
                for i in 0..row_len {
                    let a = if i >= bpp { row[i - bpp] as i16 } else { 0 };
                    let b = prev_row[i] as i16;
                    let c = if i >= bpp { prev_row[i - bpp] as i16 } else { 0 };
                    let p = a + b - c;
                    let pa = (p - a).abs();
                    let pb = (p - b).abs();
                    let pc = (p - c).abs();
                    let pred = if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    };
                    row[i] = row[i].wrapping_add(pred as u8);
                }
            }
            // Unknown filter type: leave the row untouched.
            _ => {}
        }
        out.extend_from_slice(&row);
        prev_row.copy_from_slice(&row);
    }
    out
}

fn tiff_predictor(mut data: Vec<u8>, colors: usize, bpc: usize, columns: usize) -> Vec<u8> {
    if bpc != 8 {
        // Sub-byte TIFF prediction is vanishingly rare; pass through.
        return data;
    }
    let row_len = columns * colors;
    if row_len == 0 {
        return data;
    }
    let rows = data.len() / row_len;
    for r in 0..rows {
        let base = r * row_len;
        for i in colors..row_len {
            data[base + i] = data[base + i].wrapping_add(data[base + i - colors]);
        }
    }
    data
}

// ── filter chain ─────────────────────────────────────────────────────────────

/// Result of running a stream's filter chain.
pub struct Decoded {
    pub data: Vec<u8>,
    /// Set when decoding stopped at an image filter: its name and its decode
    /// parms, for the image decoder to finish.
    pub image_filter: Option<(String, Dict)>,
}

/// Applies every non-image filter in `filters` to `data`, in order. Stops at
/// the first image filter and reports it.
pub fn decode_chain(
    mut data: Vec<u8>,
    filters: &[String],
    parms: &[Option<Dict>],
) -> Decoded {
    for (i, name) in filters.iter().enumerate() {
        let parm = parms.get(i).and_then(|p| p.as_ref());
        if is_image_filter(name) {
            return Decoded {
                data,
                image_filter: Some((name.clone(), parm.cloned().unwrap_or_default())),
            };
        }
        data = apply_one(data, name, parm);
    }
    Decoded { data, image_filter: None }
}

fn parm_i64(parm: Option<&Dict>, key: &str, default: i64) -> i64 {
    parm.and_then(|d| d.get(key)).and_then(|o| o.as_i64()).unwrap_or(default)
}

fn apply_one(data: Vec<u8>, name: &str, parm: Option<&Dict>) -> Vec<u8> {
    let decoded = match name {
        "FlateDecode" | "Fl" => flate_decode(&data).unwrap_or_default(),
        "LZWDecode" | "LZW" => {
            let early = parm_i64(parm, "EarlyChange", 1) != 0;
            lzw_decode(&data, early)
        }
        "ASCIIHexDecode" | "AHx" => return ascii_hex_decode(&data),
        "ASCII85Decode" | "A85" => return ascii85_decode(&data),
        "RunLengthDecode" | "RL" => return run_length_decode(&data),
        // Crypt filters are handled by the security handler, not here.
        "Crypt" => return data,
        _ => return data,
    };
    let predictor = parm_i64(parm, "Predictor", 1);
    if predictor < 2 {
        return decoded;
    }
    apply_predictor(
        decoded,
        predictor,
        parm_i64(parm, "Colors", 1).max(1) as usize,
        parm_i64(parm, "BitsPerComponent", 8).max(1) as usize,
        parm_i64(parm, "Columns", 1).max(1) as usize,
    )
}

/// Reads `/Filter` into a list of names. Accepts a single name or an array.
pub fn filter_names(obj: Option<&Obj>) -> Vec<String> {
    match obj {
        Some(Obj::Name(n)) => vec![n.as_ref().clone()],
        Some(Obj::Array(a)) => a.iter().filter_map(|o| o.as_name().map(|s| s.to_string())).collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_hex() {
        assert_eq!(ascii_hex_decode(b"48 65 6C 6C 6F>"), b"Hello".to_vec());
        assert_eq!(ascii_hex_decode(b"4>"), vec![0x40]);
    }

    #[test]
    fn ascii85_vectors() {
        // 'z' is the shorthand for four zero bytes.
        assert_eq!(ascii85_decode(b"z~>"), vec![0, 0, 0, 0]);
        // Classic full group: "Man " <-> "9jqo^".
        assert_eq!(ascii85_decode(b"9jqo^~>"), b"Man ".to_vec());
        // Partial group: 4 characters decode to 3 bytes.
        assert_eq!(ascii85_decode(b"9jqo~>"), b"Man".to_vec());
        // The `<~` prefix is optional and whitespace is ignored.
        assert_eq!(ascii85_decode(b"<~9jq\n o^~>"), b"Man ".to_vec());
    }

    #[test]
    fn run_length() {
        // literal run of 3, then a repeat of 4 'A', then EOD
        let src = [2u8, b'a', b'b', b'c', 253, b'A', 128];
        assert_eq!(run_length_decode(&src), b"abcAAAA".to_vec());
    }

    #[test]
    fn png_up_predictor() {
        // 2 rows, 3 columns, 1 color, 8 bpc; filter type 2 (Up) on row 2.
        let data = vec![0, 10, 20, 30, 2, 1, 2, 3];
        let out = apply_predictor(data, 12, 1, 8, 3);
        assert_eq!(out, vec![10, 20, 30, 11, 22, 33]);
    }

    #[test]
    fn tiff_predictor_horizontal() {
        // 1 row, 4 columns, 1 color: deltas 10,1,1,1 -> 10,11,12,13
        let data = vec![10, 1, 1, 1];
        let out = apply_predictor(data, 2, 1, 8, 4);
        assert_eq!(out, vec![10, 11, 12, 13]);
    }

    #[test]
    fn flate_roundtrip() {
        use std::io::Write;
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(b"anicca engine pdf").unwrap();
        let comp = e.finish().unwrap();
        assert_eq!(flate_decode(&comp).unwrap(), b"anicca engine pdf".to_vec());
    }

    /// Reference LZW encoder, used only to round-trip the decoder.
    ///
    /// The decoder widens its code size one step later than the encoder does,
    /// because its table lags by one entry; `/EarlyChange` is exactly that
    /// offset. Mirroring the rule here (encoder offset = decoder offset - 1)
    /// keeps the two in lockstep across width changes.
    fn lzw_encode(input: &[u8], early: bool) -> Vec<u8> {
        use std::collections::HashMap;

        let mut out: Vec<u8> = Vec::new();
        let mut acc: u32 = 0;
        let mut nbits: u32 = 0;
        let mut width: u32 = 9;
        let mut dict: HashMap<Vec<u8>, u16> = HashMap::new();
        let mut next_code: u16 = 258;

        fn push(code: u16, width: u32, acc: &mut u32, nbits: &mut u32, out: &mut Vec<u8>) {
            *acc = (*acc << width) | code as u32;
            *nbits += width;
            while *nbits >= 8 {
                *nbits -= 8;
                out.push(((*acc >> *nbits) & 0xff) as u8);
            }
        }
        fn width_for(entries: usize, early: bool) -> u32 {
            let n = 258i64 + entries as i64 + if early { 0 } else { -1 };
            match n {
                i64::MIN..=511 => 9,
                512..=1023 => 10,
                1024..=2047 => 11,
                _ => 12,
            }
        }

        push(256, width, &mut acc, &mut nbits, &mut out);
        if input.is_empty() {
            push(257, width, &mut acc, &mut nbits, &mut out);
            if nbits > 0 {
                out.push(((acc << (8 - nbits)) & 0xff) as u8);
            }
            return out;
        }

        let mut w: Vec<u8> = vec![input[0]];
        for &c in &input[1..] {
            let mut wc = w.clone();
            wc.push(c);
            if wc.len() == 1 || dict.contains_key(&wc) {
                w = wc;
                continue;
            }
            let code = if w.len() == 1 { w[0] as u16 } else { dict[&w] };
            push(code, width, &mut acc, &mut nbits, &mut out);
            if next_code < 4095 {
                dict.insert(wc, next_code);
                next_code += 1;
                width = width_for(dict.len(), early);
            }
            w = vec![c];
        }
        let code = if w.len() == 1 { w[0] as u16 } else { dict[&w] };
        push(code, width, &mut acc, &mut nbits, &mut out);
        push(257, width, &mut acc, &mut nbits, &mut out);
        if nbits > 0 {
            out.push(((acc << (8 - nbits)) & 0xff) as u8);
        }
        out
    }

    #[test]
    fn lzw_roundtrip_short() {
        // Contains the KwKwK case (a code used in the same step it is defined).
        let src = vec![45u8, 45, 45, 66, 66, 66, 66, 66, 66];
        assert_eq!(lzw_decode(&lzw_encode(&src, true), true), src);
        assert_eq!(lzw_decode(&lzw_encode(&src, false), false), src);
    }

    #[test]
    fn lzw_roundtrip_across_width_changes() {
        // Long enough to push the code width from 9 to 10 and beyond.
        let mut src = Vec::new();
        for i in 0..4000u32 {
            src.push((i % 251) as u8);
            src.push((i / 251) as u8);
        }
        assert_eq!(lzw_decode(&lzw_encode(&src, true), true), src);
        assert_eq!(lzw_decode(&lzw_encode(&src, false), false), src);
    }
}
