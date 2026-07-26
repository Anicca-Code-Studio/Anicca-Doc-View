//! VP8 lossy WebP keyframe (intra) decoder, written from scratch against
//! RFC 6386. Implements the boolean entropy decoder, frame/macroblock header
//! parsing, DCT token decoding, dequantisation, IDCT/IWHT, intra prediction
//! (16x16, chroma 8x8, and the ten 4x4 B modes), the deblocking loop filter,
//! and YUV->RGB. Constant tables (quantiser steps, token probabilities, B-pred
//! mode probabilities) are the RFC 6386 spec constants in `vp8_tables.rs`.

use crate::raster::Bitmap;
use super::vp8_tables::{AC_QUANT, COEFF_PROBS, COEFF_UPDATE_PROBS, DC_QUANT, KEYFRAME_BPRED_MODE_PROBS};

const MAX_SEGMENTS: usize = 4;
const NUM_DCT_TOKENS: usize = 12;

// Luma prediction modes.
const DC_PRED: i8 = 0;
const V_PRED: i8 = 1;
const H_PRED: i8 = 2;
const TM_PRED: i8 = 3;
const B_PRED: i8 = 4;

// 4x4 sub-block (B) prediction modes.
const B_DC: i8 = 0;
const B_TM: i8 = 1;
const B_VE: i8 = 2;
const B_HE: i8 = 3;
const B_LD: i8 = 4;
const B_RD: i8 = 5;
const B_VR: i8 = 6;
const B_VL: i8 = 7;
const B_HD: i8 = 8;
const B_HU: i8 = 9;

// DCT token values.
const DCT_0: i8 = 0;
const DCT_1: i8 = 1;
const DCT_4: i8 = 4;
const DCT_CAT1: i8 = 5;
const DCT_CAT6: i8 = 10;
const DCT_EOB: i8 = 11;

#[rustfmt::skip]
const ZIGZAG: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];
#[rustfmt::skip]
const COEFF_BANDS: [usize; 16] = [0, 1, 2, 3, 6, 4, 5, 6, 6, 6, 6, 6, 6, 6, 6, 7];

// DCT token tree (RFC 6386 §13.2). Positive = child index, <=0 = -token.
#[rustfmt::skip]
const DCT_TOKEN_TREE: [i8; 22] = [
    -DCT_EOB, 2, -DCT_0, 4, -DCT_1, 6, 8, 12, -2, 10, -3, -DCT_4,
    14, 16, -DCT_CAT1, -6, 18, 20, -7, -8, -9, -DCT_CAT6,
];
#[rustfmt::skip]
const PROB_DCT_CAT: [[u8; 12]; 6] = [
    [159, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [165, 145, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [173, 148, 140, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [176, 155, 140, 135, 0, 0, 0, 0, 0, 0, 0, 0],
    [180, 157, 141, 134, 130, 0, 0, 0, 0, 0, 0, 0],
    [254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129, 0],
];
const DCT_CAT_BASE: [i32; 6] = [5, 7, 11, 19, 35, 67];

const YMODE_TREE: [i8; 8] = [-B_PRED, 2, 4, 6, -DC_PRED, -V_PRED, -H_PRED, -TM_PRED];
const YMODE_PROBS: [u8; 4] = [145, 156, 163, 128];
const UV_MODE_TREE: [i8; 6] = [-DC_PRED, 2, -V_PRED, 4, -H_PRED, -TM_PRED];
const UV_MODE_PROBS: [u8; 3] = [142, 114, 183];
#[rustfmt::skip]
const BPRED_TREE: [i8; 18] = [
    -B_DC, 2, -B_TM, 4, -B_VE, 6, 8, 12, -B_HE, 10, -B_RD, -B_VR,
    -B_LD, 14, -B_VL, 16, -B_HD, -B_HU,
];
const SEGMENT_TREE: [i8; 6] = [2, 4, 0, -1, -2, -3];

// ── boolean entropy decoder (RFC 6386 §7) ───────────────────────────────────────

struct BoolDec<'a> {
    d: &'a [u8],
    pos: usize,
    value: u64,
    range: u32,
    bit_count: i32,
}

impl<'a> BoolDec<'a> {
    fn new(d: &'a [u8]) -> BoolDec<'a> {
        BoolDec { d, pos: 0, value: 0, range: 255, bit_count: -8 }
    }

    fn get_bit(&mut self, prob: u8) -> u32 {
        if self.bit_count < 0 {
            let b = self.d.get(self.pos).copied().unwrap_or(0);
            self.pos += 1;
            self.value = (self.value << 8) | b as u64;
            self.bit_count += 8;
        }
        let split = 1 + (((self.range - 1) * prob as u32) >> 8);
        let bigsplit = (split as u64) << self.bit_count;
        let ret = if self.value >= bigsplit {
            self.range -= split;
            self.value -= bigsplit;
            1
        } else {
            self.range = split;
            0
        };
        let shift = self.range.leading_zeros().saturating_sub(24);
        self.range <<= shift;
        self.bit_count -= shift as i32;
        ret
    }

    fn get_flag(&mut self) -> bool {
        self.get_bit(128) != 0
    }

    fn get_literal(&mut self, n: u8) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.get_bit(128);
        }
        v
    }

    /// Optional signed value: a present-flag, then n-bit magnitude + sign.
    fn get_optional_signed(&mut self, n: u8) -> i32 {
        if !self.get_flag() {
            return 0;
        }
        let mag = self.get_literal(n) as i32;
        if self.get_flag() {
            -mag
        } else {
            mag
        }
    }

    /// Walks a flat prob tree starting at flat index `start`.
    fn read_tree(&mut self, tree: &[i8], probs: &[u8], start: usize) -> i8 {
        let mut i = start;
        loop {
            let b = self.get_bit(probs[i >> 1]) as usize;
            let t = tree[i + b];
            if t <= 0 {
                return -t;
            }
            i = t as usize;
        }
    }
}

// ── model state ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Default)]
struct Segment {
    ydc: i16,
    yac: i16,
    y2dc: i16,
    y2ac: i16,
    uvdc: i16,
    uvac: i16,
    delta_values: bool,
    quantizer_level: i8,
    loopfilter_level: i8,
}

#[derive(Clone, Copy)]
struct MacroBlock {
    bpred: [i8; 16],
    complexity: [u8; 9],
    luma_mode: i8,
    chroma_mode: i8,
    segmentid: u8,
    coeffs_skipped: bool,
    non_zero_dct: bool,
}

impl Default for MacroBlock {
    fn default() -> Self {
        MacroBlock {
            bpred: [B_DC; 16],
            complexity: [0; 9],
            luma_mode: DC_PRED,
            chroma_mode: DC_PRED,
            segmentid: 0,
            coeffs_skipped: false,
            non_zero_dct: false,
        }
    }
}

type PlaneProbs = [[[u8; 11]; 3]; 8];

pub fn decode(data: &[u8]) -> Option<Bitmap> {
    Vp8::new(data)?.run()
}

struct Vp8<'a> {
    data: &'a [u8],
    pos: usize,

    width: usize,
    height: usize,
    mbw: usize,
    mbh: usize,

    ybuf: Vec<u8>,
    ubuf: Vec<u8>,
    vbuf: Vec<u8>,

    filter_type: bool,
    filter_level: u8,
    sharpness: u8,

    segments_enabled: bool,
    segments_update_map: bool,
    segment: [Segment; MAX_SEGMENTS],
    segment_probs: [u8; 3],

    lf_adjust: bool,
    ref_delta: [i32; 4],
    mode_delta: [i32; 4],

    token_probs: Box<[[PlaneProbs; 4]; 1]>, // wrapper to keep on heap; index [0][plane]
    prob_skip_false: Option<u8>,

    num_partitions: usize,
    part_ranges: Vec<(usize, usize)>, // (start,end) offsets into data for each partition

    top: Vec<MacroBlock>,
    left: MacroBlock,
    macroblocks: Vec<MacroBlock>,

    top_y: Vec<u8>,
    left_y: Vec<u8>,
    top_u: Vec<u8>,
    left_u: Vec<u8>,
    top_v: Vec<u8>,
    left_v: Vec<u8>,
}

