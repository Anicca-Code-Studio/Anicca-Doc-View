//! BMP / DIB decoder (Windows bitmap), written from scratch.
//!
//! Covers the common BITMAPINFOHEADER cases: 1/4/8-bit palette (incl. RLE4 /
//! RLE8), 24-bit BGR, and 32-bit BGRA, both bottom-up and top-down.

use crate::raster::Bitmap;

pub fn is_bmp(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && &bytes[0..2] == b"BM"
}

fn le16(b: &[u8]) -> usize {
    (b[0] as usize) | ((b[1] as usize) << 8)
}
fn le32(b: &[u8]) -> usize {
    (b[0] as usize) | ((b[1] as usize) << 8) | ((b[2] as usize) << 16) | ((b[3] as usize) << 24)
}
fn le32i(b: &[u8]) -> i32 {
    le32(b) as u32 as i32
}

pub fn decode(bytes: &[u8]) -> Option<Bitmap> {
    if !is_bmp(bytes) || bytes.len() < 54 {
        return None;
    }
    let pixel_offset = le32(&bytes[10..14]);
    let dib_size = le32(&bytes[14..18]);
    if dib_size < 40 {
        return None; // BITMAPCOREHEADER and friends are rare; skip
    }
    let width = le32i(&bytes[18..22]);
    let height_raw = le32i(&bytes[22..26]);
    let bpp = le16(&bytes[28..30]);
    let compression = le32(&bytes[30..34]);
    let mut colors_used = le32(&bytes[46..50]);

    if width <= 0 || height_raw == 0 {
        return None;
    }
    let w = width as usize;
    let top_down = height_raw < 0;
    let h = height_raw.unsigned_abs() as usize;
    if w.saturating_mul(h) > 64_000_000 {
        return None;
    }

    // Palette (present for <=8 bpp), sits right after the DIB header.
    let palette_start = 14 + dib_size;
    if colors_used == 0 && bpp <= 8 {
        colors_used = 1usize << bpp;
    }
    let mut palette: Vec<[u8; 3]> = Vec::new();
    if bpp <= 8 {
        for i in 0..colors_used {
            let o = palette_start + i * 4;
            if o + 3 >= bytes.len() {
                break;
            }
            // Stored BGRA (the 4th byte is reserved).
            palette.push([bytes[o + 2], bytes[o + 1], bytes[o]]);
        }
    }

    let pixels = bytes.get(pixel_offset..)?;
    let mut bmp = Bitmap::new(w, h);

    // Row of decoded RGBA gets written top-left; BMP rows go bottom-up unless
    // the height was negative.
    let put = |bmp: &mut Bitmap, x: usize, y: usize, rgba: [u8; 4]| {
        let dy = if top_down { y } else { h - 1 - y };
        if x < w && dy < h {
            let i = (dy * w + x) * 4;
            bmp.data[i..i + 4].copy_from_slice(&rgba);
        }
    };

    match (bpp, compression) {
        (24, 0) => {
            let row_stride = ((w * 3 + 3) / 4) * 4;
            for y in 0..h {
                let base = y * row_stride;
                for x in 0..w {
                    let o = base + x * 3;
                    if o + 2 >= pixels.len() {
                        break;
                    }
                    put(&mut bmp, x, y, [pixels[o + 2], pixels[o + 1], pixels[o], 255]);
                }
            }
        }
        (32, 0) | (32, 3) => {
            let row_stride = w * 4;
            // BI_RGB 32-bit has an unused 4th byte; treat as opaque. BI_BITFIELDS
            // (3) commonly still lays out BGRA, so use the byte as alpha.
            let use_alpha = compression == 3;
            for y in 0..h {
                let base = y * row_stride;
                for x in 0..w {
                    let o = base + x * 4;
                    if o + 3 >= pixels.len() {
                        break;
                    }
                    let a = if use_alpha { pixels[o + 3] } else { 255 };
                    put(&mut bmp, x, y, [pixels[o + 2], pixels[o + 1], pixels[o], a]);
                }
            }
        }
        (8, 0) | (4, 0) | (1, 0) => {
            let row_bits = w * bpp;
            let row_stride = ((row_bits + 31) / 32) * 4;
            for y in 0..h {
                let base = y * row_stride;
                for x in 0..w {
                    let bit = x * bpp;
                    let byte = base + (bit >> 3);
                    if byte >= pixels.len() {
                        break;
                    }
                    let idx = match bpp {
                        8 => pixels[byte] as usize,
                        4 => {
                            if bit & 7 == 0 {
                                (pixels[byte] >> 4) as usize
                            } else {
                                (pixels[byte] & 0x0F) as usize
                            }
                        }
                        _ => ((pixels[byte] >> (7 - (bit & 7))) & 1) as usize,
                    };
                    let c = palette.get(idx).copied().unwrap_or([0, 0, 0]);
                    put(&mut bmp, x, y, [c[0], c[1], c[2], 255]);
                }
            }
        }
        (8, 1) => decode_rle8(&mut bmp, pixels, w, h, top_down, &palette),
        (4, 2) => decode_rle4(&mut bmp, pixels, w, h, top_down, &palette),
        _ => return None,
    }

    Some(bmp)
}

