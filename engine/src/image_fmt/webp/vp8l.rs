//! VP8L lossless WebP decoder, written from scratch against the VP8L
//! bitstream spec (predictor / colour / subtract-green / colour-indexing
//! transforms, meta-Huffman entropy groups, LZ77 back-references, colour cache).

use crate::raster::Bitmap;

pub fn decode(data: &[u8]) -> Option<Bitmap> {
    if data.is_empty() || data[0] != 0x2F {
        return None;
    }
    let mut br = BitReader::new(&data[1..]);
    let w = br.read(14) as usize + 1;
    let h = br.read(14) as usize + 1;
    let _alpha = br.read(1);
    let _version = br.read(3);
    if w * h > 64_000_000 {
        return None;
    }

    let (mut argb, tw, th) = decode_stream(&mut br, w, h, true)?;
    // `argb` is w*h ARGB u32; transforms were recorded and applied inside.
    debug_assert_eq!((tw, th), (w, h));

    let mut bmp = Bitmap::new(w, h);
    for (i, &px) in argb.iter().enumerate().take(w * h) {
        let o = i * 4;
        bmp.data[o] = (px >> 16) as u8; // R
        bmp.data[o + 1] = (px >> 8) as u8; // G
        bmp.data[o + 2] = px as u8; // B
        bmp.data[o + 3] = (px >> 24) as u8; // A
    }
    argb.clear();
    Some(bmp)
}

/// Decodes a headerless VP8L alpha image stream (dimensions supplied
/// externally) and returns the per-pixel alpha from the green channel.
pub fn decode_alpha(data: &[u8], w: usize, h: usize) -> Option<Vec<u8>> {
    if w * h > 64_000_000 {
        return None;
    }
    let mut br = BitReader::new(data);
    let (argb, _, _) = decode_stream(&mut br, w, h, true)?;
    Some(argb.iter().take(w * h).map(|&p| (p >> 8) as u8).collect())
}

/// Decodes one image stream (top level or a transform's sub-image) and returns
/// the ARGB pixels plus their dimensions.
fn decode_stream(br: &mut BitReader, mut w: usize, h: usize, top: bool) -> Option<(Vec<u32>, usize, usize)> {
    let mut transforms: Vec<Transform> = Vec::new();
    if top {
        while br.read(1) == 1 {
            let t = read_transform(br, w, h)?;
            // Colour-indexing bundles several palette indices per pixel, which
            // reduces the width the entropy image (and any later transforms) use.
            if let Transform::ColorIndex { width_bits, .. } = &t {
                if *width_bits > 0 {
                    w = (w + (1 << width_bits) - 1) >> width_bits;
                }
            }
            transforms.push(t);
        }
    }

    // Colour cache.
    let cache_bits = if br.read(1) == 1 { br.read(4) as usize } else { 0 };

    let (groups, group_bits, group_xsize) = read_huffman_groups(br, w, h, top, cache_bits)?;

    let pixels = decode_pixels(br, w, h, cache_bits, &groups, group_bits, group_xsize)?;

    // Apply transforms in reverse (they were listed in application order).
    let mut argb = pixels;
    let mut cur_w = w;
    for t in transforms.iter().rev() {
        argb = apply_transform(t, argb, &mut cur_w, h)?;
    }
    Some((argb, cur_w, h))
}

// ── transforms ───────────────────────────────────────────────────────────────

enum Transform {
    Predictor { bits: usize, data: Vec<u32>, blocks_w: usize },
    Color { bits: usize, data: Vec<u32>, blocks_w: usize },
    SubtractGreen,
    // `width_bits` = packing bits (0/1/2/3); `out_w` = un-bundled width to
    // restore on inverse.
    ColorIndex { table: Vec<u32>, width_bits: usize, out_w: usize },
}