impl<'a> Vp8<'a> {
    fn new(data: &'a [u8]) -> Option<Vp8<'a>> {
        Some(Vp8 {
            data,
            pos: 0,
            width: 0,
            height: 0,
            mbw: 0,
            mbh: 0,
            ybuf: Vec::new(),
            ubuf: Vec::new(),
            vbuf: Vec::new(),
            filter_type: false,
            filter_level: 0,
            sharpness: 0,
            segments_enabled: false,
            segments_update_map: false,
            segment: [Segment::default(); MAX_SEGMENTS],
            segment_probs: [255; 3],
            lf_adjust: false,
            ref_delta: [0; 4],
            mode_delta: [0; 4],
            token_probs: Box::new([[[[[0u8; 11]; 3]; 8]; 4]]),
            prob_skip_false: None,
            num_partitions: 1,
            part_ranges: Vec::new(),
            top: Vec::new(),
            left: MacroBlock::default(),
            macroblocks: Vec::new(),
            top_y: Vec::new(),
            left_y: Vec::new(),
            top_u: Vec::new(),
            left_u: Vec::new(),
            top_v: Vec::new(),
            left_v: Vec::new(),
        })
    }

    fn u8_at(&self, o: usize) -> u8 {
        self.data.get(o).copied().unwrap_or(0)
    }
    fn le16(&self, o: usize) -> usize {
        self.u8_at(o) as usize | ((self.u8_at(o + 1) as usize) << 8)
    }
    fn le24(&self, o: usize) -> usize {
        self.le16(o) | ((self.u8_at(o + 2) as usize) << 16)
    }

    fn run(mut self) -> Option<Bitmap> {
        // Frame tag (3 bytes).
        let tag = self.le24(0);
        let keyframe = tag & 1 == 0;
        if !keyframe {
            return None; // WebP still images are always keyframes
        }
        let first_part_size = tag >> 5;
        // Keyframe start code + dimensions.
        if self.u8_at(3) != 0x9d || self.u8_at(4) != 0x01 || self.u8_at(5) != 0x2a {
            return None;
        }
        self.width = self.le16(6) & 0x3FFF;
        self.height = self.le16(8) & 0x3FFF;
        if self.width == 0 || self.height == 0 || self.width * self.height > 64_000_000 {
            return None;
        }
        self.mbw = self.width.div_ceil(16);
        self.mbh = self.height.div_ceil(16);

        self.ybuf = vec![0u8; self.mbw * 16 * self.mbh * 16];
        self.ubuf = vec![0u8; self.mbw * 8 * self.mbh * 8];
        self.vbuf = vec![0u8; self.mbw * 8 * self.mbh * 8];

        self.top = vec![MacroBlock::default(); self.mbw];
        self.reset_borders();

        // Bind the data slice independently of `self` so the persistent
        // partition decoders can coexist with `&mut self` reconstruction calls.
        let data: &'a [u8] = self.data;

        let header_start = 10;
        let header_end = header_start + first_part_size;
        if header_end > data.len() {
            return None;
        }
        let mut b = BoolDec::new(&data[header_start..header_end]);

        // Header.
        let color_space = b.get_literal(1);
        let _clamp = b.get_literal(1);
        if color_space != 0 {
            return None;
        }

        self.segments_enabled = b.get_flag();
        if self.segments_enabled {
            self.read_segment_updates(&mut b);
        }

        self.filter_type = b.get_flag();
        self.filter_level = b.get_literal(6) as u8;
        self.sharpness = b.get_literal(3) as u8;

        self.lf_adjust = b.get_flag();
        if self.lf_adjust {
            self.read_lf_adjustments(&mut b);
        }

        self.num_partitions = 1 << b.get_literal(2);
        self.pos = header_end;
        self.init_partitions()?;

        self.read_quant_indices(&mut b);

        // Refresh entropy probs flag (ignored for a single keyframe).
        let _ = b.get_flag();

        // Token probability updates.
        *self.token_probs = [COEFF_PROBS];
        for i in 0..4 {
            for j in 0..8 {
                for k in 0..3 {
                    for t in 0..NUM_DCT_TOKENS - 1 {
                        if b.get_bit(COEFF_UPDATE_PROBS[i][j][k][t]) != 0 {
                            self.token_probs[0][i][j][k][t] = b.get_literal(8) as u8;
                        }
                    }
                }
            }
        }

        let mb_no_skip = b.get_literal(1);
        self.prob_skip_false = if mb_no_skip == 1 {
            Some(b.get_literal(8) as u8)
        } else {
            None
        };

        self.macroblocks = Vec::with_capacity(self.mbw * self.mbh);

        // Persistent token-partition decoders (one per partition), each read
        // continuously across the macroblock rows assigned to it.
        let mut parts: Vec<BoolDec> = self
            .part_ranges
            .iter()
            .map(|&(s, e)| BoolDec::new(&data[s..e]))
            .collect();

        // Decode all macroblocks. `b` is the first partition (mode data);
        // residuals come from the token partitions.
        for mby in 0..self.mbh {
            let p = mby % self.num_partitions;
            self.left = MacroBlock::default();
            self.left_y = vec![129u8; 1 + 16];
            self.left_u = vec![129u8; 1 + 8];
            self.left_v = vec![129u8; 1 + 8];
            for mbx in 0..self.mbw {
                let mut mb = self.read_mb_header(&mut b, mbx);
                let blocks = if !mb.coeffs_skipped {
                    self.read_residual(&mut parts[p], &mut mb, mbx)
                } else {
                    if mb.luma_mode != B_PRED {
                        self.left.complexity[0] = 0;
                        self.top[mbx].complexity[0] = 0;
                    }
                    for i in 1..9 {
                        self.left.complexity[i] = 0;
                        self.top[mbx].complexity[i] = 0;
                    }
                    [0i32; 384]
                };
                self.predict_luma(mbx, mby, &mb, &blocks);
                self.predict_chroma(mbx, mby, &mb, &blocks);
                self.macroblocks.push(mb);
            }
        }

        // Deblocking loop filter (whole frame, after reconstruction).
        for mby in 0..self.mbh {
            for mbx in 0..self.mbw {
                let mb = self.macroblocks[mby * self.mbw + mbx];
                self.loop_filter(mbx, mby, &mb);
            }
        }

        Some(self.to_rgba())
    }

    fn reset_borders(&mut self) {
        self.top_y = vec![127u8; self.mbw * 16 + 20];
        self.left_y = vec![129u8; 1 + 16];
        self.top_u = vec![127u8; self.mbw * 8 + 8];
        self.left_u = vec![129u8; 1 + 8];
        self.top_v = vec![127u8; self.mbw * 8 + 8];
        self.left_v = vec![129u8; 1 + 8];
    }

    fn init_partitions(&mut self) -> Option<()> {
        let n = self.num_partitions;
        self.part_ranges = Vec::with_capacity(n);
        // Size table for the first n-1 partitions (3 bytes each).
        let mut p = self.pos;
        let table = p;
        let mut data_start = p + 3 * (n - 1);
        for i in 0..n {
            if i < n - 1 {
                let size = self.le24(table + i * 3);
                let start = data_start;
                let end = (start + size).min(self.data.len());
                self.part_ranges.push((start, end));
                data_start = end;
            } else {
                self.part_ranges.push((data_start, self.data.len()));
            }
        }
        let _ = &mut p;
        Some(())
    }

    fn read_segment_updates(&mut self, b: &mut BoolDec) {
        self.segments_update_map = b.get_flag();
        let update_data = b.get_flag();
        if update_data {
            let abs_mode = b.get_flag();
            for s in self.segment.iter_mut() {
                s.delta_values = !abs_mode;
            }
            for s in self.segment.iter_mut() {
                s.quantizer_level = b.get_optional_signed(7) as i8;
            }
            for s in self.segment.iter_mut() {
                s.loopfilter_level = b.get_optional_signed(6) as i8;
            }
        }
        if self.segments_update_map {
            for i in 0..3 {
                self.segment_probs[i] = if b.get_flag() { b.get_literal(8) as u8 } else { 255 };
            }
        }
    }

    fn read_lf_adjustments(&mut self, b: &mut BoolDec) {
        if b.get_flag() {
            for i in 0..4 {
                self.ref_delta[i] = b.get_optional_signed(6);
            }
            for i in 0..4 {
                self.mode_delta[i] = b.get_optional_signed(6);
            }
        }
    }

    fn read_quant_indices(&mut self, b: &mut BoolDec) {
        let dcq = |i: i32| DC_QUANT[i.clamp(0, 127) as usize];
        let acq = |i: i32| AC_QUANT[i.clamp(0, 127) as usize];

        let yac_abs = b.get_literal(7) as i32;
        let ydc_d = b.get_optional_signed(4);
        let y2dc_d = b.get_optional_signed(4);
        let y2ac_d = b.get_optional_signed(4);
        let uvdc_d = b.get_optional_signed(4);
        let uvac_d = b.get_optional_signed(4);

        let n = if self.segments_enabled { MAX_SEGMENTS } else { 1 };
        for i in 0..n {
            let base = if self.segments_enabled {
                if self.segment[i].delta_values {
                    self.segment[i].quantizer_level as i32 + yac_abs
                } else {
                    self.segment[i].quantizer_level as i32
                }
            } else {
                yac_abs
            };
            let s = &mut self.segment[i];
            s.ydc = dcq(base + ydc_d);
            s.yac = acq(base);
            s.y2dc = dcq(base + y2dc_d) * 2;
            s.y2ac = ((acq(base + y2ac_d) as i32) * 155 / 100) as i16;
            if s.y2ac < 8 {
                s.y2ac = 8;
            }
            s.uvdc = dcq(base + uvdc_d);
            if s.uvdc > 132 {
                s.uvdc = 132;
            }
            s.uvac = acq(base + uvac_d);
        }
    }

    fn read_mb_header(&mut self, b: &mut BoolDec, mbx: usize) -> MacroBlock {
        let mut mb = MacroBlock::default();

        if self.segments_enabled && self.segments_update_map {
            mb.segmentid = b.read_tree(&SEGMENT_TREE, &self.segment_probs, 0) as u8;
        }
        mb.coeffs_skipped = match self.prob_skip_false {
            Some(prob) => b.get_bit(prob) != 0,
            None => false,
        };

        let luma = b.read_tree(&YMODE_TREE, &YMODE_PROBS, 0);
        mb.luma_mode = luma;
        if luma == B_PRED {
            for y in 0..4 {
                for x in 0..4 {
                    let a = self.top[mbx].bpred[12 + x];
                    let l = self.left.bpred[y];
                    let probs = &KEYFRAME_BPRED_MODE_PROBS[a as usize][l as usize];
                    let bmode = b.read_tree(&BPRED_TREE, probs, 0);
                    mb.bpred[x + y * 4] = bmode;
                    self.top[mbx].bpred[12 + x] = bmode;
                    self.left.bpred[y] = bmode;
                }
            }
        } else {
            let intra = luma_to_bmode(luma);
            for i in 0..4 {
                mb.bpred[12 + i] = intra;
                self.left.bpred[i] = intra;
            }
        }
        mb.chroma_mode = b.read_tree(&UV_MODE_TREE, &UV_MODE_PROBS, 0);

        self.top[mbx].luma_mode = mb.luma_mode;
        self.top[mbx].chroma_mode = mb.chroma_mode;
        self.top[mbx].bpred = mb.bpred;
        mb
    }

    fn read_residual(&mut self, tok: &mut BoolDec, mb: &mut MacroBlock, mbx: usize) -> [i32; 384] {
        let s = self.segment[mb.segmentid as usize];
        let mut blocks = [0i32; 384];
        let mut plane = if mb.luma_mode == B_PRED { 3 } else { 1 };

        if plane == 1 {
            let complexity = (self.top[mbx].complexity[0] + self.left.complexity[0]) as usize;
            let mut block = [0i32; 16];
            let n = read_coefficients(tok, &self.token_probs[0][plane], 0, complexity, s.y2dc, s.y2ac, &mut block);
            self.left.complexity[0] = n as u8;
            self.top[mbx].complexity[0] = n as u8;
            iwht4x4(&mut block);
            for k in 0..16 {
                blocks[16 * k] = block[k];
            }
            plane = 0;
        }

        for y in 0..4 {
            let mut left = self.left.complexity[y + 1];
            for x in 0..4 {
                let i = x + y * 4;
                let block: &mut [i32; 16] = (&mut blocks[i * 16..i * 16 + 16]).try_into().unwrap();
                let complexity = (self.top[mbx].complexity[x + 1] + left) as usize;
                let n = read_coefficients(tok, &self.token_probs[0][plane], if plane == 0 { 1 } else { 0 }, complexity, s.ydc, s.yac, block);
                if block[0] != 0 || n {
                    mb.non_zero_dct = true;
                    idct4x4(block);
                }
                left = n as u8;
                self.top[mbx].complexity[x + 1] = n as u8;
            }
            self.left.complexity[y + 1] = left;
        }

        for &j in &[5usize, 7usize] {
            for y in 0..2 {
                let mut left = self.left.complexity[y + j];
                for x in 0..2 {
                    let i = x + y * 2 + if j == 5 { 16 } else { 20 };
                    let block: &mut [i32; 16] = (&mut blocks[i * 16..i * 16 + 16]).try_into().unwrap();
                    let complexity = (self.top[mbx].complexity[x + j] + left) as usize;
                    let n = read_coefficients(tok, &self.token_probs[0][2], 0, complexity, s.uvdc, s.uvac, block);
                    if block[0] != 0 || n {
                        mb.non_zero_dct = true;
                        idct4x4(block);
                    }
                    left = n as u8;
                    self.top[mbx].complexity[x + j] = n as u8;
                }
                self.left.complexity[y + j] = left;
            }
        }

        blocks
    }

    // ── intra prediction ────────────────────────────────────────────────────────

    fn predict_luma(&mut self, mbx: usize, mby: usize, mb: &MacroBlock, res: &[i32]) {
        let stride = 1 + 16 + 4;
        let mw = self.mbw;
        let mut ws = create_border_luma(mbx, mby, mw, &self.top_y, &self.left_y);

        match mb.luma_mode {
            V_PRED => predict_v(&mut ws, 16, 1, 1, stride),
            H_PRED => predict_h(&mut ws, 16, 1, 1, stride),
            TM_PRED => predict_tm(&mut ws, 16, 1, 1, stride),
            DC_PRED => predict_dc(&mut ws, 16, stride, mby != 0, mbx != 0),
            _ => predict_4x4(&mut ws, stride, &mb.bpred, res),
        }

        if mb.luma_mode != B_PRED {
            for y in 0..4 {
                for x in 0..4 {
                    let i = x + y * 4;
                    let rb: &[i32; 16] = res[i * 16..i * 16 + 16].try_into().unwrap();
                    add_residue(&mut ws, rb, 1 + y * 4, 1 + x * 4, stride);
                }
            }
        }

        self.left_y[0] = ws[16];
        for (i, left) in self.left_y[1..1 + 16].iter_mut().enumerate() {
            *left = ws[(i + 1) * stride + 16];
        }
        for (top, &w) in self.top_y[mbx * 16..mbx * 16 + 16].iter_mut().zip(&ws[16 * stride + 1..]) {
            *top = w;
        }

        for y in 0..16 {
            let dst = (mby * 16 + y) * mw * 16 + mbx * 16;
            for (o, &w) in self.ybuf[dst..dst + 16].iter_mut().zip(ws[(1 + y) * stride + 1..].iter()) {
                *o = w;
            }
        }
    }

    fn predict_chroma(&mut self, mbx: usize, mby: usize, mb: &MacroBlock, res: &[i32]) {
        let stride = 1 + 8;
        let mw = self.mbw;
        let mut uws = create_border_chroma(mbx, mby, &self.top_u, &self.left_u);
        let mut vws = create_border_chroma(mbx, mby, &self.top_v, &self.left_v);

        match mb.chroma_mode {
            DC_PRED => {
                predict_dc(&mut uws, 8, stride, mby != 0, mbx != 0);
                predict_dc(&mut vws, 8, stride, mby != 0, mbx != 0);
            }
            V_PRED => {
                predict_v(&mut uws, 8, 1, 1, stride);
                predict_v(&mut vws, 8, 1, 1, stride);
            }
            H_PRED => {
                predict_h(&mut uws, 8, 1, 1, stride);
                predict_h(&mut vws, 8, 1, 1, stride);
            }
            _ => {
                predict_tm(&mut uws, 8, 1, 1, stride);
                predict_tm(&mut vws, 8, 1, 1, stride);
            }
        }

        for y in 0..2 {
            for x in 0..2 {
                let i = x + y * 2;
                let urb: &[i32; 16] = res[16 * 16 + i * 16..16 * 16 + i * 16 + 16].try_into().unwrap();
                add_residue(&mut uws, urb, 1 + y * 4, 1 + x * 4, stride);
                let vrb: &[i32; 16] = res[20 * 16 + i * 16..20 * 16 + i * 16 + 16].try_into().unwrap();
                add_residue(&mut vws, vrb, 1 + y * 4, 1 + x * 4, stride);
            }
        }

        set_chroma_border(&mut self.left_u, &mut self.top_u, &uws, mbx);
        set_chroma_border(&mut self.left_v, &mut self.top_v, &vws, mbx);

        for y in 0..8 {
            let dst = (mby * 8 + y) * mw * 8 + mbx * 8;
            let si = (1 + y) * stride + 1;
            for (((ub, vb), &uw), &vw) in self.ubuf[dst..dst + 8]
                .iter_mut()
                .zip(self.vbuf[dst..dst + 8].iter_mut())
                .zip(uws[si..si + 8].iter())
                .zip(vws[si..si + 8].iter())
            {
                *ub = uw;
                *vb = vw;
            }
        }
    }

    // ── loop filter (RFC 6386 §15) ──────────────────────────────────────────────

    fn filter_params(&self, mb: &MacroBlock) -> (u8, u8, u8) {
        let seg = self.segment[mb.segmentid as usize];
        let mut level = self.filter_level as i32;
        if level == 0 {
            return (0, 0, 0);
        }
        if self.segments_enabled {
            if seg.delta_values {
                level += seg.loopfilter_level as i32;
            } else {
                level = seg.loopfilter_level as i32;
            }
        }
        level = level.clamp(0, 63);
        if self.lf_adjust {
            level += self.ref_delta[0];
            if mb.luma_mode == B_PRED {
                level += self.mode_delta[0];
            }
        }
        let level = level.clamp(0, 63) as u8;

        let mut interior = level;
        if self.sharpness > 0 {
            interior >>= if self.sharpness > 4 { 2 } else { 1 };
            if interior > 9 - self.sharpness {
                interior = 9 - self.sharpness;
            }
        }
        if interior == 0 {
            interior = 1;
        }
        let hev = if level >= 40 {
            2
        } else if level >= 15 {
            1
        } else {
            0
        };
        (level, interior, hev)
    }

    fn loop_filter(&mut self, mbx: usize, mby: usize, mb: &MacroBlock) {
        let (level, interior, hev) = self.filter_params(mb);
        if level == 0 {
            return;
        }
        let lw = self.mbw * 16;
        let cw = self.mbw * 8;
        let mb_edge = (level + 2) * 2 + interior;
        let sub_edge = level * 2 + interior;
        let do_sub = mb.luma_mode == B_PRED || (!mb.coeffs_skipped && mb.non_zero_dct);
        let simple = self.filter_type;

        // Left MB edge.
        if mbx > 0 {
            for y in 0..16 {
                let o = (mby * 16 + y) * lw + mbx * 16;
                if simple {
                    simple_h(mb_edge, &mut self.ybuf[o - 4..o + 4]);
                } else {
                    mb_filter_h(hev, interior, mb_edge, &mut self.ybuf[o - 4..o + 4]);
                }
            }
            if !simple {
                for y in 0..8 {
                    let o = (mby * 8 + y) * cw + mbx * 8;
                    mb_filter_h(hev, interior, mb_edge, &mut self.ubuf[o - 4..o + 4]);
                    mb_filter_h(hev, interior, mb_edge, &mut self.vbuf[o - 4..o + 4]);
                }
            }
        }
        // Internal vertical (subblock) edges.
        if do_sub {
            for x in (4..16).step_by(4) {
                for y in 0..16 {
                    let o = (mby * 16 + y) * lw + mbx * 16 + x;
                    if simple {
                        simple_h(sub_edge, &mut self.ybuf[o - 4..o + 4]);
                    } else {
                        sub_filter_h(hev, interior, sub_edge, &mut self.ybuf[o - 4..o + 4]);
                    }
                }
            }
            if !simple {
                for y in 0..8 {
                    let o = (mby * 8 + y) * cw + mbx * 8 + 4;
                    sub_filter_h(hev, interior, sub_edge, &mut self.ubuf[o - 4..o + 4]);
                    sub_filter_h(hev, interior, sub_edge, &mut self.vbuf[o - 4..o + 4]);
                }
            }
        }
        // Top MB edge.
        if mby > 0 {
            for x in 0..16 {
                let o = (mby * 16) * lw + mbx * 16 + x;
                if simple {
                    simple_v(mb_edge, &mut self.ybuf, o, lw);
                } else {
                    mb_filter_v(hev, interior, mb_edge, &mut self.ybuf, o, lw);
                }
            }
            if !simple {
                for x in 0..8 {
                    let o = (mby * 8) * cw + mbx * 8 + x;
                    mb_filter_v(hev, interior, mb_edge, &mut self.ubuf, o, cw);
                    mb_filter_v(hev, interior, mb_edge, &mut self.vbuf, o, cw);
                }
            }
        }
        // Internal horizontal (subblock) edges.
        if do_sub {
            for y in (4..16).step_by(4) {
                for x in 0..16 {
                    let o = (mby * 16 + y) * lw + mbx * 16 + x;
                    if simple {
                        simple_v(sub_edge, &mut self.ybuf, o, lw);
                    } else {
                        sub_filter_v(hev, interior, sub_edge, &mut self.ybuf, o, lw);
                    }
                }
            }
            if !simple {
                for x in 0..8 {
                    let o = (mby * 8 + 4) * cw + mbx * 8 + x;
                    sub_filter_v(hev, interior, sub_edge, &mut self.ubuf, o, cw);
                    sub_filter_v(hev, interior, sub_edge, &mut self.vbuf, o, cw);
                }
            }
        }
    }

    fn to_rgba(&self) -> Bitmap {
        let mut bmp = Bitmap::new(self.width, self.height);
        let ystride = self.mbw * 16;
        let cstride = self.mbw * 8;
        for y in 0..self.height {
            for x in 0..self.width {
                let yv = self.ybuf[y * ystride + x];
                // Bilinear chroma upsampling (chroma is half resolution).
                let (u, v) = self.sample_chroma(x, y, cstride);
                let o = (y * self.width + x) * 4;
                bmp.data[o] = yuv_r(yv, v);
                bmp.data[o + 1] = yuv_g(yv, u, v);
                bmp.data[o + 2] = yuv_b(yv, u);
                bmp.data[o + 3] = 255;
            }
        }
        bmp
    }

    fn sample_chroma(&self, x: usize, y: usize, cstride: usize) -> (u8, u8) {
        let cw = self.mbw * 8;
        let ch = self.mbh * 8;
        let fx = (x as f32) * 0.5 - 0.25;
        let fy = (y as f32) * 0.5 - 0.25;
        let x0 = fx.floor().max(0.0);
        let y0 = fy.floor().max(0.0);
        let tx = (fx - x0).clamp(0.0, 1.0);
        let ty = (fy - y0).clamp(0.0, 1.0);
        let x0 = (x0 as usize).min(cw - 1);
        let y0 = (y0 as usize).min(ch - 1);
        let x1 = (x0 + 1).min(cw - 1);
        let y1 = (y0 + 1).min(ch - 1);
        let bil = |buf: &[u8]| -> u8 {
            let c00 = buf[y0 * cstride + x0] as f32;
            let c10 = buf[y0 * cstride + x1] as f32;
            let c01 = buf[y1 * cstride + x0] as f32;
            let c11 = buf[y1 * cstride + x1] as f32;
            let top = c00 + (c10 - c00) * tx;
            let bot = c01 + (c11 - c01) * tx;
            (top + (bot - top) * ty).round().clamp(0.0, 255.0) as u8
        };
        (bil(&self.ubuf), bil(&self.vbuf))
    }
}

fn luma_to_bmode(luma: i8) -> i8 {
    match luma {
        DC_PRED => B_DC,
        V_PRED => B_VE,
        H_PRED => B_HE,
        TM_PRED => B_TM,
        _ => B_DC,
    }
}

// ── token decoding ──────────────────────────────────────────────────────────────

fn read_coefficients(
    dec: &mut BoolDec,
    probs: &PlaneProbs,
    first: usize,
    complexity: usize,
    dcq: i16,
    acq: i16,
    block: &mut [i32; 16],
) -> bool {
    let mut ctx = complexity.min(2);
    let mut has = false;
    let mut skip_eob = false;

    let mut i = first;
    while i < 16 {
        let band = COEFF_BANDS[i];
        let tree_probs = &probs[band][ctx];
        // After a zero token, the next read skips the EOB branch (flat index 2).
        let start = if skip_eob { 2 } else { 0 };
        let token = dec.read_tree(&DCT_TOKEN_TREE, tree_probs, start);

        let mut abs_value: i32 = match token {
            DCT_EOB => break,
            DCT_0 => {
                skip_eob = true;
                has = true;
                ctx = 0;
                i += 1;
                continue;
            }
            t @ DCT_1..=DCT_4 => t as i32,
            cat @ DCT_CAT1..=DCT_CAT6 => {
                let cprobs = &PROB_DCT_CAT[(cat - DCT_CAT1) as usize];
                let mut extra = 0i32;
                for &pp in cprobs.iter() {
                    if pp == 0 {
                        break;
                    }
                    extra = extra + extra + dec.get_bit(pp) as i32;
                }
                DCT_CAT_BASE[(cat - DCT_CAT1) as usize] + extra
            }
            _ => break,
        };

        skip_eob = false;
        ctx = if abs_value == 1 { 1 } else { 2 };
        if dec.get_flag() {
            abs_value = -abs_value;
        }
        let zz = ZIGZAG[i];
        block[zz] = abs_value * if zz > 0 { acq as i32 } else { dcq as i32 };
        has = true;
        i += 1;
    }
    has
}

// ── transforms (RFC 6386 §14.3) ─────────────────────────────────────────────────

const C1: i64 = 20091;
const C2: i64 = 35468;

fn idct4x4(b: &mut [i32; 16]) {
    let f = |b: &[i32; 16], i: usize| b[i] as i64;
    let mut t = [0i64; 16];
    for i in 0..4 {
        let a1 = f(b, i) + f(b, 8 + i);
        let b1 = f(b, i) - f(b, 8 + i);
        let c1 = ((f(b, 4 + i) * C2) >> 16) - (f(b, 12 + i) + ((f(b, 12 + i) * C1) >> 16));
        let d1 = (f(b, 4 + i) + ((f(b, 4 + i) * C1) >> 16)) + ((f(b, 12 + i) * C2) >> 16);
        t[i] = a1 + d1;
        t[4 + i] = b1 + c1;
        t[12 + i] = a1 - d1;
        t[8 + i] = b1 - c1;
    }
    for i in 0..4 {
        let a1 = t[4 * i] + t[4 * i + 2];
        let b1 = t[4 * i] - t[4 * i + 2];
        let c1 = ((t[4 * i + 1] * C2) >> 16) - (t[4 * i + 3] + ((t[4 * i + 3] * C1) >> 16));
        let d1 = (t[4 * i + 1] + ((t[4 * i + 1] * C1) >> 16)) + ((t[4 * i + 3] * C2) >> 16);
        b[4 * i] = ((a1 + d1 + 4) >> 3) as i32;
        b[4 * i + 3] = ((a1 - d1 + 4) >> 3) as i32;
        b[4 * i + 1] = ((b1 + c1 + 4) >> 3) as i32;
        b[4 * i + 2] = ((b1 - c1 + 4) >> 3) as i32;
    }
}

fn iwht4x4(b: &mut [i32; 16]) {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let a1 = b[i] + b[12 + i];
        let b1 = b[4 + i] + b[8 + i];
        let c1 = b[4 + i] - b[8 + i];
        let d1 = b[i] - b[12 + i];
        t[i] = a1 + b1;
        t[4 + i] = c1 + d1;
        t[8 + i] = a1 - b1;
        t[12 + i] = d1 - c1;
    }
    for i in 0..4 {
        let a1 = t[4 * i] + t[4 * i + 3];
        let b1 = t[4 * i + 1] + t[4 * i + 2];
        let c1 = t[4 * i + 1] - t[4 * i + 2];
        let d1 = t[4 * i] - t[4 * i + 3];
        b[4 * i] = (a1 + b1 + 3) >> 3;
        b[4 * i + 1] = (c1 + d1 + 3) >> 3;
        b[4 * i + 2] = (a1 - b1 + 3) >> 3;
        b[4 * i + 3] = (d1 - c1 + 3) >> 3;
    }
}

