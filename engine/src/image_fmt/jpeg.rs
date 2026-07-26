//! JPEG decoder (ITU-T T.81 / JFIF), written from scratch.
//!
//! Supports baseline and progressive DCT, Huffman entropy coding, restart
//! markers, chroma subsampling, grayscale / YCbCr / (YC)CMYK colour, JFIF
//! density, and EXIF orientation. No third-party codec is used.

use crate::pdf::colorspace::cmyk_to_rgb;
use crate::raster::Bitmap;

pub struct Decoded {
    pub bitmap: Bitmap,
    pub dpi: Option<f32>,
}

pub fn is_jpeg(b: &[u8]) -> bool {
    b.len() >= 3 && b[0] == 0xFF && b[1] == 0xD8 && b[2] == 0xFF
}

/// zigzag[k] = natural (row-major) index of the k-th coefficient in zigzag order.
#[rustfmt::skip]
const ZIGZAG: [usize; 64] = [
    0,1,8,16,9,2,3,10,17,24,32,25,18,11,4,5,
    12,19,26,33,40,48,41,34,27,20,13,6,7,14,21,28,
    35,42,49,56,57,50,43,36,29,22,15,23,30,37,44,51,
    58,59,52,45,38,31,39,46,53,60,61,54,47,55,62,63,
];

#[derive(Clone, Default)]
struct HuffTable {
    // Canonical decode tables (T.81 Annex F).
    mincode: [i32; 17],
    maxcode: [i32; 17], // -1 when no codes of that length
    valptr: [usize; 17],
    values: Vec<u8>,
}

impl HuffTable {
    fn build(counts: &[u8; 16], values: Vec<u8>) -> HuffTable {
        let mut t = HuffTable { values, ..Default::default() };
        let mut code = 0i32;
        let mut k = 0usize;
        for len in 1..=16usize {
            let n = counts[len - 1] as usize;
            if n == 0 {
                t.maxcode[len] = -1;
            } else {
                t.valptr[len] = k;
                t.mincode[len] = code;
                code += n as i32;
                t.maxcode[len] = code - 1;
                k += n;
            }
            code <<= 1;
        }
        t
    }
}

#[derive(Clone, Default)]
struct Component {
    id: u8,
    h: usize,
    v: usize,
    qt: usize,
    // Per-scan Huffman selectors (set at SOS).
    dc: usize,
    ac: usize,
    // Block grid (interleaved dimensions).
    bpl: usize,
    bpc: usize,
    coeffs: Vec<i32>, // bpl*bpc*64, natural order
    pred: i32,
}

struct Bits<'a> {
    d: &'a [u8],
    p: usize,
    acc: u32,
    n: u32,
    eob_run: u32,
}

