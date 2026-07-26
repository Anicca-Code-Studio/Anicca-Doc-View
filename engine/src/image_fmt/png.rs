//! PNG decoder (ISO/IEC 15948), written from scratch.
//!
//! Reuses the engine's own zlib inflate and PNG scanline predictor from
//! `crate::pdf::filter`, so no third-party image codec is involved. Covers all
//! five colour types, bit depths 1/2/4/8/16, palette + `tRNS` transparency, and
//! Adam7 interlacing.

use crate::pdf::filter;
use crate::raster::Bitmap;

/// Decoded PNG plus its physical resolution, if the file declares one.
pub struct Decoded {
    pub bitmap: Bitmap,
    /// Pixels per metre on the x axis (from `pHYs`), or `None`.
    pub dpi: Option<f32>,
}

const SIG: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

pub fn is_png(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && bytes[..8] == SIG
}

struct Header {
    width: usize,
    height: usize,
    bit_depth: usize,
    color_type: u8,
    interlace: u8,
}

impl Header {
    /// Samples per pixel for this colour type.
    fn channels(&self) -> usize {
        match self.color_type {
            0 => 1, // grayscale
            2 => 3, // truecolour
            3 => 1, // indexed
            4 => 2, // grayscale + alpha
            6 => 4, // truecolour + alpha
            _ => 0,
        }
    }
}

fn be32(b: &[u8]) -> usize {
    ((b[0] as usize) << 24) | ((b[1] as usize) << 16) | ((b[2] as usize) << 8) | b[3] as usize
}