// ── prediction helpers ──────────────────────────────────────────────────────────

fn create_border_luma(mbx: usize, mby: usize, mbw: usize, top: &[u8], left: &[u8]) -> [u8; 357] {
    let stride = 1 + 16 + 4;
    let mut ws = [0u8; 357];
    // Above row (A) + top-right extension.
    if mby == 0 {
        for a in ws[1..stride].iter_mut() {
            *a = 127;
        }
    } else {
        for (a, &t) in ws[1..17].iter_mut().zip(&top[mbx * 16..]) {
            *a = t;
        }
        if mbx == mbw - 1 {
            let last = top[mbx * 16 + 15];
            for a in ws[17..stride].iter_mut() {
                *a = last;
            }
        } else {
            for (a, &t) in ws[17..stride].iter_mut().zip(&top[mbx * 16 + 16..]) {
                *a = t;
            }
        }
    }
    for i in 17..stride {
        ws[4 * stride + i] = ws[i];
        ws[8 * stride + i] = ws[i];
        ws[12 * stride + i] = ws[i];
    }
    // Left column (L).
    if mbx == 0 {
        for i in 0..16 {
            ws[(i + 1) * stride] = 129;
        }
    } else {
        for (i, &l) in (0..16).zip(&left[1..]) {
            ws[(i + 1) * stride] = l;
        }
    }
    // Top-left (P).
    ws[0] = if mby == 0 {
        127
    } else if mbx == 0 {
        129
    } else {
        left[0]
    };
    ws
}

