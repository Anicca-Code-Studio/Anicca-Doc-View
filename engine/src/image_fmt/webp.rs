//! WebP container decoder (RIFF): routes to the VP8 lossy and VP8L lossless
//! decoders, and applies an `ALPH` alpha plane for extended (`VP8X`) files.
//! Written from scratch; no third-party codec.

use crate::raster::Bitmap;

pub fn is_webp(b: &[u8]) -> bool {
    b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP"
}

fn le32(b: &[u8], o: usize) -> usize {
    b.get(o).copied().unwrap_or(0) as usize
        | ((b.get(o + 1).copied().unwrap_or(0) as usize) << 8)
        | ((b.get(o + 2).copied().unwrap_or(0) as usize) << 16)
        | ((b.get(o + 3).copied().unwrap_or(0) as usize) << 24)
}

pub fn decode(bytes: &[u8]) -> Option<Bitmap> {
    if !is_webp(bytes) {
        return None;
    }
    // Collect the frame chunk (VP8/VP8L) and an optional ALPH chunk.
    let mut frame: Option<(bool, std::ops::Range<usize>)> = None; // (is_lossless, range)
    let mut alph: Option<std::ops::Range<usize>> = None;

    let mut p = 12usize;
    while p + 8 <= bytes.len() {
        let fourcc = [bytes[p], bytes[p + 1], bytes[p + 2], bytes[p + 3]];
        let size = le32(bytes, p + 4);
        let body = p + 8;
        let end = (body + size).min(bytes.len());
        match &fourcc {
            b"VP8L" => {
                frame = Some((true, body..end));
                break;
            }
            b"VP8 " => {
                frame = Some((false, body..end));
                break;
            }
            b"ALPH" => alph = Some(body..end),
            _ => {} // VP8X and others: keep scanning
        }
        p = body + ((size + 1) & !1); // chunks padded to even size
    }

    let (is_lossless, range) = frame?;
    let mut bmp = if is_lossless {
        vp8l::decode(&bytes[range])?
    } else {
        vp8::decode(&bytes[range])?
    };

    if let Some(ar) = alph {
        apply_alpha(&mut bmp, &bytes[ar]);
    }
    Some(bmp)
}

/// Decodes and applies an `ALPH` alpha plane to `bmp`.
fn apply_alpha(bmp: &mut Bitmap, chunk: &[u8]) {
    if chunk.is_empty() {
        return;
    }
    let method = chunk[0];
    let filtering = (method >> 2) & 3;
    let compression = method & 3;
    let (w, h) = (bmp.w, bmp.h);
    let data = &chunk[1..];

    let mut alpha = match compression {
        0 => {
            if data.len() < w * h {
                return;
            }
            data[..w * h].to_vec()
        }
        1 => match vp8l::decode_alpha(data, w, h) {
            Some(a) => a,
            None => return,
        },
        _ => return,
    };

    unfilter_alpha(&mut alpha, w, h, filtering);

    for (i, &a) in alpha.iter().enumerate().take(w * h) {
        bmp.data[i * 4 + 3] = a;
    }
}

/// Undoes the ALPH per-row filter (0 none, 1 horizontal, 2 vertical, 3 gradient).
fn unfilter_alpha(a: &mut [u8], w: usize, h: usize, filter: u8) {
    if filter == 0 || w == 0 || h == 0 {
        return;
    }
    let clip = |v: i32| v.clamp(0, 255) as u8;
    for y in 0..h {
        let row = y * w;
        match filter {
            1 => {
                // Horizontal: first column predicted from the pixel above.
                let pred = if y == 0 { 0 } else { a[row - w] as i32 };
                a[row] = clip(a[row] as i32 + pred);
                for x in 1..w {
                    a[row + x] = clip(a[row + x] as i32 + a[row + x - 1] as i32);
                }
            }
            2 => {
                // Vertical: first row is horizontal; others predict from above.
                if y == 0 {
                    a[row] = a[row];
                    for x in 1..w {
                        a[row + x] = clip(a[row + x] as i32 + a[row + x - 1] as i32);
                    }
                } else {
                    for x in 0..w {
                        a[row + x] = clip(a[row + x] as i32 + a[row - w + x] as i32);
                    }
                }
            }
            3 => {
                // Gradient.
                if y == 0 {
                    for x in 1..w {
                        a[row + x] = clip(a[row + x] as i32 + a[row + x - 1] as i32);
                    }
                } else {
                    a[row] = clip(a[row] as i32 + a[row - w] as i32);
                    for x in 1..w {
                        let l = a[row + x - 1] as i32;
                        let t = a[row - w + x] as i32;
                        let tl = a[row - w + x - 1] as i32;
                        let pred = (l + t - tl).clamp(0, 255);
                        a[row + x] = clip(a[row + x] as i32 + pred);
                    }
                }
            }
            _ => {}
        }
    }
}

mod vp8;
mod vp8_tables;
mod vp8l;