pub fn decode(bytes: &[u8]) -> Option<Decoded> {
    if !is_png(bytes) {
        return None;
    }
    let mut pos = 8usize;

    let mut hdr: Option<Header> = None;
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut trns: Vec<u8> = Vec::new(); // per-index alpha (indexed), or key bytes
    let mut idat: Vec<u8> = Vec::new();
    let mut dpi: Option<f32> = None;

    while pos + 8 <= bytes.len() {
        let len = be32(&bytes[pos..pos + 4]);
        let ctype = &bytes[pos + 4..pos + 8];
        let dstart = pos + 8;
        let dend = dstart.checked_add(len)?;
        if dend + 4 > bytes.len() {
            break; // truncated; use what we have
        }
        let data = &bytes[dstart..dend];
        match ctype {
            b"IHDR" => {
                if data.len() < 13 {
                    return None;
                }
                hdr = Some(Header {
                    width: be32(&data[0..4]),
                    height: be32(&data[4..8]),
                    bit_depth: data[8] as usize,
                    color_type: data[9],
                    interlace: data[12],
                });
            }
            b"PLTE" => {
                palette = data.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
            }
            b"tRNS" => {
                trns = data.to_vec();
            }
            b"pHYs" => {
                if data.len() >= 9 && data[8] == 1 {
                    // unit = metre; ppux stored, convert to DPI (dots per inch).
                    let ppux = be32(&data[0..4]) as f32;
                    if ppux > 0.0 {
                        dpi = Some(ppux * 0.0254);
                    }
                }
            }
            b"IDAT" => idat.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
        pos = dend + 4; // skip CRC
    }

    let hdr = hdr?;
    let channels = hdr.channels();
    if channels == 0 || hdr.width == 0 || hdr.height == 0 {
        return None;
    }
    if hdr.width.saturating_mul(hdr.height) > 64_000_000 {
        return None;
    }

    let raw = filter::flate_decode(&idat)?;

    let mut bmp = Bitmap::new(hdr.width, hdr.height);
    if hdr.interlace == 0 {
        let row_len = (hdr.width * channels * hdr.bit_depth + 7) / 8;
        let unfiltered = filter::apply_predictor(raw, 12, channels, hdr.bit_depth, hdr.width);
        emit_rect(&mut bmp, &unfiltered, row_len, 0, 0, hdr.width, hdr.height, 1, 1, &hdr, &palette, &trns);
    } else {
        decode_adam7(&mut bmp, &raw, &hdr, channels, &palette, &trns);
    }

    Some(Decoded { bitmap: bmp, dpi })
}

/// Adam7 pass parameters: (x_start, y_start, x_step, y_step).
const ADAM7: [(usize, usize, usize, usize); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

fn decode_adam7(
    bmp: &mut Bitmap,
    raw: &[u8],
    hdr: &Header,
    channels: usize,
    palette: &[[u8; 3]],
    trns: &[u8],
) {
    let mut offset = 0usize;
    for &(x0, y0, xs, ys) in ADAM7.iter() {
        if x0 >= hdr.width || y0 >= hdr.height {
            continue;
        }
        let pw = (hdr.width - x0 + xs - 1) / xs;
        let ph = (hdr.height - y0 + ys - 1) / ys;
        if pw == 0 || ph == 0 {
            continue;
        }
        let row_len = (pw * channels * hdr.bit_depth + 7) / 8;
        let stride = row_len + 1;
        let pass_bytes = stride * ph;
        if offset + pass_bytes > raw.len() {
            break;
        }
        let pass = filter::apply_predictor(raw[offset..offset + pass_bytes].to_vec(), 12, channels, hdr.bit_depth, pw);
        emit_rect(bmp, &pass, row_len, x0, y0, pw, ph, xs, ys, hdr, palette, trns);
        offset += pass_bytes;
    }
}

/// Unpacks `data` (unfiltered scanlines, `row_len` bytes each) into `bmp`,
/// placing pixel (px,py) of the pass at bmp(x0+px*xs, y0+py*ys).
#[allow(clippy::too_many_arguments)]
fn emit_rect(
    bmp: &mut Bitmap,
    data: &[u8],
    row_len: usize,
    x0: usize,
    y0: usize,
    pw: usize,
    ph: usize,
    xs: usize,
    ys: usize,
    hdr: &Header,
    palette: &[[u8; 3]],
    trns: &[u8],
) {
    let channels = hdr.channels();
    let bpc = hdr.bit_depth;
    let maxv = ((1u32 << bpc) - 1) as u32;

    // Colour-key transparency for gray/truecolour lives in tRNS as big-endian
    // sample values.
    let gray_key: Option<u32> = if hdr.color_type == 0 && trns.len() >= 2 {
        Some(((trns[0] as u32) << 8 | trns[1] as u32) & maxv)
    } else {
        None
    };
    let rgb_key: Option<(u32, u32, u32)> = if hdr.color_type == 2 && trns.len() >= 6 {
        Some((
            (trns[0] as u32) << 8 | trns[1] as u32,
            (trns[2] as u32) << 8 | trns[3] as u32,
            (trns[4] as u32) << 8 | trns[5] as u32,
        ))
    } else {
        None
    };

    for py in 0..ph {
        let row = match data.get(py * row_len..py * row_len + row_len) {
            Some(r) => r,
            None => break,
        };
        let mut br = BitReader::new(row);
        for px in 0..pw {
            let mut s = [0u32; 4];
            for c in 0..channels {
                s[c] = br.read(bpc);
            }
            let (r, g, b, a) = resolve(hdr, &s, maxv, palette, trns, gray_key, rgb_key);
            let dx = x0 + px * xs;
            let dy = y0 + py * ys;
            if dx < bmp.w && dy < bmp.h {
                let i = (dy * bmp.w + dx) * 4;
                bmp.data[i] = r;
                bmp.data[i + 1] = g;
                bmp.data[i + 2] = b;
                bmp.data[i + 3] = a;
            }
        }
    }
}

/// Scales a raw sample of `bpc` bits to 8-bit.
#[inline]
fn scale8(v: u32, maxv: u32) -> u8 {
    if maxv == 0 {
        return 0;
    }
    ((v * 255 + maxv / 2) / maxv) as u8
}

#[allow(clippy::too_many_arguments)]
fn resolve(
    hdr: &Header,
    s: &[u32; 4],
    maxv: u32,
    palette: &[[u8; 3]],
    trns: &[u8],
    gray_key: Option<u32>,
    rgb_key: Option<(u32, u32, u32)>,
) -> (u8, u8, u8, u8) {
    match hdr.color_type {
        0 => {
            let g = scale8(s[0], maxv);
            let a = if gray_key == Some(s[0]) { 0 } else { 255 };
            (g, g, g, a)
        }
        2 => {
            let a = if rgb_key == Some((s[0], s[1], s[2])) { 0 } else { 255 };
            (scale8(s[0], maxv), scale8(s[1], maxv), scale8(s[2], maxv), a)
        }
        3 => {
            let idx = s[0] as usize;
            let c = palette.get(idx).copied().unwrap_or([0, 0, 0]);
            let a = trns.get(idx).copied().unwrap_or(255);
            (c[0], c[1], c[2], a)
        }
        4 => {
            let g = scale8(s[0], maxv);
            (g, g, g, scale8(s[1], maxv))
        }
        6 => (
            scale8(s[0], maxv),
            scale8(s[1], maxv),
            scale8(s[2], maxv),
            scale8(s[3], maxv),
        ),
        _ => (0, 0, 0, 0),
    }
}

/// MSB-first bit reader for sub-byte and multi-byte samples.
struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> BitReader<'a> {
        BitReader { data, bit: 0 }
    }

    fn read(&mut self, bits: usize) -> u32 {
        let mut v = 0u32;
        for _ in 0..bits.min(32) {
            let byte = self.data.get(self.bit >> 3).copied().unwrap_or(0);
            let b = (byte >> (7 - (self.bit & 7))) & 1;
            v = (v << 1) | b as u32;
            self.bit += 1;
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal non-interlaced 8-bit RGBA PNG by hand and round-trips it.
    fn make_png(w: u32, h: u32, color_type: u8, bit_depth: u8, pixels: &[u8]) -> Vec<u8> {
        use std::io::Write;
        fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            // CRC over type+data.
            let mut crc = 0xFFFF_FFFFu32;
            for &b in kind.iter().chain(data.iter()) {
                crc ^= b as u32;
                for _ in 0..8 {
                    crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
                }
            }
            out.extend_from_slice(&(!crc).to_be_bytes());
        }
        let channels = match color_type {
            0 => 1,
            2 => 3,
            6 => 4,
            _ => 1,
        };
        let mut raw = Vec::new();
        let row_len = w as usize * channels * bit_depth as usize / 8;
        for y in 0..h as usize {
            raw.push(0u8); // filter type None
            raw.extend_from_slice(&pixels[y * row_len..(y + 1) * row_len]);
        }
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&raw).unwrap();
        let idat = e.finish().unwrap();

        let mut out = Vec::new();
        out.extend_from_slice(&SIG);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.push(bit_depth);
        ihdr.push(color_type);
        ihdr.extend_from_slice(&[0, 0, 0]); // compression, filter, interlace
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(&mut out, b"IDAT", &idat);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    #[test]
    fn rgba_roundtrip() {
        // 2x1: red opaque, green half-alpha.
        let px = [255u8, 0, 0, 255, 0, 255, 0, 128];
        let png = make_png(2, 1, 6, 8, &px);
        let d = decode(&png).unwrap();
        assert_eq!((d.bitmap.w, d.bitmap.h), (2, 1));
        assert_eq!(&d.bitmap.data[0..4], &[255, 0, 0, 255]);
        assert_eq!(&d.bitmap.data[4..8], &[0, 255, 0, 128]);
    }

    #[test]
    fn gray_roundtrip() {
        let px = [0u8, 128, 255];
        let png = make_png(3, 1, 0, 8, &px);
        let d = decode(&png).unwrap();
        assert_eq!(&d.bitmap.data[0..4], &[0, 0, 0, 255]);
        assert_eq!(&d.bitmap.data[8..12], &[255, 255, 255, 255]);
    }
}