fn create_border_chroma(mbx: usize, mby: usize, top: &[u8], left: &[u8]) -> [u8; 81] {
    let stride = 1 + 8;
    let mut ws = [0u8; 81];
    if mby == 0 {
        for a in ws[1..stride].iter_mut() {
            *a = 127;
        }
    } else {
        for (a, &t) in ws[1..stride].iter_mut().zip(&top[mbx * 8..]) {
            *a = t;
        }
    }
    if mbx == 0 {
        for y in 0..8 {
            ws[(y + 1) * stride] = 129;
        }
    } else {
        for (y, &l) in (0..8).zip(&left[1..]) {
            ws[(y + 1) * stride] = l;
        }
    }
    ws[0] = if mby == 0 {
        127
    } else if mbx == 0 {
        129
    } else {
        left[0]
    };
    ws
}

fn set_chroma_border(left: &mut [u8], top: &mut [u8], ws: &[u8], mbx: usize) {
    let stride = 1 + 8;
    left[0] = ws[8];
    for (i, l) in left[1..1 + 8].iter_mut().enumerate() {
        *l = ws[(i + 1) * stride + 8];
    }
    for (t, &w) in top[mbx * 8..mbx * 8 + 8].iter_mut().zip(&ws[8 * stride + 1..]) {
        *t = w;
    }
}