fn read_transform(br: &mut BitReader, w: usize, h: usize) -> Option<Transform> {
    match br.read(2) {
        0 => {
            // Predictor.
            let bits = br.read(3) as usize + 2;
            let bw = subsample_size(w, bits);
            let bh = subsample_size(h, bits);
            let (data, _, _) = decode_stream(br, bw, bh, false)?;
            Some(Transform::Predictor { bits, data, blocks_w: bw })
        }
        1 => {
            let bits = br.read(3) as usize + 2;
            let bw = subsample_size(w, bits);
            let bh = subsample_size(h, bits);
            let (data, _, _) = decode_stream(br, bw, bh, false)?;
            Some(Transform::Color { bits, data, blocks_w: bw })
        }
        2 => Some(Transform::SubtractGreen),
        3 => {
            let n = br.read(8) as usize + 1;
            let (mut table, _, _) = decode_stream(br, n, 1, false)?;
            // The palette is stored delta-coded; undo the running sum.
            for i in 1..table.len() {
                table[i] = add_argb(table[i], table[i - 1]);
            }
            table.resize(n.max(table.len()), 0);
            let width_bits = if n <= 2 {
                3
            } else if n <= 4 {
                2
            } else if n <= 16 {
                1
            } else {
                0
            };
            Some(Transform::ColorIndex { table, width_bits, out_w: w })
        }
        _ => None,
    }
}

fn subsample_size(size: usize, bits: usize) -> usize {
    (size + (1 << bits) - 1) >> bits
}

fn apply_transform(t: &Transform, argb: Vec<u32>, w: &mut usize, h: usize) -> Option<Vec<u32>> {
    match t {
        Transform::SubtractGreen => {
            let mut out = argb;
            for px in out.iter_mut() {
                let g = (*px >> 8) & 0xFF;
                let r = ((*px >> 16) + g) & 0xFF;
                let b = (*px + g) & 0xFF;
                *px = (*px & 0xFF00_FF00) | (r << 16) | b;
            }
            Some(out)
        }
        Transform::ColorIndex { table, width_bits, out_w } => {
            let packed_w = *w;
            let out_w = *out_w;
            let pixels_per = 1usize << width_bits; // 1/2/4/8 indices per source pixel
            let bpp = 8 >> width_bits; // bits per palette index
            let mask = pixels_per - 1;
            let idx_mask = (1u32 << bpp) - 1;
            let mut out = vec![0u32; out_w * h];
            for y in 0..h {
                for x in 0..out_w {
                    let packed_x = x >> width_bits;
                    let green = (argb[y * packed_w + packed_x] >> 8) & 0xFF;
                    let sub = x & mask;
                    let idx = ((green >> (sub * bpp)) & idx_mask) as usize;
                    out[y * out_w + x] = table.get(idx).copied().unwrap_or(0);
                }
            }
            *w = out_w;
            Some(out)
        }
        Transform::Predictor { bits, data, blocks_w } => {
            let width = *w;
            let mut out = argb;
            apply_predictor_transform(&mut out, width, h, *bits, data, *blocks_w);
            Some(out)
        }
        Transform::Color { bits, data, blocks_w } => {
            let width = *w;
            let mut out = argb;
            apply_color_transform(&mut out, width, h, *bits, data, *blocks_w);
            Some(out)
        }
    }
}

fn add_argb(a: u32, b: u32) -> u32 {
    let aa = ((a >> 24) + (b >> 24)) & 0xFF;
    let ar = (((a >> 16) & 0xFF) + ((b >> 16) & 0xFF)) & 0xFF;
    let ag = (((a >> 8) & 0xFF) + ((b >> 8) & 0xFF)) & 0xFF;
    let ab = ((a & 0xFF) + (b & 0xFF)) & 0xFF;
    (aa << 24) | (ar << 16) | (ag << 8) | ab
}

fn apply_predictor_transform(argb: &mut [u32], w: usize, h: usize, bits: usize, data: &[u32], bw: usize) {
    for y in 0..h {
        for x in 0..w {
            let idx = y * w + x;
            let pred = if x == 0 && y == 0 {
                0xFF00_0000
            } else if y == 0 {
                argb[idx - 1]
            } else if x == 0 {
                argb[idx - w]
            } else {
                let block = data[(y >> bits) * bw + (x >> bits)];
                let mode = (block >> 8) & 0xFF;
                predict(mode, argb, x, y, w)
            };
            argb[idx] = add_argb(argb[idx], pred);
        }
    }
}

