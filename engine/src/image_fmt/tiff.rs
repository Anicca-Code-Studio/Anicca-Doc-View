//! Baseline TIFF decoder, written from scratch.
//!
//! Covers strip and tile layouts, chunky planar config, compression none /
//! PackBits / LZW / Deflate with horizontal predictor, and photometric
//! WhiteIsZero / BlackIsZero / RGB / Palette / CMYK at 1/4/8/16 bits.

use crate::pdf::filter::{apply_predictor, flate_decode, lzw_decode};
use crate::raster::Bitmap;

pub fn is_tiff(b: &[u8]) -> bool {
    b.len() >= 8 && (&b[0..4] == b"II\x2A\x00" || &b[0..4] == b"MM\x00\x2A")
}

struct Reader<'a> {
    d: &'a [u8],
    le: bool,
}
impl<'a> Reader<'a> {
    fn u16(&self, o: usize) -> usize {
        if o + 2 > self.d.len() {
            return 0;
        }
        if self.le {
            self.d[o] as usize | ((self.d[o + 1] as usize) << 8)
        } else {
            ((self.d[o] as usize) << 8) | self.d[o + 1] as usize
        }
    }
    fn u32(&self, o: usize) -> usize {
        if o + 4 > self.d.len() {
            return 0;
        }
        if self.le {
            self.d[o] as usize | ((self.d[o + 1] as usize) << 8) | ((self.d[o + 2] as usize) << 16) | ((self.d[o + 3] as usize) << 24)
        } else {
            ((self.d[o] as usize) << 24) | ((self.d[o + 1] as usize) << 16) | ((self.d[o + 2] as usize) << 8) | self.d[o + 3] as usize
        }
    }
}

#[derive(Default)]
struct Tags {
    width: usize,
    height: usize,
    bits: Vec<usize>,
    compression: usize,
    photometric: usize,
    spp: usize,
    rows_per_strip: usize,
    strip_offsets: Vec<usize>,
    strip_counts: Vec<usize>,
    predictor: usize,
    colormap: Vec<usize>,
    planar: usize,
    extra_samples: usize,
    tile_w: usize,
    tile_h: usize,
    tile_offsets: Vec<usize>,
    tile_counts: Vec<usize>,
}

fn type_size(t: usize) -> usize {
    match t {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 => 4,
        5 | 10 | 12 => 8,
        _ => 1,
    }
}