fn add_residue(p: &mut [u8], rb: &[i32; 16], y0: usize, x0: usize, stride: usize) {
    let mut pos = y0 * stride + x0;
    for row in rb.chunks(4) {
        for (px, &a) in p[pos..pos + 4].iter_mut().zip(row) {
            *px = (a + *px as i32).clamp(0, 255) as u8;
        }
        pos += stride;
    }
}

fn predict_v(a: &mut [u8], size: usize, x0: usize, y0: usize, stride: usize) {
    for y in 0..size {
        for x in 0..size {
            a[(y0 + y) * stride + x0 + x] = a[(y0 - 1) * stride + x0 + x];
        }
    }
}

fn predict_h(a: &mut [u8], size: usize, x0: usize, y0: usize, stride: usize) {
    for y in 0..size {
        let l = a[(y0 + y) * stride + x0 - 1];
        for x in 0..size {
            a[(y0 + y) * stride + x0 + x] = l;
        }
    }
}

fn predict_dc(a: &mut [u8], size: usize, stride: usize, above: bool, left: bool) {
    let mut sum = 0u32;
    let mut shf = if size == 8 { 2 } else { 3 };
    if left {
        for y in 0..size {
            sum += a[(y + 1) * stride] as u32;
        }
        shf += 1;
    }
    if above {
        for x in 0..size {
            sum += a[1 + x] as u32;
        }
        shf += 1;
    }
    let dc = if !left && !above { 128 } else { (sum + (1 << (shf - 1))) >> shf };
    for y in 0..size {
        for x in 0..size {
            a[(y + 1) * stride + 1 + x] = dc as u8;
        }
    }
}