impl<'a> Bits<'a> {
    fn new(d: &'a [u8], p: usize) -> Bits<'a> {
        Bits { d, p, acc: 0, n: 0, eob_run: 0 }
    }

    fn refill(&mut self) {
        while self.n <= 24 {
            if self.p >= self.d.len() {
                self.acc <<= 8;
                self.n += 8;
                continue;
            }
            let b = self.d[self.p];
            if b == 0xFF {
                let nx = self.d.get(self.p + 1).copied().unwrap_or(0);
                if nx == 0 {
                    self.p += 2;
                    self.acc = (self.acc << 8) | 0xFF;
                    self.n += 8;
                } else {
                    // Marker: stop consuming; feed zero bits. p stays on the 0xFF.
                    self.acc <<= 8;
                    self.n += 8;
                }
            } else {
                self.p += 1;
                self.acc = (self.acc << 8) | b as u32;
                self.n += 8;
            }
        }
    }

    fn get_bit(&mut self) -> u32 {
        if self.n == 0 {
            self.refill();
        }
        self.n -= 1;
        (self.acc >> self.n) & 1
    }

    fn get_bits(&mut self, s: u32) -> u32 {
        if s == 0 {
            return 0;
        }
        if self.n < s {
            self.refill();
        }
        self.n -= s;
        (self.acc >> self.n) & ((1 << s) - 1)
    }

    fn huff(&mut self, t: &HuffTable) -> u8 {
        let mut code = 0i32;
        for len in 1..=16usize {
            code = (code << 1) | self.get_bit() as i32;
            if t.maxcode[len] >= 0 && code <= t.maxcode[len] {
                let idx = t.valptr[len] + (code - t.mincode[len]) as usize;
                return t.values.get(idx).copied().unwrap_or(0);
            }
        }
        0
    }

    /// Drops buffered bits and skips to just past the next RSTn marker.
    fn restart(&mut self) {
        self.acc = 0;
        self.n = 0;
        self.eob_run = 0;
        while self.p + 1 < self.d.len() {
            if self.d[self.p] == 0xFF {
                let m = self.d[self.p + 1];
                if (0xD0..=0xD7).contains(&m) {
                    self.p += 2;
                    return;
                }
                if m != 0x00 && m != 0xFF {
                    return; // some other marker: scan is over
                }
            }
            self.p += 1;
        }
    }
}

fn extend(v: u32, s: u32) -> i32 {
    let v = v as i32;
    if s == 0 || v >= (1 << (s - 1)) {
        v
    } else {
        v - (1 << s) + 1
    }
}

fn be16(d: &[u8], p: usize) -> usize {
    ((d[p] as usize) << 8) | d[p + 1] as usize
}

struct Frame {
    progressive: bool,
    width: usize,
    height: usize,
    comps: Vec<Component>,
    max_h: usize,
    max_v: usize,
    mcus_x: usize,
    mcus_y: usize,
}

pub fn decode(bytes: &[u8]) -> Option<Decoded> {
    if !is_jpeg(bytes) {
        return None;
    }
    let mut p = 2usize; // past SOI

    let mut qt: [[i32; 64]; 4] = [[0; 64]; 4]; // natural order
    let mut dc_tabs: [HuffTable; 4] = Default::default();
    let mut ac_tabs: [HuffTable; 4] = Default::default();
    let mut restart_interval = 0usize;
    let mut frame: Option<Frame> = None;
    let mut adobe_transform: Option<u8> = None;
    let mut dpi: Option<f32> = None;
    let mut orientation: u8 = 1;

    while p + 1 < bytes.len() {
        if bytes[p] != 0xFF {
            p += 1;
            continue;
        }
        // Skip fill bytes.
        let mut m = bytes[p + 1];
        let mut mp = p + 1;
        while m == 0xFF && mp + 1 < bytes.len() {
            mp += 1;
            m = bytes[mp];
        }
        p = mp + 1;

        match m {
            0xD9 => break, // EOI
            0xD0..=0xD7 => continue,
            0x01 => continue,
            _ => {}
        }
        if p + 2 > bytes.len() {
            break;
        }
        let seg_len = be16(bytes, p);
        let seg_start = p + 2;
        let seg_end = (p + seg_len).min(bytes.len());
        let seg = &bytes[seg_start..seg_end];

        match m {
            0xDB => parse_dqt(seg, &mut qt),
            0xC4 => parse_dht(seg, &mut dc_tabs, &mut ac_tabs),
            0xDD => {
                if seg.len() >= 2 {
                    restart_interval = be16(seg, 0);
                }
            }
            0xC0 | 0xC1 | 0xC2 => {
                frame = parse_sof(seg, m == 0xC2);
            }
            0xE0 => {
                // APP0 / JFIF density.
                if seg.len() >= 14 && &seg[0..5] == b"JFIF\0" {
                    let unit = seg[7];
                    let xd = be16(seg, 8) as f32;
                    if xd > 0.0 {
                        dpi = Some(match unit {
                            2 => xd * 2.54, // dots per cm -> per inch
                            _ => xd,        // 1 = dpi; 0 = aspect only, treat as dpi
                        });
                    }
                }
            }
            0xE1 => {
                // APP1 / EXIF orientation.
                if let Some(o) = exif_orientation(seg) {
                    orientation = o;
                }
            }
            0xEE => {
                // APP14 / Adobe colour transform.
                if seg.len() >= 12 && &seg[0..5] == b"Adobe" {
                    adobe_transform = Some(seg[11]);
                }
            }
            0xDA => {
                // Start of scan: parse header, then decode entropy data.
                let f = frame.as_mut()?;
                let hdr = parse_sos(seg, f)?;
                let mut bits = Bits::new(bytes, seg_end);
                decode_scan(&mut bits, f, &hdr, &dc_tabs, &ac_tabs, restart_interval);
                p = bits.p;
                continue;
            }
            _ => {}
        }
        p = seg_end;
    }

    let f = frame?;
    let bmp = reconstruct(&f, &qt, adobe_transform)?;
    let bmp = apply_orientation(bmp, orientation);
    Some(Decoded { bitmap: bmp, dpi })
}

fn parse_dqt(mut seg: &[u8], qt: &mut [[i32; 64]; 4]) {
    while !seg.is_empty() {
        let pq = seg[0] >> 4; // 0 = 8-bit, 1 = 16-bit
        let tq = (seg[0] & 0x0F) as usize;
        seg = &seg[1..];
        if tq >= 4 {
            return;
        }
        if pq == 0 {
            if seg.len() < 64 {
                return;
            }
            for k in 0..64 {
                qt[tq][ZIGZAG[k]] = seg[k] as i32;
            }
            seg = &seg[64..];
        } else {
            if seg.len() < 128 {
                return;
            }
            for k in 0..64 {
                qt[tq][ZIGZAG[k]] = be16(seg, k * 2) as i32;
            }
            seg = &seg[128..];
        }
    }
}

fn parse_dht(mut seg: &[u8], dc: &mut [HuffTable; 4], ac: &mut [HuffTable; 4]) {
    while seg.len() >= 17 {
        let tc = seg[0] >> 4;
        let th = (seg[0] & 0x0F) as usize;
        let mut counts = [0u8; 16];
        counts.copy_from_slice(&seg[1..17]);
        let total: usize = counts.iter().map(|&c| c as usize).sum();
        if seg.len() < 17 + total || th >= 4 {
            return;
        }
        let values = seg[17..17 + total].to_vec();
        let table = HuffTable::build(&counts, values);
        if tc == 0 {
            dc[th] = table;
        } else {
            ac[th] = table;
        }
        seg = &seg[17 + total..];
    }
}

fn parse_sof(seg: &[u8], progressive: bool) -> Option<Frame> {
    if seg.len() < 6 {
        return None;
    }
    let height = be16(seg, 1);
    let width = be16(seg, 3);
    let nc = seg[5] as usize;
    if width == 0 || height == 0 || nc == 0 || seg.len() < 6 + nc * 3 {
        return None;
    }
    if width.saturating_mul(height) > 100_000_000 {
        return None;
    }
    let mut comps = Vec::with_capacity(nc);
    let mut max_h = 1;
    let mut max_v = 1;
    for i in 0..nc {
        let o = 6 + i * 3;
        let h = (seg[o + 1] >> 4) as usize;
        let v = (seg[o + 1] & 0x0F) as usize;
        let h = h.max(1);
        let v = v.max(1);
        max_h = max_h.max(h);
        max_v = max_v.max(v);
        comps.push(Component {
            id: seg[o],
            h,
            v,
            qt: (seg[o + 2] & 0x0F) as usize,
            ..Default::default()
        });
    }
    let mcus_x = (width + 8 * max_h - 1) / (8 * max_h);
    let mcus_y = (height + 8 * max_v - 1) / (8 * max_v);
    for c in comps.iter_mut() {
        c.bpl = mcus_x * c.h;
        c.bpc = mcus_y * c.v;
        c.coeffs = vec![0i32; c.bpl * c.bpc * 64];
    }
    Some(Frame { progressive, width, height, comps, max_h, max_v, mcus_x, mcus_y })
}

struct ScanHeader {
    comps: Vec<usize>, // indices into frame.comps, in scan order
    ss: usize,
    se: usize,
    ah: u32,
    al: u32,
}

fn parse_sos(seg: &[u8], f: &mut Frame) -> Option<ScanHeader> {
    if seg.is_empty() {
        return None;
    }
    let ns = seg[0] as usize;
    if seg.len() < 1 + ns * 2 + 3 {
        return None;
    }
    let mut comps = Vec::with_capacity(ns);
    for i in 0..ns {
        let cs = seg[1 + i * 2];
        let td = (seg[2 + i * 2] >> 4) as usize;
        let ta = (seg[2 + i * 2] & 0x0F) as usize;
        let idx = f.comps.iter().position(|c| c.id == cs)?;
        f.comps[idx].dc = td;
        f.comps[idx].ac = ta;
        comps.push(idx);
    }
    let o = 1 + ns * 2;
    Some(ScanHeader {
        comps,
        ss: seg[o] as usize,
        se: seg[o + 1] as usize,
        ah: (seg[o + 2] >> 4) as u32,
        al: (seg[o + 2] & 0x0F) as u32,
    })
}

fn decode_scan(
    bits: &mut Bits,
    f: &mut Frame,
    hdr: &ScanHeader,
    dc: &[HuffTable; 4],
    ac: &[HuffTable; 4],
    ri: usize,
) {
    for &ci in &hdr.comps {
        f.comps[ci].pred = 0;
    }
    bits.eob_run = 0;

    let interleaved = hdr.comps.len() > 1;

    if !f.progressive {
        decode_baseline(bits, f, hdr, dc, ac, ri, interleaved);
    } else if hdr.ss == 0 {
        decode_prog_dc(bits, f, hdr, dc, ri, interleaved);
    } else {
        // Progressive AC scans are always single-component (non-interleaved).
        decode_prog_ac(bits, f, hdr, ac, ri);
    }
}

fn block_at<'a>(c: &'a mut Component, row: usize, col: usize) -> &'a mut [i32] {
    let i = (row * c.bpl + col) * 64;
    &mut c.coeffs[i..i + 64]
}

