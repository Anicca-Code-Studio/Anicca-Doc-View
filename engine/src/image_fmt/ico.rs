//! ICO / CUR decoder. Each directory entry is either an embedded PNG (decoded
//! by `png.rs`) or a BMP DIB with a doubled height (XOR colour bitmap + 1-bpp
//! AND transparency mask). The largest entry is chosen.

use super::png;
use crate::raster::Bitmap;

pub fn is_ico(b: &[u8]) -> bool {
    b.len() >= 6 && b[0] == 0 && b[1] == 0 && (b[2] == 1 || b[2] == 2) && b[3] == 0
}

fn le16(b: &[u8], i: usize) -> usize {
    b[i] as usize | ((b[i + 1] as usize) << 8)
}
fn le32(b: &[u8], i: usize) -> usize {
    b[i] as usize | ((b[i + 1] as usize) << 8) | ((b[i + 2] as usize) << 16) | ((b[i + 3] as usize) << 24)
}

pub fn decode(b: &[u8]) -> Option<Bitmap> {
    if !is_ico(b) {
        return None;
    }
    let count = le16(b, 4);
    let mut best: Option<Bitmap> = None;
    let mut best_area = 0usize;
    for i in 0..count {
        let e = 6 + i * 16;
        if e + 16 > b.len() {
            break;
        }
        let size = le32(b, e + 8);
        let off = le32(b, e + 12);
        if off + size > b.len() || size == 0 {
            continue;
        }
        let data = &b[off..off + size];
        let bmp = if png::is_png(data) {
            png::decode(data).map(|d| d.bitmap)
        } else {
            decode_dib(data)
        };
        if let Some(bmp) = bmp {
            let area = bmp.w * bmp.h;
            if area > best_area {
                best_area = area;
                best = Some(bmp);
            }
        }
    }
    best
}

/// Decodes a BMP DIB embedded in an ICO entry (no file header; height doubled).
fn decode_dib(d: &[u8]) -> Option<Bitmap> {
    if d.len() < 40 {
        return None;
    }
    let header = le32(d, 0);
    let w = le32(d, 4);
    let h2 = le32(d, 8);
    let bpp = le16(d, 14);
    if w == 0 || h2 == 0 {
        return None;
    }
    let h = h2 / 2; // XOR + AND
    if w * h > 64_000_000 {
        return None;
    }

    // Palette for <= 8 bpp.
    let mut colors = le32(d, 32);
    if colors == 0 && bpp <= 8 {
        colors = 1 << bpp;
    }
    let pal_start = header;
    let mut palette: Vec<[u8; 3]> = Vec::new();
    if bpp <= 8 {
        for i in 0..colors {
            let o = pal_start + i * 4;
            if o + 3 >= d.len() {
                break;
            }
            palette.push([d[o + 2], d[o + 1], d[o]]);
        }
    }
    let xor_start = pal_start + if bpp <= 8 { colors * 4 } else { 0 };

    let xor_stride = ((w * bpp + 31) / 32) * 4;
    let and_stride = ((w + 31) / 32) * 4;
    let and_start = xor_start + xor_stride * h;

    let mut bmp = Bitmap::new(w, h);
    for y in 0..h {
        let sy = h - 1 - y; // bottom-up
        let xrow = xor_start + sy * xor_stride;
        let arow = and_start + sy * and_stride;
        for x in 0..w {
            let (mut rgb, mut a);
            match bpp {
                32 => {
                    let o = xrow + x * 4;
                    if o + 3 >= d.len() {
                        continue;
                    }
                    rgb = [d[o + 2], d[o + 1], d[o]];
                    a = d[o + 3];
                    // Some icons leave alpha zero; fall back to the AND mask.
                    if a == 0 && !and_bit(d, arow, x) {
                        a = 255;
                    }
                }
                24 => {
                    let o = xrow + x * 3;
                    if o + 2 >= d.len() {
                        continue;
                    }
                    rgb = [d[o + 2], d[o + 1], d[o]];
                    a = if and_bit(d, arow, x) { 0 } else { 255 };
                }
                8 | 4 | 1 => {
                    let bit = x * bpp;
                    let byte = xrow + (bit >> 3);
                    let idx = match bpp {
                        8 => *d.get(byte)? as usize,
                        4 => {
                            let v = *d.get(byte)?;
                            if bit & 7 == 0 { (v >> 4) as usize } else { (v & 0x0F) as usize }
                        }
                        _ => ((*d.get(byte)? >> (7 - (bit & 7))) & 1) as usize,
                    };
                    let c = palette.get(idx).copied().unwrap_or([0, 0, 0]);
                    rgb = c;
                    a = if and_bit(d, arow, x) { 0 } else { 255 };
                }
                _ => return None,
            }
            let _ = &mut rgb;
            let _ = &mut a;
            let o = (y * w + x) * 4;
            bmp.data[o] = rgb[0];
            bmp.data[o + 1] = rgb[1];
            bmp.data[o + 2] = rgb[2];
            bmp.data[o + 3] = a;
        }
    }
    Some(bmp)
}

fn and_bit(d: &[u8], row: usize, x: usize) -> bool {
    let byte = d.get(row + (x >> 3)).copied().unwrap_or(0);
    (byte >> (7 - (x & 7))) & 1 == 1
}