fn predict_tm(a: &mut [u8], size: usize, x0: usize, y0: usize, stride: usize) {
    let p = a[(y0 - 1) * stride + x0 - 1] as i32;
    for y in 0..size {
        let l = a[(y0 + y) * stride + x0 - 1] as i32;
        for x in 0..size {
            let t = a[(y0 - 1) * stride + x0 + x] as i32;
            a[(y0 + y) * stride + x0 + x] = (l + t - p).clamp(0, 255) as u8;
        }
    }
}

fn avg3(x: u8, y: u8, z: u8) -> u8 {
    ((x as u16 + 2 * y as u16 + z as u16 + 2) >> 2) as u8
}
fn avg2(x: u8, y: u8) -> u8 {
    ((x as u16 + y as u16 + 1) >> 1) as u8
}

fn predict_4x4(ws: &mut [u8], stride: usize, modes: &[i8; 16], res: &[i32]) {
    for sby in 0..4 {
        for sbx in 0..4 {
            let i = sbx + sby * 4;
            let y0 = sby * 4 + 1;
            let x0 = sbx * 4 + 1;
            match modes[i] {
                B_TM => predict_tm(ws, 4, x0, y0, stride),
                B_VE => bve(ws, x0, y0, stride),
                B_HE => bhe(ws, x0, y0, stride),
                B_DC => bdc(ws, x0, y0, stride),
                B_LD => bld(ws, x0, y0, stride),
                B_RD => brd(ws, x0, y0, stride),
                B_VR => bvr(ws, x0, y0, stride),
                B_VL => bvl(ws, x0, y0, stride),
                B_HD => bhd(ws, x0, y0, stride),
                _ => bhu(ws, x0, y0, stride),
            }
            let rb: &[i32; 16] = res[i * 16..i * 16 + 16].try_into().unwrap();
            add_residue(ws, rb, y0, x0, stride);
        }
    }
}

fn tl(a: &[u8], x0: usize, y0: usize, s: usize) -> u8 {
    a[(y0 - 1) * s + x0 - 1]
}
fn tops(a: &[u8], x0: usize, y0: usize, s: usize) -> [u8; 8] {
    let p = (y0 - 1) * s + x0;
    let mut o = [0u8; 8];
    o.copy_from_slice(&a[p..p + 8]);
    o
}
fn lefts(a: &[u8], x0: usize, y0: usize, s: usize) -> [u8; 4] {
    [
        a[y0 * s + x0 - 1],
        a[(y0 + 1) * s + x0 - 1],
        a[(y0 + 2) * s + x0 - 1],
        a[(y0 + 3) * s + x0 - 1],
    ]
}
fn edges(a: &[u8], x0: usize, y0: usize, s: usize) -> [u8; 9] {
    let p = (y0 - 1) * s + x0 - 1;
    [
        a[p + 4 * s],
        a[p + 3 * s],
        a[p + 2 * s],
        a[p + s],
        a[p],
        a[p + 1],
        a[p + 2],
        a[p + 3],
        a[p + 4],
    ]
}
fn put(a: &mut [u8], x0: usize, y0: usize, s: usize, dx: usize, dy: usize, v: u8) {
    a[(y0 + dy) * s + x0 + dx] = v;
}