fn decode_baseline(
    bits: &mut Bits,
    f: &mut Frame,
    hdr: &ScanHeader,
    dc: &[HuffTable; 4],
    ac: &[HuffTable; 4],
    ri: usize,
    interleaved: bool,
) {
    let (units_x, units_y) = scan_units(f, hdr, interleaved);
    let mut since_restart = 0usize;
    for uy in 0..units_y {
        for ux in 0..units_x {
            if ri > 0 && since_restart == ri {
                for &ci in &hdr.comps {
                    f.comps[ci].pred = 0;
                }
                bits.restart();
                since_restart = 0;
            }
            for &ci in &hdr.comps {
                let (bh, bv) = if interleaved {
                    (f.comps[ci].h, f.comps[ci].v)
                } else {
                    (1, 1)
                };
                for by in 0..bv {
                    for bx in 0..bh {
                        let (row, col) = if interleaved {
                            (uy * f.comps[ci].v + by, ux * f.comps[ci].h + bx)
                        } else {
                            (uy, ux)
                        };
                        let dct = f.comps[ci].dc;
                        let act = f.comps[ci].ac;
                        let pred = f.comps[ci].pred;
                        let blk = block_at(&mut f.comps[ci], row, col);
                        let t = bits.huff(&dc[dct]);
                        let diff = extend(bits.get_bits(t as u32), t as u32);
                        let ndc = pred + diff;
                        blk[0] = ndc;
                        let mut k = 1usize;
                        while k < 64 {
                            let rs = bits.huff(&ac[act]);
                            let r = (rs >> 4) as usize;
                            let s = (rs & 0x0F) as u32;
                            if s == 0 {
                                if r == 15 {
                                    k += 16;
                                    continue;
                                }
                                break;
                            }
                            k += r;
                            if k >= 64 {
                                break;
                            }
                            blk[ZIGZAG[k]] = extend(bits.get_bits(s), s);
                            k += 1;
                        }
                        f.comps[ci].pred = ndc;
                    }
                }
            }
            since_restart += 1;
        }
    }
}

