//! GIF decoder (87a/89a), written from scratch.
//!
//! Renders the first frame onto the logical screen. Implements GIF's own
//! LSB-first, variable-width LZW (distinct from the MSB-first PDF LZW in
//! `pdf::filter`), global/local colour tables, and the Graphic Control
//! Extension's transparent-index flag.

use crate::raster::Bitmap;

pub fn is_gif(bytes: &[u8]) -> bool {
    bytes.len() >= 6 && (&bytes[0..6] == b"GIF87a" || &bytes[0..6] == b"GIF89a")
}

fn le16(b: &[u8], i: usize) -> usize {
    b[i] as usize | ((b[i + 1] as usize) << 8)
}

pub fn decode(bytes: &[u8]) -> Option<Bitmap> {
    if !is_gif(bytes) || bytes.len() < 13 {
        return None;
    }
    let screen_w = le16(bytes, 6);
    let screen_h = le16(bytes, 8);
    if screen_w == 0 || screen_h == 0 || screen_w * screen_h > 64_000_000 {
        return None;
    }
    let packed = bytes[10];
    let global_flag = packed & 0x80 != 0;
    let global_size = 2usize << (packed & 0x07);

    let mut pos = 13usize;
    let global_palette = if global_flag {
        let p = read_palette(bytes, pos, global_size)?;
        pos += global_size * 3;
        p
    } else {
        Vec::new()
    };

    let mut bmp = Bitmap::new(screen_w, screen_h);
    let mut transparent: Option<u8> = None;

    while pos < bytes.len() {
        match bytes[pos] {
            0x21 => {
                // Extension. 0xF9 = Graphic Control (has transparency info).
                if pos + 1 >= bytes.len() {
                    break;
                }
                let label = bytes[pos + 1];
                pos += 2;
                if label == 0xF9 {
                    if pos < bytes.len() {
                        let bsize = bytes[pos] as usize;
                        if bsize >= 4 && pos + 1 + bsize <= bytes.len() {
                            let flags = bytes[pos + 1];
                            if flags & 0x01 != 0 {
                                transparent = Some(bytes[pos + 4]);
                            }
                        }
                    }
                }
                pos = skip_sub_blocks(bytes, pos);
            }
            0x2C => {
                // Image descriptor.
                if pos + 10 > bytes.len() {
                    break;
                }
                let ix = le16(bytes, pos + 1);
                let iy = le16(bytes, pos + 3);
                let iw = le16(bytes, pos + 5);
                let ih = le16(bytes, pos + 7);
                let lflags = bytes[pos + 9];
                pos += 10;

                let local_flag = lflags & 0x80 != 0;
                let interlaced = lflags & 0x40 != 0;
                let palette = if local_flag {
                    let lsize = 2usize << (lflags & 0x07);
                    let p = read_palette(bytes, pos, lsize)?;
                    pos += lsize * 3;
                    p
                } else {
                    global_palette.clone()
                };

                if pos >= bytes.len() {
                    break;
                }
                let min_code = bytes[pos];
                pos += 1;
                let (data, next) = gather_sub_blocks(bytes, pos);
                pos = next;

                let indices = lzw_decode_gif(&data, min_code, iw * ih);
                paint_frame(&mut bmp, &indices, ix, iy, iw, ih, interlaced, &palette, transparent);
                // First frame only for a static viewer.
                break;
            }
            0x3B => break, // trailer
            _ => {
                pos += 1;
            }
        }
    }

    Some(bmp)
}

fn read_palette(bytes: &[u8], start: usize, count: usize) -> Option<Vec<[u8; 3]>> {
    let end = start + count * 3;
    if end > bytes.len() {
        return None;
    }
    Some(bytes[start..end].chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect())
}

/// Advances past a chain of sub-blocks (each: 1 length byte + data), including
/// the terminating zero-length block.
fn skip_sub_blocks(bytes: &[u8], mut pos: usize) -> usize {
    while pos < bytes.len() {
        let len = bytes[pos] as usize;
        pos += 1;
        if len == 0 {
            break;
        }
        pos += len;
    }
    pos
}

/// Concatenates a sub-block chain into one buffer; returns (data, next_pos).
fn gather_sub_blocks(bytes: &[u8], mut pos: usize) -> (Vec<u8>, usize) {
    let mut out = Vec::new();
    while pos < bytes.len() {
        let len = bytes[pos] as usize;
        pos += 1;
        if len == 0 {
            break;
        }
        let end = (pos + len).min(bytes.len());
        out.extend_from_slice(&bytes[pos..end]);
        pos = end;
    }
    (out, pos)
}