fn predict(mode: u32, argb: &[u32], x: usize, y: usize, w: usize) -> u32 {
    let i = y * w + x;
    let left = argb[i - 1];
    let top = argb[i - w];
    let tl = argb[i - w - 1];
    // Top-right: at the last column this intentionally wraps to the current
    // row's first pixel (already decoded), matching the reference decoder's
    // contiguous-buffer access `argb[i - w + 1]`.
    let tr = argb[i - w + 1];
    match mode {
        0 => 0xFF00_0000,
        1 => left,
        2 => top,
        3 => tr,
        4 => tl,
        5 => avg2(avg2(left, tr), top),
        6 => avg2(left, tl),
        7 => avg2(left, top),
        8 => avg2(tl, top),
        9 => avg2(top, tr),
        10 => avg2(avg2(left, tl), avg2(top, tr)),
        11 => select(left, top, tl),
        12 => clamp_add_subtract_full(left, top, tl),
        13 => clamp_add_subtract_half(avg2(left, top), tl),
        _ => 0xFF00_0000,
    }
}

fn avg2(a: u32, b: u32) -> u32 {
    let mut out = 0u32;
    for s in [0, 8, 16, 24] {
        let v = (((a >> s) & 0xFF) + ((b >> s) & 0xFF)) / 2;
        out |= (v & 0xFF) << s;
    }
    out
}

fn select(l: u32, t: u32, tl: u32) -> u32 {
    // Predictor 11 = Select(top, left, topleft): return top when
    // sum|left-tl| <= sum|top-tl|, else left. The tie must resolve to top to
    // match the reference decoder.
    let mut s_left = 0i32; // sum|left - tl|
    let mut s_top = 0i32; // sum|top - tl|
    for s in [0, 8, 16, 24] {
        let ll = ((l >> s) & 0xFF) as i32;
        let tt = ((t >> s) & 0xFF) as i32;
        let ttl = ((tl >> s) & 0xFF) as i32;
        s_left += (ll - ttl).abs();
        s_top += (tt - ttl).abs();
    }
    if s_left <= s_top {
        t
    } else {
        l
    }
}

fn clamp_add_subtract_full(a: u32, b: u32, c: u32) -> u32 {
    let mut out = 0u32;
    for s in [0, 8, 16, 24] {
        let v = (((a >> s) & 0xFF) as i32 + ((b >> s) & 0xFF) as i32 - ((c >> s) & 0xFF) as i32).clamp(0, 255);
        out |= (v as u32) << s;
    }
    out
}

fn clamp_add_subtract_half(a: u32, b: u32) -> u32 {
    let mut out = 0u32;
    for s in [0, 8, 16, 24] {
        let aa = ((a >> s) & 0xFF) as i32;
        let bb = ((b >> s) & 0xFF) as i32;
        let v = (aa + (aa - bb) / 2).clamp(0, 255);
        out |= (v as u32) << s;
    }
    out
}

fn apply_color_transform(argb: &mut [u32], w: usize, h: usize, bits: usize, data: &[u32], bw: usize) {
    for y in 0..h {
        for x in 0..w {
            let block = data[(y >> bits) * bw + (x >> bits)];
            let gtr = (block & 0xFF) as i8 as i32; // green_to_red
            let gtb = ((block >> 8) & 0xFF) as i8 as i32; // green_to_blue
            let rtb = ((block >> 16) & 0xFF) as i8 as i32; // red_to_blue
            let i = y * w + x;
            let px = argb[i];
            let g = ((px >> 8) & 0xFF) as i32;
            let mut r = ((px >> 16) & 0xFF) as i32;
            let mut b = (px & 0xFF) as i32;
            r = (r + color_delta(gtr, g)) & 0xFF;
            b = (b + color_delta(gtb, g) + color_delta(rtb, r)) & 0xFF;
            argb[i] = (px & 0xFF00_FF00) | ((r as u32) << 16) | (b as u32);
        }
    }
}