fn decode_prog_dc(
    bits: &mut Bits,
    f: &mut Frame,
    hdr: &ScanHeader,
    dc: &[HuffTable; 4],
    ri: usize,
    interleaved: bool,
) {
    let (units_x, units_y) = scan_units(f, hdr, interleaved);
    let mut since_restart = 0usize;
    for uy in 0..units_y {
        for ux in 0..units_x {
            if ri > 0 && since_restart == ri {
                for &ci in &hdr.comps {
                    f.comps[ci].pred = 0;
                }
                bits.restart();
                since_restart = 0;
            }
            for &ci in &hdr.comps {
                let (bh, bv) = if interleaved {
                    (f.comps[ci].h, f.comps[ci].v)
                } else {
                    (1, 1)
                };
                for by in 0..bv {
                    for bx in 0..bh {
                        let (row, col) = if interleaved {
                            (uy * f.comps[ci].v + by, ux * f.comps[ci].h + bx)
                        } else {
                            (uy, ux)
                        };
                        if hdr.ah == 0 {
                            let dct = f.comps[ci].dc;
                            let pred = f.comps[ci].pred;
                            let t = bits.huff(&dc[dct]);
                            let diff = extend(bits.get_bits(t as u32), t as u32);
                            let ndc = pred + diff;
                            f.comps[ci].pred = ndc;
                            let blk = block_at(&mut f.comps[ci], row, col);
                            blk[0] = ndc << hdr.al;
                        } else {
                            let bit = bits.get_bit() as i32;
                            let blk = block_at(&mut f.comps[ci], row, col);
                            blk[0] |= bit << hdr.al;
                        }
                    }
                }
            }
            since_restart += 1;
        }
    }
}