/// GIF LZW: LSB-first bit packing, initial width = min_code+1, clear/EOI codes,
/// deferred-clear tolerant.
fn lzw_decode_gif(data: &[u8], min_code: u8, expected: usize) -> Vec<u8> {
    let min_code = min_code.clamp(2, 11) as u32;
    let clear = 1u32 << min_code;
    let eoi = clear + 1;

    let mut out: Vec<u8> = Vec::with_capacity(expected);
    let mut dict: Vec<Vec<u8>> = Vec::new();

    let reset = |dict: &mut Vec<Vec<u8>>| {
        dict.clear();
        for i in 0..clear {
            dict.push(vec![i as u8]);
        }
        dict.push(Vec::new()); // clear
        dict.push(Vec::new()); // eoi
    };
    reset(&mut dict);

    let mut width = min_code + 1;
    let mut bitbuf = 0u32;
    let mut bits = 0u32;
    let mut pos = 0usize;
    let mut prev: Option<u32> = None;

    loop {
        while bits < width {
            match data.get(pos) {
                Some(&b) => {
                    bitbuf |= (b as u32) << bits;
                    bits += 8;
                    pos += 1;
                }
                None => return out,
            }
        }
        let code = bitbuf & ((1 << width) - 1);
        bitbuf >>= width;
        bits -= width;

        if code == clear {
            reset(&mut dict);
            width = min_code + 1;
            prev = None;
            continue;
        }
        if code == eoi {
            return out;
        }

        let entry: Vec<u8> = if (code as usize) < dict.len() {
            dict[code as usize].clone()
        } else if let Some(p) = prev {
            // KwKwK: code is the one about to be added.
            let mut e = dict[p as usize].clone();
            if let Some(&f) = dict[p as usize].first() {
                e.push(f);
            }
            e
        } else {
            return out;
        };

        out.extend_from_slice(&entry);

        if let Some(p) = prev {
            let mut new_entry = dict[p as usize].clone();
            if let Some(&f) = entry.first() {
                new_entry.push(f);
            }
            if dict.len() < 4096 {
                dict.push(new_entry);
                // Widen just before the dict would overflow the current width.
                if dict.len() == (1 << width) && width < 12 {
                    width += 1;
                }
            }
        }
        prev = Some(code);

        if out.len() >= expected {
            return out;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_frame(
    bmp: &mut Bitmap,
    indices: &[u8],
    ix: usize,
    iy: usize,
    iw: usize,
    ih: usize,
    interlaced: bool,
    palette: &[[u8; 3]],
    transparent: Option<u8>,
) {
    // Interlaced rows arrive in four passes; map source row -> destination row.
    let row_order: Vec<usize> = if interlaced {
        let mut v = Vec::with_capacity(ih);
        for start_step in [(0usize, 8usize), (4, 8), (2, 4), (1, 2)] {
            let (start, step) = start_step;
            let mut r = start;
            while r < ih {
                v.push(r);
                r += step;
            }
        }
        v
    } else {
        (0..ih).collect()
    };

    for (src_row, &dst_row) in row_order.iter().enumerate() {
        for col in 0..iw {
            let si = src_row * iw + col;
            let idx = match indices.get(si) {
                Some(&i) => i,
                None => return,
            };
            if Some(idx) == transparent {
                continue;
            }
            let c = palette.get(idx as usize).copied().unwrap_or([0, 0, 0]);
            let dx = ix + col;
            let dy = iy + dst_row;
            if dx < bmp.w && dy < bmp.h {
                let i = (dy * bmp.w + dx) * 4;
                bmp.data[i] = c[0];
                bmp.data[i + 1] = c[1];
                bmp.data[i + 2] = c[2];
                bmp.data[i + 3] = 255;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gif_2x2_solid() {
        // Hand-built 2x2 GIF, 2-colour global table (black, white), all white.
        let mut f = Vec::new();
        f.extend_from_slice(b"GIF89a");
        f.extend_from_slice(&2u16.to_le_bytes()); // width
        f.extend_from_slice(&2u16.to_le_bytes()); // height
        f.push(0x80); // global table, size 2
        f.push(0); // bg
        f.push(0); // aspect
        f.extend_from_slice(&[0, 0, 0, 255, 255, 255]); // palette: black, white
        // Image descriptor
        f.push(0x2C);
        f.extend_from_slice(&0u16.to_le_bytes()); // left
        f.extend_from_slice(&0u16.to_le_bytes()); // top
        f.extend_from_slice(&2u16.to_le_bytes()); // w
        f.extend_from_slice(&2u16.to_le_bytes()); // h
        f.push(0); // no local table
        // LZW min code size 2. Encode the four pixels [1,1,1,1] as literals
        // with clear/eoi. Codes: clear=4, then 1,1,1,1, eoi=5. width=3.
        f.push(2);
        // Build LSB bitstream: 4,1,1,1,1,5 each 3 bits.
        let codes = [4u32, 1, 1, 1, 1, 5];
        let mut buf = 0u32;
        let mut nbits = 0u32;
        let mut bytes = Vec::new();
        for c in codes {
            buf |= c << nbits;
            nbits += 3;
            while nbits >= 8 {
                bytes.push((buf & 0xFF) as u8);
                buf >>= 8;
                nbits -= 8;
            }
        }
        if nbits > 0 {
            bytes.push((buf & 0xFF) as u8);
        }
        f.push(bytes.len() as u8);
        f.extend_from_slice(&bytes);
        f.push(0); // block terminator
        f.push(0x3B); // trailer

        let bmp = decode(&f).unwrap();
        assert_eq!((bmp.w, bmp.h), (2, 2));
        // All white.
        for px in bmp.data.chunks_exact(4) {
            assert_eq!(px, &[255, 255, 255, 255]);
        }
    }
}
