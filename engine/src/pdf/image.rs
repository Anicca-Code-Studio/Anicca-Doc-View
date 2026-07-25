//! Image XObjects and inline images (ISO 32000-1 clause 8.9).
//!
//! Produces a straight RGBA bitmap that the rasterizer can blit through any
//! transform. Sample unpacking covers 1/2/4/8/16 bits per component, the
//! `/Decode` array, stencil masks, soft masks and colour-key masking.

use crate::raster::Bitmap;

use super::colorspace::{self, ColorSpace};
use super::filter;
use super::object::{Dict, Obj, Stream};
use super::xref::PdfFile;

/// Refuse absurd allocations from corrupt or hostile dimensions.
const MAX_PIXELS: usize = 64_000_000;

/// Decodes an image XObject (or an inline image) into RGBA.
///
/// `fill_rgb` is the current fill colour, which stencil masks paint with.
pub fn decode(
    file: &PdfFile,
    stream: &Stream,
    resources: &Obj,
    fill_rgb: [u8; 3],
) -> Option<Bitmap> {
    let dict = &stream.dict;
    let w = int_of(file, dict, &["Width", "W"])? as usize;
    let h = int_of(file, dict, &["Height", "H"])? as usize;
    if w == 0 || h == 0 || w.saturating_mul(h) > MAX_PIXELS {
        return None;
    }

    let is_stencil = bool_of(file, dict, &["ImageMask", "IM"]).unwrap_or(false);
    let decoded = file.decode_stream(stream);

    // Filters that carry their own pixel format.
    if let Some((name, _parms)) = &decoded.image_filter {
        return match name.as_str() {
            "DCTDecode" | "DCT" => {
                let mut bmp = decode_jpeg(&decoded.data, w, h, file, dict, resources)?;
                apply_masks(file, dict, resources, &mut bmp);
                Some(bmp)
            }
            // JPEG 2000, fax and JBIG2 need their own decoders; skipping the
            // image is better than painting a wrong one.
            _ => None,
        };
    }

    // A colour-key mask names source-sample ranges that become transparent, so
    // it has to be applied while the samples are still unpacked.
    let color_key: Option<Vec<i64>> = match file.dget(dict, "Mask") {
        Some(Obj::Array(a)) => Some(a.iter().filter_map(|o| o.as_i64()).collect()),
        _ => None,
    };

    let mut bmp = if is_stencil {
        decode_stencil(&decoded.data, w, h, file, dict, fill_rgb)?
    } else {
        decode_samples(&decoded.data, w, h, file, dict, resources, color_key.as_deref())?
    };
    apply_masks(file, dict, resources, &mut bmp);
    Some(bmp)
}