fn decode_prog_ac(bits: &mut Bits, f: &mut Frame, hdr: &ScanHeader, ac: &[HuffTable; 4], ri: usize) {
    let ci = hdr.comps[0];
    // Non-interleaved: block grid from this component's own sample size.
    let comp_w = (f.width * f.comps[ci].h + f.max_h - 1) / f.max_h;
    let comp_h = (f.height * f.comps[ci].v + f.max_v - 1) / f.max_v;
    let bx_count = (comp_w + 7) / 8;
    let by_count = (comp_h + 7) / 8;
    let act = f.comps[ci].ac;

    let mut since_restart = 0usize;
    for by in 0..by_count {
        for bx in 0..bx_count {
            if ri > 0 && since_restart == ri {
                bits.restart();
                since_restart = 0;
            }
            let blk = block_at(&mut f.comps[ci], by, bx);
            if hdr.ah == 0 {
                ac_first(bits, blk, &ac[act], hdr.ss, hdr.se, hdr.al);
            } else {
                ac_refine(bits, blk, &ac[act], hdr.ss, hdr.se, hdr.al);
            }
            since_restart += 1;
        }
    }
}

fn ac_first(bits: &mut Bits, blk: &mut [i32], t: &HuffTable, ss: usize, se: usize, al: u32) {
    if bits.eob_run > 0 {
        bits.eob_run -= 1;
        return;
    }
    let mut k = ss;
    while k <= se {
        let rs = bits.huff(t);
        let r = (rs >> 4) as usize;
        let s = (rs & 0x0F) as u32;
        if s == 0 {
            if r < 15 {
                bits.eob_run = (1u32 << r) - 1;
                if r > 0 {
                    bits.eob_run += bits.get_bits(r as u32);
                }
                break;
            }
            k += 16;
            continue;
        }
        k += r;
        if k > se {
            break;
        }
        blk[ZIGZAG[k]] = extend(bits.get_bits(s), s) << al;
        k += 1;
    }
}