fn color_delta(t: i32, c: i32) -> i32 {
    // ColorTransformDelta: (t * ((c<<24)>>24)) >> 5, with c already 0..255.
    let c8 = (c as i8) as i32;
    (t * c8) >> 5
}

// ── huffman ──────────────────────────────────────────────────────────────────

struct Huffman {
    // Fast table: for the max length, map code -> (symbol, len).
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn from_lengths(lengths: &[u8]) -> Option<Huffman> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        // Assign canonical codes ordered by (length, symbol).
        let mut symbols: Vec<u16> = Vec::new();
        for len in 1..16u8 {
            for (sym, &l) in lengths.iter().enumerate() {
                if l == len {
                    symbols.push(sym as u16);
                }
            }
        }
        if symbols.is_empty() {
            return None;
        }
        Some(Huffman { counts, symbols })
    }

    /// Single-symbol tree (all codes map to one value).
    fn single(sym: u16) -> Huffman {
        Huffman { counts: [0; 16], symbols: vec![sym] }
    }

    fn read(&self, br: &mut BitReader) -> u16 {
        if self.counts.iter().all(|&c| c == 0) {
            return self.symbols[0];
        }
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;
        for len in 1..16usize {
            code |= br.read(1) as i32;
            let count = self.counts[len] as i32;
            if code - first < count {
                return self.symbols[(index + (code - first)) as usize];
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        0
    }
}

const CODE_LENGTH_ORDER: [usize; 19] = [17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

fn read_huffman(br: &mut BitReader, alphabet: usize) -> Option<Huffman> {
    if br.read(1) == 1 {
        // Simple code length code.
        let num = br.read(1) as usize + 1;
        let first = if br.read(1) == 1 { br.read(8) } else { br.read(1) };
        let mut lengths = vec![0u8; alphabet];
        if (first as usize) < alphabet {
            // simple codes: symbols get length 1 (num symbols)
        }
        let mut syms = vec![first as u16];
        if num == 2 {
            syms.push(br.read(8) as u16);
        }
        // Build a tiny tree: 1 symbol -> single; 2 symbols -> 1-bit each.
        if num == 1 {
            return Some(Huffman::single(syms[0]));
        }
        for &s in &syms {
            if (s as usize) < alphabet {
                lengths[s as usize] = 1;
            }
        }
        return Huffman::from_lengths(&lengths);
    }

    // Normal: read code-length code lengths, then the symbol code lengths.
    let num_code_lengths = br.read(4) as usize + 4;
    let mut cl_lengths = [0u8; 19];
    for i in 0..num_code_lengths {
        cl_lengths[CODE_LENGTH_ORDER[i]] = br.read(3) as u8;
    }
    let cl_huff = Huffman::from_lengths(&cl_lengths)?;

    let mut lengths = vec![0u8; alphabet];
    let max_symbol = if br.read(1) == 1 {
        let len_bits = br.read(3) * 2 + 2;
        2 + br.read(len_bits) as usize
    } else {
        alphabet
    };

    let mut sym = 0usize;
    let mut prev_len = 8u8;
    let mut count = 0usize;
    while sym < alphabet && count < max_symbol {
        let code = cl_huff.read(br);
        count += 1;
        if code < 16 {
            lengths[sym] = code as u8;
            if code != 0 {
                prev_len = code as u8;
            }
            sym += 1;
        } else {
            let (repeat, rep_len) = match code {
                16 => (3 + br.read(2) as usize, prev_len),
                17 => (3 + br.read(3) as usize, 0),
                _ => (11 + br.read(7) as usize, 0),
            };
            for _ in 0..repeat {
                if sym >= alphabet {
                    break;
                }
                lengths[sym] = rep_len;
                sym += 1;
            }
        }
    }
    Huffman::from_lengths(&lengths)
}

struct HGroup {
    green: Huffman,
    red: Huffman,
    blue: Huffman,
    alpha: Huffman,
    dist: Huffman,
}

fn read_huffman_groups(
    br: &mut BitReader,
    w: usize,
    h: usize,
    top: bool,
    cache_bits: usize,
) -> Option<(Vec<HGroup>, usize, usize)> {
    let mut group_bits = 0usize;
    let mut group_xsize = 1usize;
    let mut num_groups = 1usize;
    let mut entropy: Vec<u32> = Vec::new();

    if top && br.read(1) == 1 {
        // Meta-Huffman: an entropy image selects a group per block.
        group_bits = br.read(3) as usize + 2;
        let bw = subsample_size(w, group_bits);
        let bh = subsample_size(h, group_bits);
        let (img, _, _) = decode_stream(br, bw, bh, false)?;
        group_xsize = bw;
        let mut maxg = 0u32;
        entropy = img;
        for px in &entropy {
            let g = (px >> 8) & 0xFFFF; // red<<8 | green holds the index
            let idx = ((px >> 16) & 0xFF) << 8 | ((px >> 8) & 0xFF);
            let _ = g;
            maxg = maxg.max(idx);
        }
        num_groups = maxg as usize + 1;
    }

    let cache_size = if cache_bits > 0 { 1 << cache_bits } else { 0 };
    let green_alpha = 256 + 24 + cache_size;

    let mut groups = Vec::with_capacity(num_groups);
    for _ in 0..num_groups {
        groups.push(HGroup {
            green: read_huffman(br, green_alpha)?,
            red: read_huffman(br, 256)?,
            blue: read_huffman(br, 256)?,
            alpha: read_huffman(br, 256)?,
            dist: read_huffman(br, 40)?,
        });
    }

    // Stash the entropy image for pixel decode by packing it into group_xsize
    // and returning via a side channel: encode into the first group vec ordering.
    // Simpler: return entropy through a thread-local-free approach — store it in
    // a Vec appended after groups is not possible, so we recompute mapping here.
    ENTROPY.with(|e| *e.borrow_mut() = entropy);
    Some((groups, group_bits, group_xsize))
}

use std::cell::RefCell;
thread_local! {
    static ENTROPY: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

fn decode_pixels(
    br: &mut BitReader,
    w: usize,
    h: usize,
    cache_bits: usize,
    groups: &[HGroup],
    group_bits: usize,
    group_xsize: usize,
) -> Option<Vec<u32>> {
    let entropy = ENTROPY.with(|e| e.borrow().clone());
    let cache_size = if cache_bits > 0 { 1usize << cache_bits } else { 0 };
    let mut cache = vec![0u32; cache_size];

    let mut argb = vec![0u32; w * h];
    let mut x = 0usize;
    let mut y = 0usize;

    let group_at = |x: usize, y: usize| -> usize {
        if entropy.is_empty() {
            0
        } else {
            let bx = x >> group_bits;
            let by = y >> group_bits;
            let px = entropy[by * group_xsize + bx];
            (((px >> 16) & 0xFF) << 8 | ((px >> 8) & 0xFF)) as usize
        }
    };

    let mut pos = 0usize;
    let total = w * h;
    while pos < total {
        let g = &groups[group_at(x, y).min(groups.len() - 1)];
        let s = g.green.read(br) as usize;
        if s < 256 {
            // Literal ARGB.
            let green = s as u32;
            let red = g.red.read(br) as u32;
            let blue = g.blue.read(br) as u32;
            let alpha = g.alpha.read(br) as u32;
            let px = (alpha << 24) | (red << 16) | (green << 8) | blue;
            argb[pos] = px;
            if cache_size > 0 {
                cache[cache_index(px, cache_bits)] = px;
            }
            advance(&mut x, &mut y, w);
            pos += 1;
        } else if s < 256 + 24 {
            // LZ77 back-reference.
            let len = read_lz_extra(br, s - 256);
            let dist_sym = g.dist.read(br) as usize;
            let dist_code = read_lz_extra(br, dist_sym);
            let dist = map_distance(dist_code, w);
            if dist == 0 || dist > pos {
                return None;
            }
            for _ in 0..len {
                if pos >= total {
                    break;
                }
                let px = argb[pos - dist];
                argb[pos] = px;
                if cache_size > 0 {
                    cache[cache_index(px, cache_bits)] = px;
                }
                advance(&mut x, &mut y, w);
                pos += 1;
            }
        } else {
            // Colour-cache reference. The key is a direct index, but every
            // produced pixel (this one included) is re-inserted by hash, which
            // can land in a different slot — so we must insert here too, else
            // the cache diverges from the encoder.
            let idx = s - 256 - 24;
            if idx >= cache.len() {
                return None;
            }
            let px = cache[idx];
            argb[pos] = px;
            cache[cache_index(px, cache_bits)] = px;
            advance(&mut x, &mut y, w);
            pos += 1;
        }
    }
    Some(argb)
}

fn advance(x: &mut usize, y: &mut usize, w: usize) {
    *x += 1;
    if *x >= w {
        *x = 0;
        *y += 1;
    }
}

fn cache_index(argb: u32, bits: usize) -> usize {
    ((0x1e35a7bdu32.wrapping_mul(argb)) >> (32 - bits)) as usize
}

/// Prefix-coded length/distance: symbols >=4 carry extra bits.
fn read_lz_extra(br: &mut BitReader, sym: usize) -> usize {
    if sym < 4 {
        return sym + 1;
    }
    let extra_bits = (sym - 2) >> 1;
    let offset = (2 + (sym & 1)) << extra_bits;
    offset + br.read(extra_bits as u32) as usize + 1
}

/// Distance codes 1..120 map to 2-D neighbourhood offsets; larger are plain.
fn map_distance(code: usize, w: usize) -> usize {
    if code > 120 {
        return code - 120;
    }
    let d = DIST_MAP[code - 1] as i32;
    let yoff = d >> 4;
    let xoff = 8 - (d & 0xF);
    let dist = yoff * w as i32 + xoff;
    if dist < 1 {
        1
    } else {
        dist as usize
    }
}

#[rustfmt::skip]
const DIST_MAP: [u8; 120] = [
    0x18,0x07,0x17,0x19,0x28,0x06,0x27,0x29,0x16,0x1a,0x26,0x2a,0x38,0x05,0x37,0x39,
    0x15,0x1b,0x36,0x3a,0x25,0x2b,0x48,0x04,0x47,0x49,0x14,0x1c,0x35,0x3b,0x46,0x4a,
    0x24,0x2c,0x58,0x45,0x4b,0x34,0x3c,0x03,0x57,0x59,0x13,0x1d,0x56,0x5a,0x23,0x2d,
    0x44,0x4c,0x55,0x5b,0x33,0x3d,0x68,0x02,0x67,0x69,0x12,0x1e,0x66,0x6a,0x22,0x2e,
    0x54,0x5c,0x43,0x4d,0x65,0x6b,0x32,0x3e,0x78,0x01,0x77,0x79,0x53,0x5d,0x11,0x1f,
    0x64,0x6c,0x42,0x4e,0x76,0x7a,0x21,0x2f,0x75,0x7b,0x31,0x3f,0x63,0x6d,0x52,0x5e,
    0x00,0x74,0x7c,0x41,0x4f,0x62,0x6e,0x51,0x5f,0x73,0x7d,0x30,0x72,0x7e,0x40,0x50,
    0x61,0x6f,0x71,0x7f,0x60,0x70,0x00,0x00,
];

// ── bit reader (LSB-first, little-endian bytes) ─────────────────────────────────

struct BitReader<'a> {
    d: &'a [u8],
    bitpos: usize,
}

impl<'a> BitReader<'a> {
    fn new(d: &'a [u8]) -> BitReader<'a> {
        BitReader { d, bitpos: 0 }
    }

    fn read(&mut self, n: u32) -> u32 {
        let mut v = 0u32;
        for i in 0..n {
            let byte = self.d.get(self.bitpos >> 3).copied().unwrap_or(0);
            let bit = (byte >> (self.bitpos & 7)) & 1;
            v |= (bit as u32) << i;
            self.bitpos += 1;
        }
        v
    }
}