pub fn decode(b: &[u8]) -> Option<Bitmap> {
    if !is_tiff(b) {
        return None;
    }
    let le = &b[0..2] == b"II";
    let r = Reader { d: b, le };
    let ifd = r.u32(4);
    let n = r.u16(ifd);
    if ifd == 0 || ifd + 2 + n * 12 > b.len() {
        return None;
    }

    // Reads a tag's values as usize (SHORT/LONG/BYTE).
    let read_vals = |entry: usize| -> Vec<usize> {
        let typ = r.u16(entry + 2);
        let count = r.u32(entry + 4);
        let ts = type_size(typ);
        let total = ts * count;
        let base = if total <= 4 { entry + 8 } else { r.u32(entry + 8) };
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let o = base + i * ts;
            out.push(match ts {
                1 => *b.get(o).unwrap_or(&0) as usize,
                2 => r.u16(o),
                _ => r.u32(o),
            });
        }
        out
    };

    let mut t = Tags {
        compression: 1,
        photometric: 1,
        spp: 1,
        rows_per_strip: usize::MAX,
        predictor: 1,
        planar: 1,
        ..Default::default()
    };

    for i in 0..n {
        let e = ifd + 2 + i * 12;
        let tag = r.u16(e);
        match tag {
            256 => t.width = read_vals(e).first().copied().unwrap_or(0),
            257 => t.height = read_vals(e).first().copied().unwrap_or(0),
            258 => t.bits = read_vals(e),
            259 => t.compression = read_vals(e).first().copied().unwrap_or(1),
            262 => t.photometric = read_vals(e).first().copied().unwrap_or(1),
            273 => t.strip_offsets = read_vals(e),
            277 => t.spp = read_vals(e).first().copied().unwrap_or(1),
            278 => t.rows_per_strip = read_vals(e).first().copied().unwrap_or(usize::MAX),
            279 => t.strip_counts = read_vals(e),
            284 => t.planar = read_vals(e).first().copied().unwrap_or(1),
            317 => t.predictor = read_vals(e).first().copied().unwrap_or(1),
            320 => t.colormap = read_vals(e),
            322 => t.tile_w = read_vals(e).first().copied().unwrap_or(0),
            323 => t.tile_h = read_vals(e).first().copied().unwrap_or(0),
            324 => t.tile_offsets = read_vals(e),
            325 => t.tile_counts = read_vals(e),
            338 => t.extra_samples = read_vals(e).first().copied().unwrap_or(0),
            _ => {}
        }
    }

    if t.width == 0 || t.height == 0 || t.width * t.height > 64_000_000 {
        return None;
    }
    if t.bits.is_empty() {
        t.bits = vec![1; t.spp];
    }
    let bits = t.bits[0];
    if t.bits.iter().any(|&x| x != bits) {
        return None; // mixed bit depths unsupported
    }
    // Only chunky planar config.
    if t.planar != 1 {
        return None;
    }

    let mut bmp = Bitmap::new(t.width, t.height);

    if !t.tile_offsets.is_empty() && t.tile_w > 0 && t.tile_h > 0 {
        // Tiled.
        let tiles_across = (t.width + t.tile_w - 1) / t.tile_w;
        let row_bytes = (t.tile_w * t.spp * bits + 7) / 8;
        for (i, (&off, &cnt)) in t.tile_offsets.iter().zip(t.tile_counts.iter()).enumerate() {
            if off + cnt > b.len() {
                continue;
            }
            let tx = (i % tiles_across) * t.tile_w;
            let ty = (i / tiles_across) * t.tile_h;
            let data = decompress_predict(&b[off..off + cnt], &t, bits, t.tile_w);
            place(&mut bmp, &t, bits, &data, tx, ty, t.tile_w, t.tile_h, row_bytes);
        }
    } else {
        // Stripped.
        let rps = t.rows_per_strip.min(t.height).max(1);
        let row_bytes = (t.width * t.spp * bits + 7) / 8;
        for (s, (&off, &cnt)) in t.strip_offsets.iter().zip(t.strip_counts.iter()).enumerate() {
            if off + cnt > b.len() {
                continue;
            }
            let y0 = s * rps;
            let rows = rps.min(t.height.saturating_sub(y0));
            let data = decompress_predict(&b[off..off + cnt], &t, bits, t.width);
            place(&mut bmp, &t, bits, &data, 0, y0, t.width, rows, row_bytes);
        }
    }

    Some(bmp)
}

fn decompress_predict(raw: &[u8], t: &Tags, bits: usize, columns: usize) -> Vec<u8> {
    let out = match t.compression {
        1 => raw.to_vec(),
        5 => lzw_decode(raw, true),
        8 | 32946 => flate_decode(raw).unwrap_or_default(),
        32773 => packbits(raw, raw.len() * 4),
        _ => raw.to_vec(),
    };
    if t.predictor == 2 {
        apply_predictor(out, 2, t.spp, bits, columns)
    } else {
        out
    }
}

#[allow(clippy::too_many_arguments)]
fn place(bmp: &mut Bitmap, t: &Tags, bits: usize, data: &[u8], x0: usize, y0: usize, bw: usize, bh: usize, row_bytes: usize) {
    let maxv = ((1u32 << bits.min(16)) - 1).max(1);
    for ry in 0..bh {
        let y = y0 + ry;
        if y >= t.height {
            break;
        }
        let row = &data[(ry * row_bytes).min(data.len())..];
        let mut br = Bits::new(row);
        for rx in 0..bw {
            let x = x0 + rx;
            let mut samp = [0u32; 4];
            for c in 0..t.spp.min(4) {
                let v = br.read(bits);
                samp[c] = v;
            }
            if x >= t.width {
                continue;
            }
            let px = to_rgba(t, bits, maxv, &samp);
            let o = (y * t.width + x) * 4;
            bmp.data[o..o + 4].copy_from_slice(&px);
        }
    }
}