fn ac_refine(bits: &mut Bits, blk: &mut [i32], t: &HuffTable, ss: usize, se: usize, al: u32) {
    let bit = 1i32 << al;

    if bits.eob_run > 0 {
        // Inside an EOB run: only apply correction bits to non-zero history.
        bits.eob_run -= 1;
        for k in ss..=se {
            let pos = ZIGZAG[k];
            if blk[pos] != 0 && bits.get_bit() != 0 && (blk[pos] & bit) == 0 {
                blk[pos] += if blk[pos] > 0 { bit } else { -bit };
            }
        }
        return;
    }

    let mut k = ss;
    loop {
        let rs = bits.huff(t);
        let mut r = (rs >> 4) as i32;
        let s = rs & 0x0F;
        let mut value = 0i32;
        if s == 0 {
            if r < 15 {
                // Start of an EOB run; refine remaining history in this block
                // by letting r exceed the band so no new value is placed.
                bits.eob_run = (1u32 << r) - 1;
                if r > 0 {
                    bits.eob_run += bits.get_bits(r as u32);
                }
                r = 64;
            }
            // r == 15: advance over 16 zero-history coefficients (r stays 15).
        } else {
            value = if bits.get_bit() != 0 { bit } else { -bit };
        }

        while k <= se {
            let pos = ZIGZAG[k];
            k += 1;
            if blk[pos] != 0 {
                if bits.get_bit() != 0 && (blk[pos] & bit) == 0 {
                    blk[pos] += if blk[pos] > 0 { bit } else { -bit };
                }
            } else {
                if r == 0 {
                    if value != 0 {
                        blk[pos] = value;
                    }
                    break;
                }
                r -= 1;
            }
        }

        if k > se {
            break;
        }
    }
}

/// Iteration extent for a scan, in MCUs (interleaved) or blocks (single comp).
fn scan_units(f: &Frame, hdr: &ScanHeader, interleaved: bool) -> (usize, usize) {
    if interleaved {
        (f.mcus_x, f.mcus_y)
    } else {
        let ci = hdr.comps[0];
        let comp_w = (f.width * f.comps[ci].h + f.max_h - 1) / f.max_h;
        let comp_h = (f.height * f.comps[ci].v + f.max_v - 1) / f.max_v;
        ((comp_w + 7) / 8, (comp_h + 7) / 8)
    }
}

// ── reconstruction ─────────────────────────────────────────────────────────────