fn bve(a: &mut [u8], x0: usize, y0: usize, s: usize) {
    let p = tl(a, x0, y0, s);
    let t = tops(a, x0, y0, s);
    let avg = [avg3(p, t[0], t[1]), avg3(t[0], t[1], t[2]), avg3(t[1], t[2], t[3]), avg3(t[2], t[3], t[4])];
    for dy in 0..4 {
        for dx in 0..4 {
            put(a, x0, y0, s, dx, dy, avg[dx]);
        }
    }
}
fn bhe(a: &mut [u8], x0: usize, y0: usize, s: usize) {
    let p = tl(a, x0, y0, s);
    let l = lefts(a, x0, y0, s);
    let avg = [avg3(p, l[0], l[1]), avg3(l[0], l[1], l[2]), avg3(l[1], l[2], l[3]), avg3(l[2], l[3], l[3])];
    for dy in 0..4 {
        for dx in 0..4 {
            put(a, x0, y0, s, dx, dy, avg[dy]);
        }
    }
}
fn bdc(a: &mut [u8], x0: usize, y0: usize, s: usize) {
    let mut v = 4u32;
    for dx in 0..4 {
        v += a[(y0 - 1) * s + x0 + dx] as u32;
    }
    for dy in 0..4 {
        v += a[(y0 + dy) * s + x0 - 1] as u32;
    }
    v >>= 3;
    for dy in 0..4 {
        for dx in 0..4 {
            put(a, x0, y0, s, dx, dy, v as u8);
        }
    }
}
fn bld(a: &mut [u8], x0: usize, y0: usize, s: usize) {
    let t = tops(a, x0, y0, s);
    let avg = [
        avg3(t[0], t[1], t[2]),
        avg3(t[1], t[2], t[3]),
        avg3(t[2], t[3], t[4]),
        avg3(t[3], t[4], t[5]),
        avg3(t[4], t[5], t[6]),
        avg3(t[5], t[6], t[7]),
        avg3(t[6], t[7], t[7]),
    ];
    for dy in 0..4 {
        for dx in 0..4 {
            put(a, x0, y0, s, dx, dy, avg[dx + dy]);
        }
    }
}
fn brd(a: &mut [u8], x0: usize, y0: usize, s: usize) {
    let e = edges(a, x0, y0, s);
    let avg = [
        avg3(e[0], e[1], e[2]),
        avg3(e[1], e[2], e[3]),
        avg3(e[2], e[3], e[4]),
        avg3(e[3], e[4], e[5]),
        avg3(e[4], e[5], e[6]),
        avg3(e[5], e[6], e[7]),
        avg3(e[6], e[7], e[8]),
    ];
    for dy in 0..4 {
        for dx in 0..4 {
            put(a, x0, y0, s, dx, dy, avg[3 - dy + dx]);
        }
    }
}
fn bvr(a: &mut [u8], x0: usize, y0: usize, s: usize) {
    let e = edges(a, x0, y0, s);
    put(a, x0, y0, s, 0, 3, avg3(e[1], e[2], e[3]));
    put(a, x0, y0, s, 0, 2, avg3(e[2], e[3], e[4]));
    put(a, x0, y0, s, 1, 3, avg3(e[3], e[4], e[5]));
    put(a, x0, y0, s, 0, 1, avg3(e[3], e[4], e[5]));
    put(a, x0, y0, s, 1, 2, avg2(e[4], e[5]));
    put(a, x0, y0, s, 0, 0, avg2(e[4], e[5]));
    put(a, x0, y0, s, 2, 3, avg3(e[4], e[5], e[6]));
    put(a, x0, y0, s, 1, 1, avg3(e[4], e[5], e[6]));
    put(a, x0, y0, s, 2, 2, avg2(e[5], e[6]));
    put(a, x0, y0, s, 1, 0, avg2(e[5], e[6]));
    put(a, x0, y0, s, 3, 3, avg3(e[5], e[6], e[7]));
    put(a, x0, y0, s, 2, 1, avg3(e[5], e[6], e[7]));
    put(a, x0, y0, s, 3, 2, avg2(e[6], e[7]));
    put(a, x0, y0, s, 2, 0, avg2(e[6], e[7]));
    put(a, x0, y0, s, 3, 1, avg3(e[6], e[7], e[8]));
    put(a, x0, y0, s, 3, 0, avg2(e[7], e[8]));
}
fn bvl(a: &mut [u8], x0: usize, y0: usize, s: usize) {
    let t = tops(a, x0, y0, s);
    put(a, x0, y0, s, 0, 0, avg2(t[0], t[1]));
    put(a, x0, y0, s, 0, 1, avg3(t[0], t[1], t[2]));
    put(a, x0, y0, s, 0, 2, avg2(t[1], t[2]));
    put(a, x0, y0, s, 1, 0, avg2(t[1], t[2]));
    put(a, x0, y0, s, 1, 1, avg3(t[1], t[2], t[3]));
    put(a, x0, y0, s, 0, 3, avg3(t[1], t[2], t[3]));
    put(a, x0, y0, s, 1, 2, avg2(t[2], t[3]));
    put(a, x0, y0, s, 2, 0, avg2(t[2], t[3]));
    put(a, x0, y0, s, 1, 3, avg3(t[2], t[3], t[4]));
    put(a, x0, y0, s, 2, 1, avg3(t[2], t[3], t[4]));
    put(a, x0, y0, s, 2, 2, avg2(t[3], t[4]));
    put(a, x0, y0, s, 3, 0, avg2(t[3], t[4]));
    put(a, x0, y0, s, 2, 3, avg3(t[3], t[4], t[5]));
    put(a, x0, y0, s, 3, 1, avg3(t[3], t[4], t[5]));
    put(a, x0, y0, s, 3, 2, avg3(t[4], t[5], t[6]));
    put(a, x0, y0, s, 3, 3, avg3(t[5], t[6], t[7]));
}
fn bhd(a: &mut [u8], x0: usize, y0: usize, s: usize) {
    let e = edges(a, x0, y0, s);
    put(a, x0, y0, s, 0, 3, avg2(e[0], e[1]));
    put(a, x0, y0, s, 1, 3, avg3(e[0], e[1], e[2]));
    put(a, x0, y0, s, 0, 2, avg2(e[1], e[2]));
    put(a, x0, y0, s, 2, 3, avg2(e[1], e[2]));
    put(a, x0, y0, s, 1, 2, avg3(e[1], e[2], e[3]));
    put(a, x0, y0, s, 3, 3, avg3(e[1], e[2], e[3]));
    put(a, x0, y0, s, 2, 2, avg2(e[2], e[3]));
    put(a, x0, y0, s, 0, 1, avg2(e[2], e[3]));
    put(a, x0, y0, s, 3, 2, avg3(e[2], e[3], e[4]));
    put(a, x0, y0, s, 1, 1, avg3(e[2], e[3], e[4]));
    put(a, x0, y0, s, 2, 1, avg2(e[3], e[4]));
    put(a, x0, y0, s, 0, 0, avg2(e[3], e[4]));
    put(a, x0, y0, s, 3, 1, avg3(e[3], e[4], e[5]));
    put(a, x0, y0, s, 1, 0, avg3(e[3], e[4], e[5]));
    put(a, x0, y0, s, 2, 0, avg3(e[4], e[5], e[6]));
    put(a, x0, y0, s, 3, 0, avg3(e[5], e[6], e[7]));
}
fn bhu(a: &mut [u8], x0: usize, y0: usize, s: usize) {
    let l = lefts(a, x0, y0, s);
    put(a, x0, y0, s, 0, 0, avg2(l[0], l[1]));
    put(a, x0, y0, s, 1, 0, avg3(l[0], l[1], l[2]));
    put(a, x0, y0, s, 2, 0, avg2(l[1], l[2]));
    put(a, x0, y0, s, 0, 1, avg2(l[1], l[2]));
    put(a, x0, y0, s, 3, 0, avg3(l[1], l[2], l[3]));
    put(a, x0, y0, s, 1, 1, avg3(l[1], l[2], l[3]));
    put(a, x0, y0, s, 2, 1, avg2(l[2], l[3]));
    put(a, x0, y0, s, 0, 2, avg2(l[2], l[3]));
    put(a, x0, y0, s, 3, 1, avg3(l[2], l[3], l[3]));
    put(a, x0, y0, s, 1, 2, avg3(l[2], l[3], l[3]));
    put(a, x0, y0, s, 2, 2, l[3]);
    put(a, x0, y0, s, 3, 2, l[3]);
    put(a, x0, y0, s, 0, 3, l[3]);
    put(a, x0, y0, s, 1, 3, l[3]);
    put(a, x0, y0, s, 2, 3, l[3]);
    put(a, x0, y0, s, 3, 3, l[3]);
}

// ── loop filter primitives (RFC 6386 §15.2-15.3) ────────────────────────────────

fn c128(v: i32) -> i32 {
    v.clamp(-128, 127)
}
fn u2s(v: u8) -> i32 {
    v as i32 - 128
}
fn s2u(v: i32) -> u8 {
    (c128(v) + 128) as u8
}
fn adiff(a: u8, b: u8) -> u8 {
    a.abs_diff(b)
}