/// Alpha-only decode, used for `/SMask` and stencil `/Mask` entries.
fn decode_alpha(file: &PdfFile, stream: &Stream, resources: &Obj, stencil_invert: bool) -> Option<(usize, usize, Vec<u8>)> {
    let dict = &stream.dict;
    let w = int_of(file, dict, &["Width", "W"])? as usize;
    let h = int_of(file, dict, &["Height", "H"])? as usize;
    if w == 0 || h == 0 || w.saturating_mul(h) > MAX_PIXELS {
        return None;
    }
    let is_stencil = bool_of(file, dict, &["ImageMask", "IM"]).unwrap_or(false);
    let decoded = file.decode_stream(stream);
    if decoded.image_filter.is_some() {
        // A JPEG soft mask: take its luminance.
        let bmp = decode_jpeg(&decoded.data, w, h, file, dict, resources)?;
        let alpha = bmp
            .data
            .chunks_exact(4)
            .map(|p| ((p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8) as u8)
            .collect();
        return Some((w, h, alpha));
    }

    let bpc = if is_stencil {
        1
    } else {
        int_of(file, dict, &["BitsPerComponent", "BPC"]).unwrap_or(8).clamp(1, 16) as usize
    };
    // `/Decode [1 0]` swaps the ends of the range.
    let flipped = decode_array(file, dict)
        .map(|d| d.len() >= 2 && d[0] > d[1])
        .unwrap_or(false);
    let max = ((1u64 << bpc) - 1) as f32;

    let mut alpha = vec![0u8; w * h];
    let row_bytes = (w * bpc + 7) / 8;
    for y in 0..h {
        let row = decoded.data.get(y * row_bytes..).unwrap_or(&[]);
        let mut r = BitReader::new(row);
        for x in 0..w {
            let mut a = r.read(bpc) as f32 / max.max(1.0);
            if flipped {
                a = 1.0 - a;
            }
            if is_stencil {
                // In a `/Mask` stencil a 1 sample means "masked out", the
                // opposite of a stencil that paints.
                a = 1.0 - a;
                if stencil_invert {
                    a = 1.0 - a;
                }
            }
            alpha[y * w + x] = (a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
    }
    Some((w, h, alpha))
}

// ── sample decoding ──────────────────────────────────────────────────────────

fn decode_stencil(
    data: &[u8],
    w: usize,
    h: usize,
    file: &PdfFile,
    dict: &Dict,
    fill_rgb: [u8; 3],
) -> Option<Bitmap> {
    // Default `/Decode [0 1]` means sample 0 paints, 1 is transparent.
    let invert = decode_array(file, dict)
        .map(|d| d.first().copied().unwrap_or(0.0) > 0.5)
        .unwrap_or(false);

    let mut bmp = Bitmap::new(w, h);
    let row_bytes = (w + 7) / 8;
    for y in 0..h {
        let row = data.get(y * row_bytes..).unwrap_or(&[]);
        let mut r = BitReader::new(row);
        for x in 0..w {
            let bit = r.read(1) != 0;
            let paint = if invert { bit } else { !bit };
            let i = (y * w + x) * 4;
            bmp.data[i] = fill_rgb[0];
            bmp.data[i + 1] = fill_rgb[1];
            bmp.data[i + 2] = fill_rgb[2];
            bmp.data[i + 3] = if paint { 255 } else { 0 };
        }
    }
    Some(bmp)
}

fn decode_samples(
    data: &[u8],
    w: usize,
    h: usize,
    file: &PdfFile,
    dict: &Dict,
    resources: &Obj,
    color_key: Option<&[i64]>,
) -> Option<Bitmap> {
    let cs = match dict.get("ColorSpace").or_else(|| dict.get("CS")) {
        Some(o) => colorspace::parse(file, o, resources, 0),
        None => ColorSpace::DeviceGray,
    };
    let n = cs.n_comps();
    let bpc = int_of(file, dict, &["BitsPerComponent", "BPC"]).unwrap_or(8).clamp(1, 16) as usize;

    // Component ranges: the /Decode array, or the space's own default.
    let defaults = cs.default_decode(bpc);
    let decode_arr = decode_array(file, dict);
    let ranges: Vec<(f32, f32)> = (0..n)
        .map(|i| match &decode_arr {
            Some(d) if d.len() >= (i + 1) * 2 => (d[i * 2], d[i * 2 + 1]),
            _ => defaults.get(i).copied().unwrap_or((0.0, 1.0)),
        })
        .collect();

    let max = ((1u64 << bpc) - 1) as f32;
    let row_bits = w * n * bpc;
    let row_bytes = (row_bits + 7) / 8;

    let key = color_key.filter(|k| k.len() >= n * 2);

    let mut bmp = Bitmap::new(w, h);
    let mut comps = vec![0f32; n];
    let mut raws = vec![0i64; n];
    for y in 0..h {
        let row = data.get(y * row_bytes..).unwrap_or(&[]);
        let mut r = BitReader::new(row);
        for x in 0..w {
            for c in 0..n {
                let raw = r.read(bpc);
                raws[c] = raw as i64;
                // Interpolate the raw sample into the component's range. For an
                // Indexed space that range is 0..2^bpc-1, so this is identity.
                let (dmin, dmax) = ranges[c];
                comps[c] = dmin + raw as f32 * (dmax - dmin) / max.max(1.0);
            }
            let masked = key
                .map(|k| (0..n).all(|c| raws[c] >= k[c * 2] && raws[c] <= k[c * 2 + 1]))
                .unwrap_or(false);
            let rgb = cs.to_rgb(&comps);
            let i = (y * w + x) * 4;
            bmp.data[i] = rgb[0];
            bmp.data[i + 1] = rgb[1];
            bmp.data[i + 2] = rgb[2];
            bmp.data[i + 3] = if masked { 0 } else { 255 };
        }
    }
    Some(bmp)
}

fn decode_jpeg(
    data: &[u8],
    w: usize,
    h: usize,
    file: &PdfFile,
    dict: &Dict,
    resources: &Obj,
) -> Option<Bitmap> {
    let img = image::load_from_memory_with_format(data, image::ImageFormat::Jpeg).ok()?;
    let rgba = img.to_rgba8();
    let (iw, ih) = (rgba.width() as usize, rgba.height() as usize);
    if iw == 0 || ih == 0 || iw.saturating_mul(ih) > MAX_PIXELS {
        return None;
    }

    // Adobe CMYK JPEGs are stored inverted, which the PDF signals with
    // `/Decode [1 0 1 0 1 0 1 0]`.
    let cs = dict
        .get("ColorSpace")
        .or_else(|| dict.get("CS"))
        .map(|o| colorspace::parse(file, o, resources, 0));
    let inverted = matches!(cs, Some(ColorSpace::DeviceCMYK))
        && decode_array(file, dict)
            .map(|d| d.first().copied().unwrap_or(0.0) > 0.5)
            .unwrap_or(false);

    let mut bmp = Bitmap::new(iw, ih);
    for (i, px) in rgba.pixels().enumerate() {
        let o = i * 4;
        if inverted {
            bmp.data[o] = 255 - px[0];
            bmp.data[o + 1] = 255 - px[1];
            bmp.data[o + 2] = 255 - px[2];
        } else {
            bmp.data[o] = px[0];
            bmp.data[o + 1] = px[1];
            bmp.data[o + 2] = px[2];
        }
        bmp.data[o + 3] = 255;
    }
    // The dictionary's dimensions are authoritative, but a mismatch only
    // matters for masks, which are sampled by relative position anyway.
    let _ = (w, h);
    Some(bmp)
}

// ── masking ──────────────────────────────────────────────────────────────────

fn apply_masks(file: &PdfFile, dict: &Dict, resources: &Obj, bmp: &mut Bitmap) {
    // Soft mask: a grayscale image giving per-pixel alpha.
    if let Some(Obj::Stream(s)) = file.dget(dict, "SMask") {
        if let Some((mw, mh, alpha)) = decode_alpha(file, &s, resources, false) {
            blend_alpha(bmp, mw, mh, &alpha);
            return;
        }
    }
    match file.dget(dict, "Mask") {
        // Stencil mask: 1 means masked out, so the alpha is inverted relative
        // to a stencil that paints.
        Some(Obj::Stream(s)) => {
            if let Some((mw, mh, alpha)) = decode_alpha(file, &s, resources, false) {
                blend_alpha(bmp, mw, mh, &alpha);
            }
        }
        // Colour-key masking is handled during sample decoding, where the raw
        // component values are still available.
        _ => {}
    }
}

fn blend_alpha(bmp: &mut Bitmap, mw: usize, mh: usize, alpha: &[u8]) {
    if mw == 0 || mh == 0 {
        return;
    }
    for y in 0..bmp.h {
        let my = y * mh / bmp.h;
        for x in 0..bmp.w {
            let mx = x * mw / bmp.w;
            let a = alpha.get(my * mw + mx).copied().unwrap_or(255);
            let i = (y * bmp.w + x) * 4 + 3;
            bmp.data[i] = ((bmp.data[i] as u32 * a as u32) / 255) as u8;
        }
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> BitReader<'a> {
        BitReader { data, bit: 0 }
    }

    fn read(&mut self, bits: usize) -> u32 {
        let mut v: u32 = 0;
        for _ in 0..bits.min(32) {
            let byte = self.data.get(self.bit >> 3).copied().unwrap_or(0);
            let b = (byte >> (7 - (self.bit & 7))) & 1;
            v = (v << 1) | b as u32;
            self.bit += 1;
        }
        v
    }
}

fn int_of(file: &PdfFile, dict: &Dict, keys: &[&str]) -> Option<i64> {
    for k in keys {
        if let Some(v) = file.dget(dict, k).and_then(|o| o.as_i64()) {
            return Some(v);
        }
    }
    None
}

fn bool_of(file: &PdfFile, dict: &Dict, keys: &[&str]) -> Option<bool> {
    for k in keys {
        if let Some(v) = file.dget(dict, k).and_then(|o| o.as_bool()) {
            return Some(v);
        }
    }
    None
}

fn decode_array(file: &PdfFile, dict: &Dict) -> Option<Vec<f32>> {
    let o = dict.get("Decode").or_else(|| dict.get("D"))?;
    let a = file.resolve(o);
    let a = a.as_array()?;
    Some(a.iter().filter_map(|v| v.as_f32()).collect())
}

/// Parses an inline image: the dictionary between `BI` and `ID`, then the data
/// up to `EI`. Returns the synthesized stream and the position just past `EI`.
pub fn parse_inline(
    file: &PdfFile,
    data: &[u8],
    start: usize,
    resources: &Obj,
) -> Option<(Stream, usize)> {
    let mut lx = super::lexer::Lexer::at(data, start);
    let mut dict: Dict = Dict::new();
    loop {
        lx.skip_ws();
        if lx.eof() {
            return None;
        }
        if lx.peek() == Some(b'/') {
            let key = match lx.parse_obj() {
                Some(Obj::Name(n)) => n.as_ref().clone(),
                _ => return None,
            };
            let before = lx.pos;
            match lx.parse_obj() {
                Some(v) => {
                    dict.insert(expand_abbrev(&key), v);
                }
                None => {
                    if lx.pos == before {
                        lx.pos += 1;
                    }
                }
            }
            continue;
        }
        let kw = lx.read_keyword();
        if kw == b"ID" {
            break;
        }
        if kw.is_empty() {
            lx.pos += 1;
        }
    }
    // Exactly one whitespace byte separates `ID` from the samples.
    if lx.peek().map(super::lexer::is_ws).unwrap_or(false) {
        lx.pos += 1;
    }
    let data_start = lx.pos;

    // With no filter the length is computable, which avoids scanning for a
    // delimiter that could appear inside the samples.
    let filters = filter::filter_names(dict.get("Filter").map(|o| file.resolve(o)).as_ref());
    let end = if filters.is_empty() {
        let w = int_of(file, &dict, &["Width", "W"]).unwrap_or(0) as usize;
        let h = int_of(file, &dict, &["Height", "H"]).unwrap_or(0) as usize;
        let bpc = if bool_of(file, &dict, &["ImageMask", "IM"]).unwrap_or(false) {
            1
        } else {
            int_of(file, &dict, &["BitsPerComponent", "BPC"]).unwrap_or(8) as usize
        };
        let n = match dict.get("ColorSpace").or_else(|| dict.get("CS")) {
            Some(o) => colorspace::parse(file, o, resources, 0).n_comps(),
            None => 1,
        };
        let n = if bool_of(file, &dict, &["ImageMask", "IM"]).unwrap_or(false) { 1 } else { n };
        let len = ((w * n * bpc + 7) / 8) * h;
        (data_start + len).min(data.len())
    } else {
        find_ei(data, data_start).unwrap_or(data.len())
    };

    let raw = data.get(data_start..end)?.to_vec();
    let mut after = end;
    // Skip to just past `EI`.
    while after + 1 < data.len() {
        if data[after] == b'E' && data[after + 1] == b'I' {
            after += 2;
            break;
        }
        after += 1;
    }

    // Abbreviated filter names need expanding for the shared filter chain.
    if let Some(f) = dict.remove("Filter") {
        dict.insert("Filter".to_string(), expand_filter_names(&file.resolve(&f)));
    }

    Some((Stream { dict, raw, obj_num: 0, obj_gen: 0 }, after))
}

/// `EI` delimited by whitespace on both sides ends the samples.
fn find_ei(data: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < data.len() {
        if data[i] == b'E' && data[i + 1] == b'I' {
            let before_ws = i > from && super::lexer::is_ws(data[i - 1]);
            let after_ok = i + 2 >= data.len()
                || super::lexer::is_ws(data[i + 2])
                || super::lexer::is_delim(data[i + 2]);
            if before_ws && after_ok {
                // Trim the whitespace byte that precedes `EI`.
                return Some(i - 1);
            }
        }
        i += 1;
    }
    None
}

fn expand_abbrev(key: &str) -> String {
    match key {
        "BPC" => "BitsPerComponent",
        "CS" => "ColorSpace",
        "D" => "Decode",
        "DP" => "DecodeParms",
        "F" => "Filter",
        "H" => "Height",
        "IM" => "ImageMask",
        "I" => "Interpolate",
        "W" => "Width",
        other => other,
    }
    .to_string()
}

fn expand_filter_names(obj: &Obj) -> Obj {
    let expand = |n: &str| -> String {
        match n {
            "AHx" => "ASCIIHexDecode",
            "A85" => "ASCII85Decode",
            "LZW" => "LZWDecode",
            "Fl" => "FlateDecode",
            "RL" => "RunLengthDecode",
            "CCF" => "CCITTFaxDecode",
            "DCT" => "DCTDecode",
            other => other,
        }
        .to_string()
    };
    match obj {
        Obj::Name(n) => Obj::name(&expand(n)),
        Obj::Array(a) => Obj::Array(std::rc::Rc::new(
            a.iter()
                .map(|o| match o.as_name() {
                    Some(n) => Obj::name(&expand(n)),
                    None => o.clone(),
                })
                .collect(),
        )),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_reader_unpacks_sub_byte_samples() {
        // 0b1011_0010 as four 2-bit samples.
        let mut r = BitReader::new(&[0b1011_0010]);
        assert_eq!(r.read(2), 0b10);
        assert_eq!(r.read(2), 0b11);
        assert_eq!(r.read(2), 0b00);
        assert_eq!(r.read(2), 0b10);
        // Reads past the end return zero rather than panicking.
        assert_eq!(r.read(8), 0);
    }

    #[test]
    fn bit_reader_16_bit() {
        let mut r = BitReader::new(&[0xAB, 0xCD]);
        assert_eq!(r.read(16), 0xABCD);
    }

    #[test]
    fn inline_abbreviations_expand() {
        assert_eq!(expand_abbrev("BPC"), "BitsPerComponent");
        assert_eq!(expand_abbrev("CS"), "ColorSpace");
        assert_eq!(expand_abbrev("Width"), "Width");
        let f = expand_filter_names(&Obj::name("AHx"));
        assert_eq!(f.as_name(), Some("ASCIIHexDecode"));
    }
}