fn reconstruct(f: &Frame, qt: &[[i32; 64]; 4], adobe: Option<u8>) -> Option<Bitmap> {
    // Dequantise + IDCT every component into a sample plane.
    let mut planes: Vec<Vec<u8>> = Vec::with_capacity(f.comps.len());
    let mut dims: Vec<(usize, usize)> = Vec::with_capacity(f.comps.len());
    for c in &f.comps {
        let pw = c.bpl * 8;
        let ph = c.bpc * 8;
        let mut plane = vec![0u8; pw * ph];
        let q = &qt[c.qt.min(3)];
        let mut block = [0i32; 64];
        for by in 0..c.bpc {
            for bx in 0..c.bpl {
                let src = &c.coeffs[(by * c.bpl + bx) * 64..(by * c.bpl + bx) * 64 + 64];
                for i in 0..64 {
                    block[i] = src[i] * q[i];
                }
                let mut out = [0u8; 64];
                idct8x8(&block, &mut out);
                for yy in 0..8 {
                    let dy = by * 8 + yy;
                    let row = dy * pw + bx * 8;
                    plane[row..row + 8].copy_from_slice(&out[yy * 8..yy * 8 + 8]);
                }
            }
        }
        planes.push(plane);
        dims.push((pw, ph));
    }

    let (w, h) = (f.width, f.height);
    let nc = f.comps.len();
    let mut bmp = Bitmap::new(w, h);

    // Bilinear chroma upsampling: for a full-resolution component this reduces
    // to an exact integer lookup; for subsampled chroma it smooths block edges
    // (matching how mainstream decoders reconstruct 4:2:0 / 4:2:2).
    let sample = |ci: usize, x: usize, y: usize| -> u8 {
        let (pw, ph) = dims[ci];
        let sx = f.comps[ci].h as f32 / f.max_h as f32;
        let sy = f.comps[ci].v as f32 / f.max_v as f32;
        let fx = (x as f32 + 0.5) * sx - 0.5;
        let fy = (y as f32 + 0.5) * sy - 0.5;
        let x0 = fx.floor();
        let y0 = fy.floor();
        let tx = fx - x0;
        let ty = fy - y0;
        let clampx = |v: f32| (v as i32).clamp(0, pw as i32 - 1) as usize;
        let clampy = |v: f32| (v as i32).clamp(0, ph as i32 - 1) as usize;
        let (x0i, x1i) = (clampx(x0), clampx(x0 + 1.0));
        let (y0i, y1i) = (clampy(y0), clampy(y0 + 1.0));
        let p = &planes[ci];
        let c00 = p[y0i * pw + x0i] as f32;
        let c10 = p[y0i * pw + x1i] as f32;
        let c01 = p[y1i * pw + x0i] as f32;
        let c11 = p[y1i * pw + x1i] as f32;
        let top = c00 + (c10 - c00) * tx;
        let bot = c01 + (c11 - c01) * tx;
        (top + (bot - top) * ty).round().clamp(0.0, 255.0) as u8
    };

    // Adobe transform: 0 = none, 1 = YCbCr, 2 = YCCK. Absent 3-comp defaults to
    // YCbCr (JFIF); absent 4-comp defaults to CMYK.
    let transform = adobe.unwrap_or(if nc == 3 { 1 } else { 0 });

    for y in 0..h {
        for x in 0..w {
            let o = (y * w + x) * 4;
            match nc {
                1 => {
                    let g = sample(0, x, y);
                    bmp.data[o] = g;
                    bmp.data[o + 1] = g;
                    bmp.data[o + 2] = g;
                    bmp.data[o + 3] = 255;
                }
                3 => {
                    let a = sample(0, x, y) as f32;
                    let b = sample(1, x, y) as f32;
                    let c = sample(2, x, y) as f32;
                    let (r, g, bl) = if transform == 0 {
                        (a, b, c)
                    } else {
                        ycc_to_rgb(a, b, c)
                    };
                    bmp.data[o] = clamp8(r);
                    bmp.data[o + 1] = clamp8(g);
                    bmp.data[o + 2] = clamp8(bl);
                    bmp.data[o + 3] = 255;
                }
                4 => {
                    // Adobe CMYK/YCCK is stored inverted.
                    let (mut c0, mut c1, mut c2, k) = (
                        sample(0, x, y) as f32,
                        sample(1, x, y) as f32,
                        sample(2, x, y) as f32,
                        sample(3, x, y) as f32,
                    );
                    if transform == 2 {
                        let (r, g, b) = ycc_to_rgb(c0, c1, c2);
                        c0 = r;
                        c1 = g;
                        c2 = b;
                    }
                    // Now c0..c2 are (inverted) CMY, k inverted K.
                    let cc = 255.0 - c0;
                    let mm = 255.0 - c1;
                    let yy = 255.0 - c2;
                    let kk = 255.0 - k;
                    let rgb = cmyk_to_rgb(cc / 255.0, mm / 255.0, yy / 255.0, kk / 255.0);
                    bmp.data[o] = rgb[0];
                    bmp.data[o + 1] = rgb[1];
                    bmp.data[o + 2] = rgb[2];
                    bmp.data[o + 3] = 255;
                }
                _ => return None,
            }
        }
    }
    Some(bmp)
}

fn ycc_to_rgb(y: f32, cb: f32, cr: f32) -> (f32, f32, f32) {
    let r = y + 1.402 * (cr - 128.0);
    let g = y - 0.344136 * (cb - 128.0) - 0.714136 * (cr - 128.0);
    let b = y + 1.772 * (cb - 128.0);
    (r, g, b)
}

fn clamp8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

