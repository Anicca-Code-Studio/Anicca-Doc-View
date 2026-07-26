//! Netpbm (PNM) decoder: PBM/PGM/PPM in both ASCII (P1-P3) and binary (P4-P6).

use crate::raster::Bitmap;

pub fn is_pnm(b: &[u8]) -> bool {
    b.len() >= 2 && b[0] == b'P' && (b'1'..=b'6').contains(&b[1])
}

pub fn decode(b: &[u8]) -> Option<Bitmap> {
    if !is_pnm(b) {
        return None;
    }
    let kind = b[1];
    let mut t = Tokenizer { d: b, p: 2 };

    let w = t.next_uint()?;
    let h = t.next_uint()?;
    if w == 0 || h == 0 || w * h > 64_000_000 {
        return None;
    }
    // Bilevel (P1/P4) has no maxval; the others do.
    let maxval = if kind == b'1' || kind == b'4' { 1 } else { t.next_uint()? };
    let maxval = maxval.max(1);

    let mut bmp = Bitmap::new(w, h);
    let scale = |v: usize| -> u8 { ((v * 255 + maxval / 2) / maxval) as u8 };

    match kind {
        b'1' => {
            // ASCII bitmap: 1 = black.
            for i in 0..w * h {
                let v = t.next_uint()?;
                let g = if v == 0 { 255 } else { 0 };
                put(&mut bmp, i, [g, g, g, 255]);
            }
        }
        b'2' => {
            for i in 0..w * h {
                let g = scale(t.next_uint()?);
                put(&mut bmp, i, [g, g, g, 255]);
            }
        }
        b'3' => {
            for i in 0..w * h {
                let r = scale(t.next_uint()?);
                let g = scale(t.next_uint()?);
                let bl = scale(t.next_uint()?);
                put(&mut bmp, i, [r, g, bl, 255]);
            }
        }
        b'4' => {
            // Binary bitmap: each row packed MSB-first, 1 = black.
            let row_bytes = (w + 7) / 8;
            let start = t.p_after_single_ws();
            for y in 0..h {
                let row = start + y * row_bytes;
                for x in 0..w {
                    let byte = b.get(row + (x >> 3)).copied().unwrap_or(0);
                    let bit = (byte >> (7 - (x & 7))) & 1;
                    let g = if bit == 1 { 0 } else { 255 };
                    put(&mut bmp, y * w + x, [g, g, g, 255]);
                }
            }
        }
        b'5' => {
            let start = t.p_after_single_ws();
            let bytes_per = if maxval > 255 { 2 } else { 1 };
            for i in 0..w * h {
                let o = start + i * bytes_per;
                let v = if bytes_per == 2 {
                    ((b.get(o).copied().unwrap_or(0) as usize) << 8) | b.get(o + 1).copied().unwrap_or(0) as usize
                } else {
                    b.get(o).copied().unwrap_or(0) as usize
                };
                let g = scale(v);
                put(&mut bmp, i, [g, g, g, 255]);
            }
        }
        b'6' => {
            let start = t.p_after_single_ws();
            let bytes_per = if maxval > 255 { 2 } else { 1 };
            for i in 0..w * h {
                let o = start + i * 3 * bytes_per;
                let rd = |k: usize| -> usize {
                    let oo = o + k * bytes_per;
                    if bytes_per == 2 {
                        ((b.get(oo).copied().unwrap_or(0) as usize) << 8) | b.get(oo + 1).copied().unwrap_or(0) as usize
                    } else {
                        b.get(oo).copied().unwrap_or(0) as usize
                    }
                };
                put(&mut bmp, i, [scale(rd(0)), scale(rd(1)), scale(rd(2)), 255]);
            }
        }
        _ => return None,
    }
    Some(bmp)
}

fn put(bmp: &mut Bitmap, i: usize, px: [u8; 4]) {
    let o = i * 4;
    if o + 4 <= bmp.data.len() {
        bmp.data[o..o + 4].copy_from_slice(&px);
    }
}

struct Tokenizer<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Tokenizer<'a> {
    fn next_uint(&mut self) -> Option<usize> {
        loop {
            // Skip whitespace and comments.
            while self.p < self.d.len() && self.d[self.p].is_ascii_whitespace() {
                self.p += 1;
            }
            if self.p < self.d.len() && self.d[self.p] == b'#' {
                while self.p < self.d.len() && self.d[self.p] != b'\n' {
                    self.p += 1;
                }
                continue;
            }
            break;
        }
        let start = self.p;
        while self.p < self.d.len() && self.d[self.p].is_ascii_digit() {
            self.p += 1;
        }
        if self.p == start {
            return None;
        }
        std::str::from_utf8(&self.d[start..self.p]).ok()?.parse().ok()
    }

    /// After the last header token, exactly one whitespace byte precedes binary
    /// raster data; return the offset of the raster.
    fn p_after_single_ws(&self) -> usize {
        (self.p + 1).min(self.d.len())
    }
}