fn put_idx(bmp: &mut Bitmap, x: usize, y: usize, w: usize, h: usize, top_down: bool, idx: usize, palette: &[[u8; 3]]) {
    let c = palette.get(idx).copied().unwrap_or([0, 0, 0]);
    let dy = if top_down { y } else { h - 1 - y };
    if x < w && dy < h {
        let i = (dy * w + x) * 4;
        bmp.data[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
    }
}

fn decode_rle8(bmp: &mut Bitmap, p: &[u8], w: usize, h: usize, td: bool, pal: &[[u8; 3]]) {
    let (mut x, mut y) = (0usize, 0usize);
    let mut i = 0usize;
    while i + 1 < p.len() {
        let n = p[i] as usize;
        let val = p[i + 1];
        i += 2;
        if n > 0 {
            for _ in 0..n {
                put_idx(bmp, x, y, w, h, td, val as usize, pal);
                x += 1;
            }
        } else {
            match val {
                0 => {
                    x = 0;
                    y += 1;
                }
                1 => break, // end of bitmap
                2 => {
                    if i + 1 < p.len() {
                        x += p[i] as usize;
                        y += p[i + 1] as usize;
                        i += 2;
                    }
                }
                _ => {
                    let cnt = val as usize;
                    for _ in 0..cnt {
                        if i < p.len() {
                            put_idx(bmp, x, y, w, h, td, p[i] as usize, pal);
                            x += 1;
                            i += 1;
                        }
                    }
                    if cnt & 1 == 1 {
                        i += 1; // pad to word boundary
                    }
                }
            }
        }
    }
}

fn decode_rle4(bmp: &mut Bitmap, p: &[u8], w: usize, h: usize, td: bool, pal: &[[u8; 3]]) {
    let (mut x, mut y) = (0usize, 0usize);
    let mut i = 0usize;
    while i + 1 < p.len() {
        let n = p[i] as usize;
        let val = p[i + 1];
        i += 2;
        if n > 0 {
            let hi = (val >> 4) as usize;
            let lo = (val & 0x0F) as usize;
            for k in 0..n {
                let idx = if k & 1 == 0 { hi } else { lo };
                put_idx(bmp, x, y, w, h, td, idx, pal);
                x += 1;
            }
        } else {
            match val {
                0 => {
                    x = 0;
                    y += 1;
                }
                1 => break,
                2 => {
                    if i + 1 < p.len() {
                        x += p[i] as usize;
                        y += p[i + 1] as usize;
                        i += 2;
                    }
                }
                _ => {
                    let cnt = val as usize;
                    let bytes = (cnt + 1) / 2;
                    for k in 0..cnt {
                        let b = p.get(i + k / 2).copied().unwrap_or(0);
                        let idx = if k & 1 == 0 { (b >> 4) as usize } else { (b & 0x0F) as usize };
                        put_idx(bmp, x, y, w, h, td, idx, pal);
                        x += 1;
                    }
                    i += bytes;
                    if bytes & 1 == 1 {
                        i += 1; // pad to word boundary
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bmp_24bit() {
        // 2x2 24-bit bottom-up: rows padded to 4 bytes (6 -> 8).
        let mut f = Vec::new();
        f.extend_from_slice(b"BM");
        f.extend_from_slice(&[0; 8]); // size + reserved
        f.extend_from_slice(&54u32.to_le_bytes()); // pixel offset
        // DIB header (40 bytes)
        f.extend_from_slice(&40u32.to_le_bytes());
        f.extend_from_slice(&2i32.to_le_bytes()); // width
        f.extend_from_slice(&2i32.to_le_bytes()); // height (bottom-up)
        f.extend_from_slice(&1u16.to_le_bytes()); // planes
        f.extend_from_slice(&24u16.to_le_bytes()); // bpp
        f.extend_from_slice(&0u32.to_le_bytes()); // compression
        f.extend_from_slice(&[0u8; 20]); // rest of header
        // Bottom row first: blue, green ; then top row: red, white.
        f.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0]); // BGR blue, green + pad
        f.extend_from_slice(&[0, 0, 255, 255, 255, 255, 0, 0]); // BGR red, white + pad
        let bmp = decode(&f).unwrap();
        assert_eq!((bmp.w, bmp.h), (2, 2));
        // Top-left after flip = red.
        assert_eq!(&bmp.data[0..4], &[255, 0, 0, 255]);
        // Bottom-left (row 1) = blue.
        assert_eq!(&bmp.data[(1 * 2 + 0) * 4..(1 * 2 + 0) * 4 + 4], &[0, 0, 255, 255]);
    }
}