/// Separable 8x8 inverse DCT. `input` is dequantised, natural order; `out` is
/// clamped 8-bit samples (level-shifted by +128).
fn idct8x8(input: &[i32; 64], out: &mut [u8; 64]) {
    // Precomputed scaled cosines: C[u][x] = cu * cos((2x+1)u*pi/16), cu at u==0
    // folded into the 1/2 normalisation below.
    let cos = cos_table();
    let mut tmp = [0f32; 64];
    // Rows.
    for y in 0..8 {
        let base = y * 8;
        for x in 0..8 {
            let mut s = 0f32;
            for u in 0..8 {
                s += cos[u][x] * input[base + u] as f32;
            }
            tmp[base + x] = s;
        }
    }
    // Columns.
    for x in 0..8 {
        for y in 0..8 {
            let mut s = 0f32;
            for v in 0..8 {
                s += cos[v][y] * tmp[v * 8 + x];
            }
            let val = (s / 4.0) + 128.0;
            out[y * 8 + x] = clamp8(val);
        }
    }
}

use std::sync::OnceLock;
static COS_TABLE: OnceLock<[[f32; 8]; 8]> = OnceLock::new();

fn cos_table() -> &'static [[f32; 8]; 8] {
    COS_TABLE.get_or_init(|| {
        let mut t = [[0f32; 8]; 8];
        for u in 0..8 {
            let cu = if u == 0 { (0.5f32).sqrt() } else { 1.0 };
            for x in 0..8 {
                t[u][x] = cu * ((2 * x + 1) as f32 * u as f32 * std::f32::consts::PI / 16.0).cos();
            }
        }
        t
    })
}

fn exif_orientation(seg: &[u8]) -> Option<u8> {
    if seg.len() < 14 || &seg[0..6] != b"Exif\0\0" {
        return None;
    }
    let tiff = &seg[6..];
    let le = &tiff[0..2] == b"II";
    let rd16 = |o: usize| -> usize {
        if le {
            tiff[o] as usize | ((tiff[o + 1] as usize) << 8)
        } else {
            ((tiff[o] as usize) << 8) | tiff[o + 1] as usize
        }
    };
    let rd32 = |o: usize| -> usize {
        if le {
            tiff[o] as usize | ((tiff[o + 1] as usize) << 8) | ((tiff[o + 2] as usize) << 16) | ((tiff[o + 3] as usize) << 24)
        } else {
            ((tiff[o] as usize) << 24) | ((tiff[o + 1] as usize) << 16) | ((tiff[o + 2] as usize) << 8) | tiff[o + 3] as usize
        }
    };
    if tiff.len() < 8 {
        return None;
    }
    let ifd = rd32(4);
    if ifd + 2 > tiff.len() {
        return None;
    }
    let n = rd16(ifd);
    for i in 0..n {
        let e = ifd + 2 + i * 12;
        if e + 12 > tiff.len() {
            break;
        }
        if rd16(e) == 0x0112 {
            return Some(rd16(e + 8) as u8);
        }
    }
    None
}

/// Applies EXIF orientation (1..8) to a decoded bitmap.
fn apply_orientation(bmp: Bitmap, o: u8) -> Bitmap {
    if o <= 1 || o > 8 {
        return bmp;
    }
    let (w, h) = (bmp.w, bmp.h);
    let (nw, nh) = if o >= 5 { (h, w) } else { (w, h) };
    let mut out = Bitmap::new(nw, nh);
    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = match o {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (h - 1 - y, x),
                7 => (h - 1 - y, w - 1 - x),
                8 => (y, w - 1 - x),
                _ => (x, y),
            };
            let si = (y * w + x) * 4;
            let di = (ny * nw + nx) * 4;
            out.data[di..di + 4].copy_from_slice(&bmp.data[si..si + 4]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idct_dc_only() {
        // A pure DC coefficient yields a flat block at DC/8 + 128.
        let mut input = [0i32; 64];
        input[0] = 8 * 8; // after dequant
        let mut out = [0u8; 64];
        idct8x8(&input, &mut out);
        // DC=64 -> row sum scaled: value = (64 * 0.5(row) *0.5(col)) *? check flat.
        let first = out[0];
        assert!(out.iter().all(|&v| v == first), "block not flat: {out:?}");
    }

    #[test]
    fn detects_jpeg_magic() {
        assert!(is_jpeg(&[0xFF, 0xD8, 0xFF, 0xE0]));
        assert!(!is_jpeg(&[0x89, 0x50]));
    }
}