/// Horizontal edge: `px` holds 8 pixels [p3 p2 p1 p0 q0 q1 q2 q3].
fn common_adjust_h(outer: bool, px: &mut [u8]) -> i32 {
    let p1 = u2s(px[2]);
    let p0 = u2s(px[3]);
    let q0 = u2s(px[4]);
    let q1 = u2s(px[5]);
    let o = if outer { c128(p1 - q1) } else { 0 };
    let a = c128(o + 3 * (q0 - p0));
    let b = c128(a + 3) >> 3;
    let a = c128(a + 4) >> 3;
    px[4] = s2u(q0 - a);
    px[3] = s2u(p0 + b);
    a
}

fn common_adjust_v(outer: bool, buf: &mut [u8], pt: usize, st: usize) -> i32 {
    let p1 = u2s(buf[pt - 2 * st]);
    let p0 = u2s(buf[pt - st]);
    let q0 = u2s(buf[pt]);
    let q1 = u2s(buf[pt + st]);
    let o = if outer { c128(p1 - q1) } else { 0 };
    let a = c128(o + 3 * (q0 - p0));
    let b = c128(a + 3) >> 3;
    let a = c128(a + 4) >> 3;
    buf[pt] = s2u(q0 - a);
    buf[pt - st] = s2u(p0 + b);
    a
}

fn simple_thresh_h(limit: i32, px: &[u8]) -> bool {
    adiff(px[3], px[4]) as i32 * 2 + adiff(px[2], px[5]) as i32 / 2 <= limit
}
fn simple_thresh_v(limit: i32, buf: &[u8], pt: usize, st: usize) -> bool {
    adiff(buf[pt - st], buf[pt]) as i32 * 2 + adiff(buf[pt - 2 * st], buf[pt + st]) as i32 / 2 <= limit
}

fn should_filter_h(interior: u8, edge: u8, px: &[u8]) -> bool {
    simple_thresh_h(edge as i32, px)
        && adiff(px[0], px[1]) <= interior
        && adiff(px[1], px[2]) <= interior
        && adiff(px[2], px[3]) <= interior
        && adiff(px[7], px[6]) <= interior
        && adiff(px[6], px[5]) <= interior
        && adiff(px[5], px[4]) <= interior
}
fn should_filter_v(interior: u8, edge: u8, buf: &[u8], pt: usize, st: usize) -> bool {
    simple_thresh_v(edge as i32, buf, pt, st)
        && adiff(buf[pt - 4 * st], buf[pt - 3 * st]) <= interior
        && adiff(buf[pt - 3 * st], buf[pt - 2 * st]) <= interior
        && adiff(buf[pt - 2 * st], buf[pt - st]) <= interior
        && adiff(buf[pt + 3 * st], buf[pt + 2 * st]) <= interior
        && adiff(buf[pt + 2 * st], buf[pt + st]) <= interior
        && adiff(buf[pt + st], buf[pt]) <= interior
}
fn hev_h(th: u8, px: &[u8]) -> bool {
    adiff(px[2], px[3]) > th || adiff(px[5], px[4]) > th
}
fn hev_v(th: u8, buf: &[u8], pt: usize, st: usize) -> bool {
    adiff(buf[pt - 2 * st], buf[pt - st]) > th || adiff(buf[pt + st], buf[pt]) > th
}

fn simple_h(edge: u8, px: &mut [u8]) {
    if simple_thresh_h(edge as i32, px) {
        common_adjust_h(true, px);
    }
}
fn simple_v(edge: u8, buf: &mut [u8], pt: usize, st: usize) {
    if simple_thresh_v(edge as i32, buf, pt, st) {
        common_adjust_v(true, buf, pt, st);
    }
}

fn sub_filter_h(hev: u8, interior: u8, edge: u8, px: &mut [u8]) {
    if should_filter_h(interior, edge, px) {
        let hv = hev_h(hev, px);
        let a = (common_adjust_h(hv, px) + 1) >> 1;
        if !hv {
            px[5] = s2u(u2s(px[5]) - a);
            px[2] = s2u(u2s(px[2]) + a);
        }
    }
}
fn sub_filter_v(hev: u8, interior: u8, edge: u8, buf: &mut [u8], pt: usize, st: usize) {
    if should_filter_v(interior, edge, buf, pt, st) {
        let hv = hev_v(hev, buf, pt, st);
        let a = (common_adjust_v(hv, buf, pt, st) + 1) >> 1;
        if !hv {
            buf[pt + st] = s2u(u2s(buf[pt + st]) - a);
            buf[pt - 2 * st] = s2u(u2s(buf[pt - 2 * st]) + a);
        }
    }
}

fn mb_filter_h(hev: u8, interior: u8, edge: u8, px: &mut [u8]) {
    if should_filter_h(interior, edge, px) {
        if !hev_h(hev, px) {
            let p2 = u2s(px[1]);
            let p1 = u2s(px[2]);
            let p0 = u2s(px[3]);
            let q0 = u2s(px[4]);
            let q1 = u2s(px[5]);
            let q2 = u2s(px[6]);
            let w = c128(c128(p1 - q1) + 3 * (q0 - p0));
            let a = c128((27 * w + 63) >> 7);
            px[4] = s2u(q0 - a);
            px[3] = s2u(p0 + a);
            let a = c128((18 * w + 63) >> 7);
            px[5] = s2u(q1 - a);
            px[2] = s2u(p1 + a);
            let a = c128((9 * w + 63) >> 7);
            px[6] = s2u(q2 - a);
            px[1] = s2u(p2 + a);
        } else {
            common_adjust_h(true, px);
        }
    }
}
fn mb_filter_v(hev: u8, interior: u8, edge: u8, buf: &mut [u8], pt: usize, st: usize) {
    if should_filter_v(interior, edge, buf, pt, st) {
        if !hev_v(hev, buf, pt, st) {
            let p2 = u2s(buf[pt - 3 * st]);
            let p1 = u2s(buf[pt - 2 * st]);
            let p0 = u2s(buf[pt - st]);
            let q0 = u2s(buf[pt]);
            let q1 = u2s(buf[pt + st]);
            let q2 = u2s(buf[pt + 2 * st]);
            let w = c128(c128(p1 - q1) + 3 * (q0 - p0));
            let a = c128((27 * w + 63) >> 7);
            buf[pt] = s2u(q0 - a);
            buf[pt - st] = s2u(p0 + a);
            let a = c128((18 * w + 63) >> 7);
            buf[pt + st] = s2u(q1 - a);
            buf[pt - 2 * st] = s2u(p1 + a);
            let a = c128((9 * w + 63) >> 7);
            buf[pt + 2 * st] = s2u(q2 - a);
            buf[pt - 3 * st] = s2u(p2 + a);
        } else {
            common_adjust_v(true, buf, pt, st);
        }
    }
}

// ── YUV -> RGB (libwebp fixed-point, RFC 6386 §16) ──────────────────────────────

fn mulhi(v: u8, coeff: i32) -> i32 {
    ((v as i32) * coeff) >> 8
}
fn yclip(v: i32) -> u8 {
    (v >> 6).clamp(0, 255) as u8
}
fn yuv_r(y: u8, v: u8) -> u8 {
    yclip(mulhi(y, 19077) + mulhi(v, 26149) - 14234)
}
fn yuv_g(y: u8, u: u8, v: u8) -> u8 {
    yclip(mulhi(y, 19077) - mulhi(u, 6419) - mulhi(v, 13320) + 8708)
}
fn yuv_b(y: u8, u: u8) -> u8 {
    yclip(mulhi(y, 19077) + mulhi(u, 33050) - 17685)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bool_decoder_hello() {
        // Reference test vector from the VP8 bitstream spec / image-webp.
        let mut b = BoolDec::new(b"hel");
        assert_eq!(b.get_flag(), false);
        assert_eq!(b.get_bit(10), 1);
        assert_eq!(b.get_bit(250), 0);
        assert_eq!(b.get_literal(1), 1);
        assert_eq!(b.get_literal(3), 5);
        assert_eq!(b.get_literal(8), 64);
        assert_eq!(b.get_literal(8), 185);
    }
}
