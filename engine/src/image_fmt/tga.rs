//! Truevision TGA decoder, written from scratch.
//!
//! Covers colour-mapped, truecolour, and grayscale images, both raw and
//! run-length encoded, at 8/15/16/24/32 bits per pixel.

use crate::raster::Bitmap;

/// TGA has no leading magic. Prefer the v2 footer; otherwise validate the
/// 18-byte header conservatively (this runs only after every magic-bearing
/// format has been ruled out).
pub fn is_tga(b: &[u8]) -> bool {
    if b.len() >= 44 && &b[b.len() - 18..b.len() - 2] == b"TRUEVISION-XFILE" {
        return true;
    }
    if b.len() < 18 {
        return false;
    }
    let cmap_type = b[1];
    let img_type = b[2];
    let depth = b[16];
    let w = le16(b, 12);
    let h = le16(b, 14);
    cmap_type <= 1
        && matches!(img_type, 1 | 2 | 3 | 9 | 10 | 11)
        && matches!(depth, 8 | 15 | 16 | 24 | 32)
        && w > 0
        && h > 0
        && w <= 30000
        && h <= 30000
        // A colour-mapped image needs a map; a non-mapped one must not declare one.
        && ((img_type % 8 == 1) == (cmap_type == 1))
}

fn le16(b: &[u8], i: usize) -> usize {
    b[i] as usize | ((b[i + 1] as usize) << 8)
}

pub fn decode(b: &[u8]) -> Option<Bitmap> {
    if b.len() < 18 {
        return None;
    }
    let id_len = b[0] as usize;
    let cmap_type = b[1];
    let img_type = b[2];
    let cmap_len = le16(b, 5);
    let cmap_bits = b[7] as usize;
    let w = le16(b, 12);
    let h = le16(b, 14);
    let depth = b[16] as usize;
    let descriptor = b[17];
    if w == 0 || h == 0 || w * h > 64_000_000 {
        return None;
    }
    let top_down = descriptor & 0x20 != 0;
    let right_left = descriptor & 0x10 != 0;

    let mut p = 18 + id_len;

    // Colour map.
    let cmap_entry_bytes = (cmap_bits + 7) / 8;
    let mut cmap: Vec<[u8; 4]> = Vec::new();
    if cmap_type == 1 {
        for _ in 0..cmap_len {
            if p + cmap_entry_bytes > b.len() {
                return None;
            }
            cmap.push(read_pixel(&b[p..], cmap_bits));
            p += cmap_entry_bytes;
        }
    }

    let rle = img_type >= 9;
    let base_type = img_type & 0x07; // 1 colormapped, 2 truecolor, 3 gray
    let px_bytes = (depth + 7) / 8;

    let npix = w * h;
    let mut pixels: Vec<[u8; 4]> = Vec::with_capacity(npix);

    if rle {
        while pixels.len() < npix && p < b.len() {
            let hdr = b[p];
            p += 1;
            let count = (hdr & 0x7F) as usize + 1;
            if hdr & 0x80 != 0 {
                // Run packet: one pixel repeated.
                if p + px_bytes > b.len() {
                    break;
                }
                let px = resolve(&b[p..], depth, base_type, &cmap);
                p += px_bytes;
                for _ in 0..count {
                    pixels.push(px);
                }
            } else {
                for _ in 0..count {
                    if p + px_bytes > b.len() {
                        break;
                    }
                    pixels.push(resolve(&b[p..], depth, base_type, &cmap));
                    p += px_bytes;
                }
            }
        }
    } else {
        for _ in 0..npix {
            if p + px_bytes > b.len() {
                break;
            }
            pixels.push(resolve(&b[p..], depth, base_type, &cmap));
            p += px_bytes;
        }
    }
    pixels.resize(npix, [0, 0, 0, 255]);

    let mut bmp = Bitmap::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let sx = if right_left { w - 1 - x } else { x };
            let sy = if top_down { y } else { h - 1 - y };
            let px = pixels[sy * w + sx];
            let o = (y * w + x) * 4;
            bmp.data[o..o + 4].copy_from_slice(&px);
        }
    }
    Some(bmp)
}

fn resolve(d: &[u8], depth: usize, base_type: u8, cmap: &[[u8; 4]]) -> [u8; 4] {
    if base_type == 1 {
        // Colour-mapped: index of `depth` bits.
        let idx = if depth <= 8 {
            d[0] as usize
        } else {
            d[0] as usize | ((d[1] as usize) << 8)
        };
        cmap.get(idx).copied().unwrap_or([0, 0, 0, 255])
    } else if base_type == 3 {
        let g = d[0];
        [g, g, g, 255]
    } else {
        read_pixel(d, depth)
    }
}

/// Reads a raw truecolour pixel (BGR/BGRA or 15/16-bit 5-5-5).
fn read_pixel(d: &[u8], bits: usize) -> [u8; 4] {
    match bits {
        32 => [d[2], d[1], d[0], d[3]],
        24 => [d[2], d[1], d[0], 255],
        15 | 16 => {
            let v = d[0] as usize | ((d[1] as usize) << 8);
            let r = ((v >> 10) & 0x1F) as u8;
            let g = ((v >> 5) & 0x1F) as u8;
            let b = (v & 0x1F) as u8;
            let ex = |c: u8| (c << 3) | (c >> 2);
            let a = if bits == 16 && (v & 0x8000) == 0 { 255 } else { 255 };
            [ex(r), ex(g), ex(b), a]
        }
        8 => {
            let g = d[0];
            [g, g, g, 255]
        }
        _ => [0, 0, 0, 255],
    }
}