fn scale8(v: u32, maxv: u32) -> u8 {
    ((v * 255 + maxv / 2) / maxv) as u8
}

fn to_rgba(t: &Tags, bits: usize, maxv: u32, s: &[u32; 4]) -> [u8; 4] {
    match t.photometric {
        0 => {
            // WhiteIsZero grayscale.
            let g = 255 - scale8(s[0], maxv);
            let a = if t.spp >= 2 { scale8(s[1], maxv) } else { 255 };
            [g, g, g, a]
        }
        1 => {
            let g = scale8(s[0], maxv);
            let a = if t.spp >= 2 { scale8(s[1], maxv) } else { 255 };
            [g, g, g, a]
        }
        2 => {
            let a = if t.spp >= 4 { scale8(s[3], maxv) } else { 255 };
            [scale8(s[0], maxv), scale8(s[1], maxv), scale8(s[2], maxv), a]
        }
        3 => {
            // Palette: colormap holds 3 * 2^bits 16-bit entries (all R, then G, B).
            let n = 1usize << bits.min(16);
            let idx = s[0] as usize;
            let r = t.colormap.get(idx).copied().unwrap_or(0);
            let g = t.colormap.get(n + idx).copied().unwrap_or(0);
            let b = t.colormap.get(2 * n + idx).copied().unwrap_or(0);
            [(r >> 8) as u8, (g >> 8) as u8, (b >> 8) as u8, 255]
        }
        5 => {
            // CMYK (not inverted).
            let c = s[0] as f32 / maxv as f32;
            let m = s[1] as f32 / maxv as f32;
            let y = s[2] as f32 / maxv as f32;
            let k = if t.spp >= 4 { s[3] as f32 / maxv as f32 } else { 0.0 };
            let rgb = crate::pdf::colorspace::cmyk_to_rgb(c, m, y, k);
            [rgb[0], rgb[1], rgb[2], 255]
        }
        _ => {
            let g = scale8(s[0], maxv);
            [g, g, g, 255]
        }
    }
}

/// TIFF PackBits (same scheme as Macintosh); 128 is a no-op (not EOD).
fn packbits(d: &[u8], hint: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(hint.min(1 << 24));
    let mut i = 0usize;
    while i < d.len() {
        let n = d[i] as i8;
        i += 1;
        if n >= 0 {
            let cnt = n as usize + 1;
            let end = (i + cnt).min(d.len());
            out.extend_from_slice(&d[i..end]);
            i = end;
        } else if n != -128 {
            let cnt = (1 - n as i32) as usize;
            if let Some(&b) = d.get(i) {
                out.extend(std::iter::repeat(b).take(cnt));
            }
            i += 1;
        }
    }
    out
}

/// MSB-first bit reader for sub-byte and multi-byte TIFF samples (big-endian
/// within a sample, matching TIFF's fill order 1).
struct Bits<'a> {
    d: &'a [u8],
    bit: usize,
}
impl<'a> Bits<'a> {
    fn new(d: &'a [u8]) -> Bits<'a> {
        Bits { d, bit: 0 }
    }
    fn read(&mut self, n: usize) -> u32 {
        let mut v = 0u32;
        for _ in 0..n.min(32) {
            let byte = self.d.get(self.bit >> 3).copied().unwrap_or(0);
            let b = (byte >> (7 - (self.bit & 7))) & 1;
            v = (v << 1) | b as u32;
            self.bit += 1;
        }
        v
    }
}
