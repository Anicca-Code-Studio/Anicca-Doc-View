//! From-scratch 2D rasterizer for the PDF renderer.
//!
//! Everything the PDF imaging model needs that `render.rs` cannot do:
//! arbitrary affine transforms, cubic Béziers, antialiased nonzero/even-odd
//! filling, stroking with caps/joins/dashes, clip masks, and transformed image
//! blits.
//!
//! Antialiasing uses `SUB_SAMPLES` sub-scanlines per pixel row with *analytic*
//! horizontal coverage: exact fractional coverage along x, box-filtered along
//! y. Edges are swept with an active-edge list so cost scales with crossings,
//! not with canvas height times edge count.

use std::rc::Rc;

/// Vertical sub-samples per pixel row.
const SUB_SAMPLES: usize = 4;

/// Maximum recursion when flattening a cubic.
const MAX_FLATTEN_DEPTH: u8 = 16;

/// Device-space flatness tolerance, in pixels.
const FLATNESS: f64 = 0.15;

// ── transform ────────────────────────────────────────────────────────────────

/// Affine transform in PDF matrix order: `[a b c d e f]` maps
/// `(x, y) -> (a·x + c·y + e, b·x + d·y + f)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Default for Transform {
    fn default() -> Self {
        Transform::identity()
    }
}

impl Transform {
    pub fn identity() -> Transform {
        Transform { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 }
    }

    pub fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Transform {
        Transform { a, b, c, d, e, f }
    }

    pub fn translate(tx: f64, ty: f64) -> Transform {
        Transform { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: tx, f: ty }
    }

    pub fn scale(sx: f64, sy: f64) -> Transform {
        Transform { a: sx, b: 0.0, c: 0.0, d: sy, e: 0.0, f: 0.0 }
    }

    /// `self` applied first, then `m` (i.e. `self × m` in PDF row-vector order).
    pub fn then(&self, m: &Transform) -> Transform {
        Transform {
            a: self.a * m.a + self.b * m.c,
            b: self.a * m.b + self.b * m.d,
            c: self.c * m.a + self.d * m.c,
            d: self.c * m.b + self.d * m.d,
            e: self.e * m.a + self.f * m.c + m.e,
            f: self.e * m.b + self.f * m.d + m.f,
        }
    }

    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (self.a * x + self.c * y + self.e, self.b * x + self.d * y + self.f)
    }

    /// Transforms a direction, ignoring translation.
    pub fn apply_vec(&self, x: f64, y: f64) -> (f64, f64) {
        (self.a * x + self.c * y, self.b * x + self.d * y)
    }

    pub fn det(&self) -> f64 {
        self.a * self.d - self.b * self.c
    }

    pub fn invert(&self) -> Option<Transform> {
        let det = self.det();
        if det.abs() < 1e-12 {
            return None;
        }
        let inv = 1.0 / det;
        Some(Transform {
            a: self.d * inv,
            b: -self.b * inv,
            c: -self.c * inv,
            d: self.a * inv,
            e: (self.c * self.f - self.d * self.e) * inv,
            f: (self.b * self.e - self.a * self.f) * inv,
        })
    }

    /// Mean scale factor, used to pick flattening steps and hairline widths.
    pub fn mean_scale(&self) -> f64 {
        let sx = (self.a * self.a + self.b * self.b).sqrt();
        let sy = (self.c * self.c + self.d * self.d).sqrt();
        ((sx * sy).abs()).sqrt().max(1e-6)
    }
}

// ── path ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    MoveTo(f64, f64),
    LineTo(f64, f64),
    /// Cubic Bézier: two control points then the end point.
    CurveTo(f64, f64, f64, f64, f64, f64),
    Close,
}

#[derive(Clone, Debug, Default)]
pub struct Path {
    pub segs: Vec<Seg>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

impl Path {
    pub fn new() -> Path {
        Path { segs: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }

    pub fn move_to(&mut self, x: f64, y: f64) {
        self.segs.push(Seg::MoveTo(x, y));
    }

    pub fn line_to(&mut self, x: f64, y: f64) {
        self.segs.push(Seg::LineTo(x, y));
    }

    pub fn curve_to(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, x3: f64, y3: f64) {
        self.segs.push(Seg::CurveTo(x1, y1, x2, y2, x3, y3));
    }

    pub fn close(&mut self) {
        self.segs.push(Seg::Close);
    }

    pub fn rect(&mut self, x: f64, y: f64, w: f64, h: f64) {
        self.move_to(x, y);
        self.line_to(x + w, y);
        self.line_to(x + w, y + h);
        self.line_to(x, y + h);
        self.close();
    }

    /// Appends a circle approximated by four cubic arcs.
    pub fn circle(&mut self, cx: f64, cy: f64, r: f64) {
        // Control-point distance for a quarter-circle Bézier.
        const K: f64 = 0.552_284_749_83;
        let k = r * K;
        self.move_to(cx + r, cy);
        self.curve_to(cx + r, cy + k, cx + k, cy + r, cx, cy + r);
        self.curve_to(cx - k, cy + r, cx - r, cy + k, cx - r, cy);
        self.curve_to(cx - r, cy - k, cx - k, cy - r, cx, cy - r);
        self.curve_to(cx + k, cy - r, cx + r, cy - k, cx + r, cy);
        self.close();
    }

    pub fn transformed(&self, m: &Transform) -> Path {
        let segs = self
            .segs
            .iter()
            .map(|s| match *s {
                Seg::MoveTo(x, y) => {
                    let (x, y) = m.apply(x, y);
                    Seg::MoveTo(x, y)
                }
                Seg::LineTo(x, y) => {
                    let (x, y) = m.apply(x, y);
                    Seg::LineTo(x, y)
                }
                Seg::CurveTo(x1, y1, x2, y2, x3, y3) => {
                    let (x1, y1) = m.apply(x1, y1);
                    let (x2, y2) = m.apply(x2, y2);
                    let (x3, y3) = m.apply(x3, y3);
                    Seg::CurveTo(x1, y1, x2, y2, x3, y3)
                }
                Seg::Close => Seg::Close,
            })
            .collect();
        Path { segs }
    }

    /// Flattens into polylines (one per subpath). `closed_only` forces every
    /// subpath closed, as filling requires.
    pub fn flatten(&self, closed_only: bool) -> Vec<Polyline> {
        let mut out: Vec<Polyline> = Vec::new();
        let mut cur: Vec<(f64, f64)> = Vec::new();
        let mut closed = false;
        let mut start = (0.0f64, 0.0f64);
        let mut pen = (0.0f64, 0.0f64);

        macro_rules! flush {
            () => {
                if cur.len() > 1 {
                    out.push(Polyline { pts: std::mem::take(&mut cur), closed: closed || closed_only });
                } else {
                    cur.clear();
                }
                closed = false;
            };
        }

        for seg in &self.segs {
            match *seg {
                Seg::MoveTo(x, y) => {
                    flush!();
                    start = (x, y);
                    pen = (x, y);
                    cur.push((x, y));
                }
                Seg::LineTo(x, y) => {
                    if cur.is_empty() {
                        cur.push(pen);
                    }
                    cur.push((x, y));
                    pen = (x, y);
                }
                Seg::CurveTo(x1, y1, x2, y2, x3, y3) => {
                    if cur.is_empty() {
                        cur.push(pen);
                    }
                    flatten_cubic(pen, (x1, y1), (x2, y2), (x3, y3), 0, &mut cur);
                    pen = (x3, y3);
                }
                Seg::Close => {
                    if !cur.is_empty() {
                        closed = true;
                        flush!();
                    }
                    pen = start;
                }
            }
        }
        flush!();
        out
    }

    /// Returns the rectangle when this path is a single axis-aligned
    /// rectangle, which is the overwhelmingly common clip shape.
    pub fn as_axis_rect(&self) -> Option<(f64, f64, f64, f64)> {
        let mut pts: Vec<(f64, f64)> = Vec::with_capacity(5);
        let mut closed = false;
        for seg in &self.segs {
            match *seg {
                Seg::MoveTo(x, y) => {
                    if !pts.is_empty() {
                        return None; // more than one subpath
                    }
                    pts.push((x, y));
                }
                Seg::LineTo(x, y) => {
                    if pts.len() >= 5 {
                        return None;
                    }
                    pts.push((x, y));
                }
                Seg::CurveTo(..) => return None,
                Seg::Close => closed = true,
            }
        }
        // A rectangle is 4 corners, optionally repeating the first.
        if pts.len() == 5 {
            if (pts[4].0 - pts[0].0).abs() > 1e-9 || (pts[4].1 - pts[0].1).abs() > 1e-9 {
                return None;
            }
            pts.pop();
        }
        if pts.len() != 4 || !closed {
            return None;
        }
        // Corners must alternate between horizontal and vertical edges.
        for i in 0..4 {
            let a = pts[i];
            let b = pts[(i + 1) % 4];
            let horizontal = (a.1 - b.1).abs() < 1e-9;
            let vertical = (a.0 - b.0).abs() < 1e-9;
            if !horizontal && !vertical {
                return None;
            }
            if horizontal == vertical {
                return None; // degenerate edge
            }
        }
        let xs = [pts[0].0, pts[1].0, pts[2].0, pts[3].0];
        let ys = [pts[0].1, pts[1].1, pts[2].1, pts[3].1];
        let x0 = xs.iter().copied().fold(f64::INFINITY, f64::min);
        let x1 = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let y0 = ys.iter().copied().fold(f64::INFINITY, f64::min);
        let y1 = ys.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        Some((x0, y0, x1, y1))
    }

    /// Device-space bounding box of the control points (a conservative bound
    /// for the curve itself).
    pub fn bounds(&self) -> Option<(f64, f64, f64, f64)> {
        let mut b: Option<(f64, f64, f64, f64)> = None;
        let mut add = |x: f64, y: f64| {
            b = Some(match b {
                None => (x, y, x, y),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
            });
        };
        for seg in &self.segs {
            match *seg {
                Seg::MoveTo(x, y) | Seg::LineTo(x, y) => add(x, y),
                Seg::CurveTo(x1, y1, x2, y2, x3, y3) => {
                    add(x1, y1);
                    add(x2, y2);
                    add(x3, y3);
                }
                Seg::Close => {}
            }
        }
        b
    }
}

#[derive(Clone, Debug)]
pub struct Polyline {
    pub pts: Vec<(f64, f64)>,
    pub closed: bool,
}

/// Recursive de Casteljau subdivision, stopping when the control polygon is
/// flat enough that a straight line is within `FLATNESS` of the curve.
fn flatten_cubic(
    p0: (f64, f64),
    p1: (f64, f64),
    p2: (f64, f64),
    p3: (f64, f64),
    depth: u8,
    out: &mut Vec<(f64, f64)>,
) {
    if depth >= MAX_FLATTEN_DEPTH || is_flat(p0, p1, p2, p3) {
        out.push(p3);
        return;
    }
    let mid = |a: (f64, f64), b: (f64, f64)| ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5);
    let p01 = mid(p0, p1);
    let p12 = mid(p1, p2);
    let p23 = mid(p2, p3);
    let p012 = mid(p01, p12);
    let p123 = mid(p12, p23);
    let p0123 = mid(p012, p123);
    flatten_cubic(p0, p01, p012, p0123, depth + 1, out);
    flatten_cubic(p0123, p123, p23, p3, depth + 1, out);
}

fn is_flat(p0: (f64, f64), p1: (f64, f64), p2: (f64, f64), p3: (f64, f64)) -> bool {
    // Distance of each control point from the chord, compared squared to avoid
    // a square root. Degenerate chords fall back to a control-polygon extent.
    let dx = p3.0 - p0.0;
    let dy = p3.1 - p0.1;
    let len2 = dx * dx + dy * dy;
    if len2 < 1e-12 {
        let d1 = (p1.0 - p0.0).abs() + (p1.1 - p0.1).abs();
        let d2 = (p2.0 - p0.0).abs() + (p2.1 - p0.1).abs();
        return d1 + d2 < FLATNESS;
    }
    let c1 = (p1.0 - p0.0) * dy - (p1.1 - p0.1) * dx;
    let c2 = (p2.0 - p0.0) * dy - (p2.1 - p0.1) * dx;
    let tol2 = FLATNESS * FLATNESS * len2;
    c1 * c1 <= tol2 && c2 * c2 <= tol2
}

// ── clip mask ────────────────────────────────────────────────────────────────

/// Per-pixel clip coverage. `None` inside a `Clip` means "everything visible".
#[derive(Clone)]
pub struct Clip {
    pub w: usize,
    pub h: usize,
    /// 0..=255 coverage, or `None` for an unclipped canvas.
    mask: Option<Rc<Vec<u8>>>,
    /// Tight bounds of the non-zero region: (x0, y0, x1, y1), exclusive on the
    /// far edge. Lets fills skip whole rows cheaply.
    pub bounds: (usize, usize, usize, usize),
}

impl Clip {
    pub fn full(w: usize, h: usize) -> Clip {
        Clip { w, h, mask: None, bounds: (0, 0, w, h) }
    }

    pub fn is_full(&self) -> bool {
        self.mask.is_none()
    }

    #[inline]
    pub fn at(&self, x: usize, y: usize) -> u8 {
        match &self.mask {
            None => 255,
            Some(m) => m.get(y * self.w + x).copied().unwrap_or(0),
        }
    }

    /// Intersects with the coverage of `path`, producing a new clip.
    pub fn intersect_path(&self, path: &Path, rule: FillRule) -> Clip {
        // A rectangle that already contains the visible region changes nothing.
        // Form XObjects clip to their BBox constantly, and most of those cover
        // the whole page, so skipping the mask allocation matters.
        if let Some((x0, y0, x1, y1)) = path.as_axis_rect() {
            let (bx0, by0, bx1, by1) = self.bounds;
            if x0 <= bx0 as f64 + 0.001
                && y0 <= by0 as f64 + 0.001
                && x1 >= bx1 as f64 - 0.001
                && y1 >= by1 as f64 - 0.001
            {
                return self.clone();
            }
        }
        let mut cov = vec![0u8; self.w * self.h];
        rasterize(path, rule, self.w, self.h, |x, y, a| {
            let a = (a * 255.0 + 0.5) as i32;
            let a = a.clamp(0, 255) as u8;
            cov[y * self.w + x] = a;
        });
        match &self.mask {
            None => {}
            Some(m) => {
                for (i, c) in cov.iter_mut().enumerate() {
                    *c = mul255(*c, m[i]);
                }
            }
        }
        let bounds = mask_bounds(&cov, self.w, self.h);
        Clip { w: self.w, h: self.h, mask: Some(Rc::new(cov)), bounds }
    }

    /// Intersects with an axis-aligned rectangle in device space.
    pub fn intersect_rect(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> Clip {
        let mut p = Path::new();
        p.rect(x0, y0, x1 - x0, y1 - y0);
        self.intersect_path(&p, FillRule::NonZero)
    }
}

fn mask_bounds(mask: &[u8], w: usize, h: usize) -> (usize, usize, usize, usize) {
    let mut x0 = w;
    let mut y0 = h;
    let mut x1 = 0usize;
    let mut y1 = 0usize;
    for y in 0..h {
        let row = &mask[y * w..(y + 1) * w];
        let mut any = false;
        for (x, &v) in row.iter().enumerate() {
            if v != 0 {
                any = true;
                if x < x0 {
                    x0 = x;
                }
                if x + 1 > x1 {
                    x1 = x + 1;
                }
            }
        }
        if any {
            if y < y0 {
                y0 = y;
            }
            y1 = y + 1;
        }
    }
    if x0 >= x1 || y0 >= y1 {
        (0, 0, 0, 0)
    } else {
        (x0, y0, x1, y1)
    }
}

#[inline]
fn mul255(a: u8, b: u8) -> u8 {
    let t = a as u32 * b as u32 + 128;
    ((t + (t >> 8)) >> 8) as u8
}

// ── paint ────────────────────────────────────────────────────────────────────

/// Supplies colour per device pixel. Shadings and tiling patterns implement
/// this; solid fills use `Paint::Solid`.
pub trait PaintSource {
    /// Colour and alpha at a device-space pixel centre, or `None` where the
    /// source paints nothing.
    fn color_at(&self, x: f64, y: f64) -> Option<([u8; 3], f32)>;
}

pub enum Paint<'a> {
    Solid([u8; 3]),
    Source(&'a dyn PaintSource),
}

// ── canvas ───────────────────────────────────────────────────────────────────

/// RGBA8 canvas, top-left origin, matching what the worker hands to
/// `createImageBitmap`.
pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Canvas {
        Canvas { w, h, data: vec![255u8; w * h * 4] }
    }

    pub fn clear(&mut self, color: [u8; 3]) {
        for px in self.data.chunks_exact_mut(4) {
            px[0] = color[0];
            px[1] = color[1];
            px[2] = color[2];
            px[3] = 255;
        }
    }

    #[inline]
    pub fn blend(&mut self, x: usize, y: usize, color: [u8; 3], alpha: f32) {
        if alpha <= 0.0 || x >= self.w || y >= self.h {
            return;
        }
        let a = alpha.min(1.0);
        let idx = (y * self.w + x) * 4;
        if a >= 1.0 {
            self.data[idx] = color[0];
            self.data[idx + 1] = color[1];
            self.data[idx + 2] = color[2];
            self.data[idx + 3] = 255;
            return;
        }
        let inv = 1.0 - a;
        for i in 0..3 {
            let dst = self.data[idx + i] as f32;
            self.data[idx + i] = (color[i] as f32 * a + dst * inv + 0.5).min(255.0) as u8;
        }
        self.data[idx + 3] = 255;
    }

    /// Fills `path` (already in device space).
    pub fn fill_path(
        &mut self,
        path: &Path,
        rule: FillRule,
        paint: &Paint,
        clip: &Clip,
        alpha: f32,
    ) {
        if alpha <= 0.0 || path.is_empty() {
            return;
        }
        let (cx0, cy0, cx1, cy1) = clip.bounds;
        if cx0 >= cx1 || cy0 >= cy1 {
            return;
        }
        let w = self.w;
        let h = self.h;
        let solid = match paint {
            Paint::Solid(c) => Some(*c),
            Paint::Source(_) => None,
        };
        rasterize(path, rule, w, h, |x, y, cov| {
            if x < cx0 || x >= cx1 || y < cy0 || y >= cy1 {
                return;
            }
            let clip_a = clip.at(x, y);
            if clip_a == 0 {
                return;
            }
            let a = cov * alpha * (clip_a as f32 / 255.0);
            match solid {
                Some(c) => self.blend(x, y, c, a),
                None => {
                    if let Paint::Source(src) = paint {
                        if let Some((c, sa)) = src.color_at(x as f64 + 0.5, y as f64 + 0.5) {
                            self.blend(x, y, c, a * sa);
                        }
                    }
                }
            }
        });
    }

    /// Strokes `path`. The path is given in user space and `ctm` maps it to
    /// device space, because line width is defined in user space and a
    /// non-uniform CTM makes the pen elliptical.
    pub fn stroke_path(
        &mut self,
        path: &Path,
        ctm: &Transform,
        style: &StrokeStyle,
        paint: &Paint,
        clip: &Clip,
        alpha: f32,
    ) {
        let outline = stroke_outline(path, ctm, style);
        if outline.is_empty() {
            return;
        }
        self.fill_path(&outline, FillRule::NonZero, paint, clip, alpha);
    }
}

// ── stroking ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineCap {
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineJoin {
    Miter,
    Round,
    Bevel,
}

#[derive(Clone, Debug)]
pub struct StrokeStyle {
    /// Line width in user space.
    pub width: f64,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter_limit: f64,
    /// Dash pattern in user space, with its phase.
    pub dash: Vec<f64>,
    pub dash_phase: f64,
}

impl Default for StrokeStyle {
    fn default() -> Self {
        StrokeStyle {
            width: 1.0,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter_limit: 10.0,
            dash: Vec::new(),
            dash_phase: 0.0,
        }
    }
}

/// Builds the fillable outline of a stroke, in device space.
///
/// The path is flattened and dashed in user space (where width and dashes are
/// defined), each piece is stamped as a quad plus join/cap shapes, and the
/// resulting polygons are transformed to device space. Every polygon is
/// emitted with the same orientation so a nonzero fill unions them instead of
/// cancelling overlaps.
pub fn stroke_outline(path: &Path, ctm: &Transform, style: &StrokeStyle) -> Path {
    let scale = ctm.mean_scale();
    // Width 0 means "thinnest line the device can draw" (one pixel).
    let mut w = style.width;
    if w * scale < 1.0 {
        w = 1.0 / scale;
    }
    let hw = w * 0.5;

    let lines = path.flatten(false);
    let dashed: Vec<Polyline> = if style.dash.iter().any(|d| *d > 0.0) {
        lines.iter().flat_map(|p| apply_dash(p, &style.dash, style.dash_phase)).collect()
    } else {
        lines
    };

    let mut out = Path::new();
    for line in &dashed {
        let pts = dedup(&line.pts);
        if pts.len() < 2 {
            // A degenerate subpath still paints a dot under round/square caps.
            if let Some(&p) = pts.first() {
                match style.cap {
                    LineCap::Round => push_circle(&mut out, p, hw, ctm),
                    LineCap::Square => {
                        push_quad(&mut out, (p.0 - hw, p.1 - hw), (p.0 + hw, p.1 - hw), (p.0 + hw, p.1 + hw), (p.0 - hw, p.1 + hw), ctm)
                    }
                    LineCap::Butt => {}
                }
            }
            continue;
        }

        let n = pts.len();
        let seg_count = if line.closed { n } else { n - 1 };
        for i in 0..seg_count {
            let p0 = pts[i];
            let p1 = pts[(i + 1) % n];
            let (dx, dy) = (p1.0 - p0.0, p1.1 - p0.1);
            let len = (dx * dx + dy * dy).sqrt();
            if len < 1e-12 {
                continue;
            }
            let (nx, ny) = (-dy / len * hw, dx / len * hw);
            push_quad(
                &mut out,
                (p0.0 + nx, p0.1 + ny),
                (p1.0 + nx, p1.1 + ny),
                (p1.0 - nx, p1.1 - ny),
                (p0.0 - nx, p0.1 - ny),
                ctm,
            );
        }

        // Joins at the interior vertices (all vertices when closed).
        let join_range: Vec<usize> = if line.closed {
            (0..n).collect()
        } else {
            (1..n - 1).collect()
        };
        for i in join_range {
            let prev = pts[(i + n - 1) % n];
            let cur = pts[i];
            let next = pts[(i + 1) % n];
            push_join(&mut out, prev, cur, next, hw, style, ctm);
        }

        if !line.closed {
            let a = pts[0];
            let b = pts[1];
            push_cap(&mut out, a, b, hw, style.cap, ctm);
            let y = pts[n - 1];
            let x = pts[n - 2];
            push_cap(&mut out, y, x, hw, style.cap, ctm);
        }
    }
    out
}

fn dedup(pts: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::with_capacity(pts.len());
    for &p in pts {
        match out.last() {
            Some(&q) if (q.0 - p.0).abs() < 1e-12 && (q.1 - p.1).abs() < 1e-12 => {}
            _ => out.push(p),
        }
    }
    out
}

/// Emits a quad with a fixed winding direction, transformed to device space.
fn push_quad(
    out: &mut Path,
    a: (f64, f64),
    b: (f64, f64),
    c: (f64, f64),
    d: (f64, f64),
    m: &Transform,
) {
    push_poly(out, &[a, b, c, d], m);
}

/// Emits a polygon, reversing it when needed so every stroke piece has the
/// same orientation. Without this, an overlapping pair could cancel under the
/// nonzero rule and leave a hole.
fn push_poly(out: &mut Path, pts: &[(f64, f64)], m: &Transform) {
    if pts.len() < 3 {
        return;
    }
    let mut area = 0.0f64;
    for i in 0..pts.len() {
        let p = pts[i];
        let q = pts[(i + 1) % pts.len()];
        area += p.0 * q.1 - q.0 * p.1;
    }
    // A mirroring CTM flips handedness; compensate so the sign is stable in
    // device space.
    let flip = m.det() < 0.0;
    let reverse = (area < 0.0) != flip;

    let emit = |out: &mut Path, p: (f64, f64), first: bool| {
        let (x, y) = m.apply(p.0, p.1);
        if first {
            out.move_to(x, y);
        } else {
            out.line_to(x, y);
        }
    };
    if reverse {
        for (i, &p) in pts.iter().rev().enumerate() {
            emit(out, p, i == 0);
        }
    } else {
        for (i, &p) in pts.iter().enumerate() {
            emit(out, p, i == 0);
        }
    }
    out.close();
}

fn push_circle(out: &mut Path, c: (f64, f64), r: f64, m: &Transform) {
    // Enough segments that the polygon error stays sub-pixel at typical sizes.
    let device_r = r * m.mean_scale();
    let steps = ((device_r * 2.0) as usize).clamp(8, 64);
    let pts: Vec<(f64, f64)> = (0..steps)
        .map(|i| {
            let t = i as f64 / steps as f64 * std::f64::consts::TAU;
            (c.0 + r * t.cos(), c.1 + r * t.sin())
        })
        .collect();
    push_poly(out, &pts, m);
}

fn push_cap(
    out: &mut Path,
    end: (f64, f64),
    toward: (f64, f64),
    hw: f64,
    cap: LineCap,
    m: &Transform,
) {
    match cap {
        LineCap::Butt => {}
        LineCap::Round => push_circle(out, end, hw, m),
        LineCap::Square => {
            let (dx, dy) = (end.0 - toward.0, end.1 - toward.1);
            let len = (dx * dx + dy * dy).sqrt();
            if len < 1e-12 {
                return;
            }
            let (ux, uy) = (dx / len * hw, dy / len * hw);
            let (nx, ny) = (-uy, ux);
            push_quad(
                out,
                (end.0 + nx, end.1 + ny),
                (end.0 + nx + ux, end.1 + ny + uy),
                (end.0 - nx + ux, end.1 - ny + uy),
                (end.0 - nx, end.1 - ny),
                m,
            );
        }
    }
}

fn push_join(
    out: &mut Path,
    prev: (f64, f64),
    cur: (f64, f64),
    next: (f64, f64),
    hw: f64,
    style: &StrokeStyle,
    m: &Transform,
) {
    let (d0x, d0y) = (cur.0 - prev.0, cur.1 - prev.1);
    let (d1x, d1y) = (next.0 - cur.0, next.1 - cur.1);
    let l0 = (d0x * d0x + d0y * d0y).sqrt();
    let l1 = (d1x * d1x + d1y * d1y).sqrt();
    if l0 < 1e-12 || l1 < 1e-12 {
        return;
    }
    let (u0x, u0y) = (d0x / l0, d0y / l0);
    let (u1x, u1y) = (d1x / l1, d1y / l1);
    let cross = u0x * u1y - u0y * u1x;
    let dot = u0x * u1x + u0y * u1y;
    if cross.abs() < 1e-12 && dot > 0.0 {
        return; // collinear, nothing to fill
    }

    match style.join {
        LineJoin::Round => push_circle(out, cur, hw, m),
        LineJoin::Bevel | LineJoin::Miter => {
            // Outer side is opposite the turn direction.
            let s = if cross > 0.0 { -1.0 } else { 1.0 };
            let n0 = (-u0y * hw * s, u0x * hw * s);
            let n1 = (-u1y * hw * s, u1x * hw * s);
            let a = (cur.0 + n0.0, cur.1 + n0.1);
            let b = (cur.0 + n1.0, cur.1 + n1.1);

            if style.join == LineJoin::Miter {
                // Miter length ratio = 1/sin(theta/2); compare with the limit
                // before extending to the intersection point.
                let sin_half = ((1.0 - dot) * 0.5).max(0.0).sqrt();
                if sin_half > 1e-9 && 1.0 / sin_half <= style.miter_limit.max(1.0) {
                    if let Some(p) = line_intersect(a, (u0x, u0y), b, (u1x, u1y)) {
                        push_poly(out, &[cur, a, p, b], m);
                        return;
                    }
                }
            }
            push_poly(out, &[cur, a, b], m);
        }
    }
}

fn line_intersect(
    p: (f64, f64),
    dp: (f64, f64),
    q: (f64, f64),
    dq: (f64, f64),
) -> Option<(f64, f64)> {
    let denom = dp.0 * dq.1 - dp.1 * dq.0;
    if denom.abs() < 1e-12 {
        return None;
    }
    let t = ((q.0 - p.0) * dq.1 - (q.1 - p.1) * dq.0) / denom;
    Some((p.0 + dp.0 * t, p.1 + dp.1 * t))
}

/// Splits a polyline into dash segments.
fn apply_dash(line: &Polyline, dash: &[f64], phase: f64) -> Vec<Polyline> {
    let pattern: Vec<f64> = dash.iter().copied().filter(|d| d.is_finite() && *d >= 0.0).collect();
    let total: f64 = pattern.iter().sum();
    if pattern.is_empty() || total <= 0.0 {
        return vec![line.clone()];
    }

    let mut pts = line.pts.clone();
    if line.closed {
        if let Some(&first) = pts.first() {
            pts.push(first);
        }
    }

    // Walk the pattern to the starting phase.
    let mut idx = 0usize;
    let mut remaining = pattern[0];
    let mut on = true;
    let mut ph = phase.rem_euclid(total * if pattern.len() % 2 == 1 { 2.0 } else { 1.0 });
    let mut guard = 0usize;
    while ph > 0.0 && guard < 10_000 {
        guard += 1;
        if ph >= remaining {
            ph -= remaining;
            idx = (idx + 1) % pattern.len();
            remaining = pattern[idx];
            on = !on;
        } else {
            remaining -= ph;
            ph = 0.0;
        }
    }

    let mut out: Vec<Polyline> = Vec::new();
    let mut cur: Vec<(f64, f64)> = Vec::new();
    if on {
        if let Some(&p) = pts.first() {
            cur.push(p);
        }
    }

    for w in pts.windows(2) {
        let (mut p0, p1) = (w[0], w[1]);
        let mut seg_len = ((p1.0 - p0.0).powi(2) + (p1.1 - p0.1).powi(2)).sqrt();
        while seg_len > 0.0 {
            if remaining <= 0.0 {
                idx = (idx + 1) % pattern.len();
                remaining = pattern[idx];
                on = !on;
                if remaining <= 0.0 {
                    // A zero-length entry would spin; nudge past it.
                    continue;
                }
            }
            if remaining >= seg_len {
                remaining -= seg_len;
                if on {
                    cur.push(p1);
                }
                seg_len = 0.0;
            } else {
                let t = remaining / seg_len;
                let mid = (p0.0 + (p1.0 - p0.0) * t, p0.1 + (p1.1 - p0.1) * t);
                if on {
                    cur.push(mid);
                    if cur.len() > 1 {
                        out.push(Polyline { pts: std::mem::take(&mut cur), closed: false });
                    } else {
                        cur.clear();
                    }
                } else {
                    cur.clear();
                    cur.push(mid);
                }
                on = !on;
                seg_len -= remaining;
                p0 = mid;
                idx = (idx + 1) % pattern.len();
                remaining = pattern[idx];
            }
        }
    }
    if on && cur.len() > 1 {
        out.push(Polyline { pts: cur, closed: false });
    }
    out
}

// ── scanline rasterizer ──────────────────────────────────────────────────────

struct Edge {
    /// Top and bottom in device y, with `y0 < y1`.
    y0: f64,
    y1: f64,
    /// x at y0, and dx/dy.
    x0: f64,
    dxdy: f64,
    /// +1 when the original edge pointed down, -1 when up.
    dir: i32,
}

/// Rasterizes `path` and reports per-pixel coverage in `[0, 1]`.
pub fn rasterize<F: FnMut(usize, usize, f32)>(
    path: &Path,
    rule: FillRule,
    w: usize,
    h: usize,
    mut emit: F,
) {
    if w == 0 || h == 0 {
        return;
    }
    let mut edges: Vec<Edge> = Vec::new();
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;

    for line in path.flatten(true) {
        let n = line.pts.len();
        if n < 2 {
            continue;
        }
        for i in 0..n {
            let a = line.pts[i];
            let b = line.pts[(i + 1) % n];
            min_x = min_x.min(a.0);
            max_x = max_x.max(a.0);
            if (a.1 - b.1).abs() < 1e-12 {
                continue; // horizontal edges never cross a sample row
            }
            let (top, bot, dir) = if a.1 < b.1 { (a, b, 1) } else { (b, a, -1) };
            let dxdy = (bot.0 - top.0) / (bot.1 - top.1);
            min_y = min_y.min(top.1);
            max_y = max_y.max(bot.1);
            edges.push(Edge { y0: top.1, y1: bot.1, x0: top.0, dxdy, dir });
        }
    }
    if edges.is_empty() {
        return;
    }

    let y_start = (min_y.floor().max(0.0)) as usize;
    let y_end = (max_y.ceil().min(h as f64)).max(0.0) as usize;
    if y_start >= y_end {
        return;
    }

    // Coverage is accumulated only across the path's own x range. A glyph is a
    // few dozen pixels wide, so clearing a full canvas row per scanline would
    // dominate the cost on text-heavy pages.
    let x_start = (min_x.floor().max(0.0) as usize).min(w);
    let x_end = ((max_x.ceil().max(0.0) as usize) + 1).min(w);
    if x_start >= x_end {
        return;
    }
    let win = x_end - x_start;

    // Sweep order: by top edge.
    edges.sort_by(|a, b| a.y0.partial_cmp(&b.y0).unwrap_or(std::cmp::Ordering::Equal));

    let mut active: Vec<usize> = Vec::new();
    let mut next_edge = 0usize;
    let mut coverage = vec![0f32; win];
    let mut crossings: Vec<(f64, i32)> = Vec::new();
    let sub_weight = 1.0f32 / SUB_SAMPLES as f32;

    for y in y_start..y_end {
        coverage.iter_mut().for_each(|c| *c = 0.0);
        let row_bottom = (y + 1) as f64;

        // Admit edges that start within this row.
        while next_edge < edges.len() && edges[next_edge].y0 < row_bottom {
            active.push(next_edge);
            next_edge += 1;
        }
        // Retire edges that ended above this row.
        active.retain(|&i| edges[i].y1 > y as f64);
        if active.is_empty() {
            continue;
        }

        let mut row_has_ink = false;
        for s in 0..SUB_SAMPLES {
            let sy = y as f64 + (s as f64 + 0.5) / SUB_SAMPLES as f64;
            crossings.clear();
            for &i in &active {
                let e = &edges[i];
                if sy < e.y0 || sy >= e.y1 {
                    continue;
                }
                crossings.push((e.x0 + (sy - e.y0) * e.dxdy, e.dir));
            }
            if crossings.len() < 2 {
                continue;
            }
            crossings.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

            let mut winding = 0i32;
            for k in 0..crossings.len() - 1 {
                winding += crossings[k].1;
                let inside = match rule {
                    FillRule::NonZero => winding != 0,
                    FillRule::EvenOdd => winding % 2 != 0,
                };
                if !inside {
                    continue;
                }
                let xa = crossings[k].0;
                let xb = crossings[k + 1].0;
                if xb <= xa {
                    continue;
                }
                add_span(&mut coverage, x_start, x_end, xa, xb, sub_weight);
                row_has_ink = true;
            }
        }

        if !row_has_ink {
            continue;
        }
        for (i, &c) in coverage.iter().enumerate() {
            if c > 0.0009 {
                emit(x_start + i, y, c.min(1.0));
            }
        }
    }
}

/// Adds `weight` of coverage over the half-open device-x interval `[xa, xb)`,
/// distributing fractional coverage at both ends. `coverage` covers device
/// columns `x_start..x_end`.
fn add_span(coverage: &mut [f32], x_start: usize, x_end: usize, xa: f64, xb: f64, weight: f32) {
    let xa = xa.max(x_start as f64);
    let xb = xb.min(x_end as f64);
    if xb <= xa {
        return;
    }
    let ia = xa.floor() as usize;
    let ib = xb.ceil() as usize;
    if ia >= x_end {
        return;
    }
    let (ia_rel, ib_rel) = (ia - x_start, ib - x_start);
    if ib_rel - ia_rel == 1 {
        coverage[ia_rel] += weight * (xb - xa) as f32;
        return;
    }
    // First partial pixel.
    let first_frac = (ia + 1) as f64 - xa;
    coverage[ia_rel] += weight * first_frac as f32;
    // Whole pixels.
    let full_end = (ib_rel - 1).min(coverage.len());
    for c in coverage.iter_mut().take(full_end).skip(ia_rel + 1) {
        *c += weight;
    }
    // Last partial pixel.
    if ib_rel - 1 < coverage.len() {
        let last_frac = xb - (ib - 1) as f64;
        coverage[ib_rel - 1] += weight * last_frac as f32;
    }
}

// ── image blit ───────────────────────────────────────────────────────────────

/// A decoded image: RGBA8, row-major, top-left origin.
pub struct Bitmap {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Bitmap {
    pub fn new(w: usize, h: usize) -> Bitmap {
        Bitmap { w, h, data: vec![0u8; w * h * 4] }
    }

    #[inline]
    fn texel(&self, x: i64, y: i64) -> [u8; 4] {
        let x = x.clamp(0, self.w as i64 - 1) as usize;
        let y = y.clamp(0, self.h as i64 - 1) as usize;
        let i = (y * self.w + x) * 4;
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }

    /// Bilinear sample at normalized coordinates, origin top-left.
    fn sample_bilinear(&self, u: f64, v: f64) -> [u8; 4] {
        let fx = u * self.w as f64 - 0.5;
        let fy = v * self.h as f64 - 0.5;
        let x0 = fx.floor();
        let y0 = fy.floor();
        let tx = fx - x0;
        let ty = fy - y0;
        let (x0, y0) = (x0 as i64, y0 as i64);
        let c00 = self.texel(x0, y0);
        let c10 = self.texel(x0 + 1, y0);
        let c01 = self.texel(x0, y0 + 1);
        let c11 = self.texel(x0 + 1, y0 + 1);
        let mut out = [0u8; 4];
        for i in 0..4 {
            let top = c00[i] as f64 * (1.0 - tx) + c10[i] as f64 * tx;
            let bot = c01[i] as f64 * (1.0 - tx) + c11[i] as f64 * tx;
            out[i] = (top * (1.0 - ty) + bot * ty + 0.5).clamp(0.0, 255.0) as u8;
        }
        out
    }

    #[inline]
    fn sample_nearest(&self, u: f64, v: f64) -> [u8; 4] {
        let x = (u * self.w as f64).floor() as i64;
        let y = (v * self.h as f64).floor() as i64;
        self.texel(x, y)
    }
}

impl Canvas {
    /// Draws `img` through `ctm`, which maps the PDF image unit square
    /// (0,0)-(1,1) to device space. The image's first row is the *top* of the
    /// unit square, which in PDF terms is `y = 1`.
    pub fn draw_image(
        &mut self,
        img: &Bitmap,
        ctm: &Transform,
        clip: &Clip,
        alpha: f32,
        smooth: bool,
    ) {
        if img.w == 0 || img.h == 0 || alpha <= 0.0 {
            return;
        }
        let inv = match ctm.invert() {
            Some(i) => i,
            None => return,
        };
        // Device bounds of the transformed unit square.
        let corners = [
            ctm.apply(0.0, 0.0),
            ctm.apply(1.0, 0.0),
            ctm.apply(0.0, 1.0),
            ctm.apply(1.0, 1.0),
        ];
        let mut x0 = f64::INFINITY;
        let mut y0 = f64::INFINITY;
        let mut x1 = f64::NEG_INFINITY;
        let mut y1 = f64::NEG_INFINITY;
        for (x, y) in corners {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        let (cx0, cy0, cx1, cy1) = clip.bounds;
        let px0 = (x0.floor().max(cx0 as f64)).max(0.0) as usize;
        let py0 = (y0.floor().max(cy0 as f64)).max(0.0) as usize;
        let px1 = (x1.ceil().min(cx1 as f64)).min(self.w as f64).max(0.0) as usize;
        let py1 = (y1.ceil().min(cy1 as f64)).min(self.h as f64).max(0.0) as usize;
        if px0 >= px1 || py0 >= py1 {
            return;
        }

        // Downscaling by more than 2x aliases badly with point sampling, so
        // average over the source footprint instead.
        let device_area = ((x1 - x0) * (y1 - y0)).max(1.0);
        let src_area = (img.w * img.h) as f64;
        let box_filter = smooth && src_area > device_area * 4.0;
        let fx = (img.w as f64 / (x1 - x0).max(1.0)).max(1.0);
        let fy = (img.h as f64 / (y1 - y0).max(1.0)).max(1.0);

        for py in py0..py1 {
            for px in px0..px1 {
                let clip_a = clip.at(px, py);
                if clip_a == 0 {
                    continue;
                }
                let (u, v) = inv.apply(px as f64 + 0.5, py as f64 + 0.5);
                if !(-0.001..=1.001).contains(&u) || !(-0.001..=1.001).contains(&v) {
                    continue;
                }
                let u = u.clamp(0.0, 1.0);
                // PDF image space puts row 0 at the top, which is v = 1.
                let vv = (1.0 - v).clamp(0.0, 1.0);
                let texel = if box_filter {
                    sample_box(img, u, vv, fx, fy)
                } else if smooth {
                    img.sample_bilinear(u, vv)
                } else {
                    img.sample_nearest(u, vv)
                };
                let a = alpha * (texel[3] as f32 / 255.0) * (clip_a as f32 / 255.0);
                if a > 0.0 {
                    self.blend(px, py, [texel[0], texel[1], texel[2]], a);
                }
            }
        }
    }
}

/// Averages the source footprint of one device pixel. Keeps downscaled scans
/// and photos from breaking into aliased speckle.
fn sample_box(img: &Bitmap, u: f64, v: f64, fx: f64, fy: f64) -> [u8; 4] {
    let cx = u * img.w as f64;
    let cy = v * img.h as f64;
    let hx = (fx * 0.5).min(64.0);
    let hy = (fy * 0.5).min(64.0);
    let x0 = ((cx - hx).floor() as i64).max(0);
    let x1 = ((cx + hx).ceil() as i64).min(img.w as i64);
    let y0 = ((cy - hy).floor() as i64).max(0);
    let y1 = ((cy + hy).ceil() as i64).min(img.h as i64);
    if x0 >= x1 || y0 >= y1 {
        return img.sample_nearest(u, v);
    }
    let mut acc = [0f64; 4];
    let mut n = 0f64;
    for y in y0..y1 {
        for x in x0..x1 {
            let t = img.texel(x, y);
            for i in 0..4 {
                acc[i] += t[i] as f64;
            }
            n += 1.0;
        }
    }
    let mut out = [0u8; 4];
    for i in 0..4 {
        out[i] = (acc[i] / n + 0.5).clamp(0.0, 255.0) as u8;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coverage_of(path: &Path, rule: FillRule, w: usize, h: usize) -> Vec<f32> {
        let mut cov = vec![0f32; w * h];
        rasterize(path, rule, w, h, |x, y, c| cov[y * w + x] = c);
        cov
    }

    #[test]
    fn transform_roundtrip() {
        let m = Transform::new(2.0, 0.5, -0.25, 3.0, 10.0, -4.0);
        let inv = m.invert().unwrap();
        let (x, y) = m.apply(7.0, -2.0);
        let (bx, by) = inv.apply(x, y);
        assert!((bx - 7.0).abs() < 1e-9, "x roundtrip {bx}");
        assert!((by + 2.0).abs() < 1e-9, "y roundtrip {by}");
    }

    #[test]
    fn transform_composition_matches_manual() {
        let a = Transform::scale(2.0, 3.0);
        let b = Transform::translate(5.0, 7.0);
        // `a.then(b)` scales first, then translates.
        let m = a.then(&b);
        assert_eq!(m.apply(1.0, 1.0), (7.0, 10.0));
    }

    #[test]
    fn axis_aligned_rect_is_exactly_covered() {
        let mut p = Path::new();
        p.rect(2.0, 3.0, 4.0, 2.0);
        let cov = coverage_of(&p, FillRule::NonZero, 10, 10);
        for y in 0..10 {
            for x in 0..10 {
                let inside = (2..6).contains(&x) && (3..5).contains(&y);
                let c = cov[y * 10 + x];
                if inside {
                    assert!((c - 1.0).abs() < 1e-3, "pixel {x},{y} = {c}, want 1");
                } else {
                    assert!(c < 1e-3, "pixel {x},{y} = {c}, want 0");
                }
            }
        }
    }

    #[test]
    fn half_covered_pixel_gets_half_alpha() {
        let mut p = Path::new();
        p.rect(0.0, 0.0, 0.5, 1.0);
        let cov = coverage_of(&p, FillRule::NonZero, 4, 1);
        assert!((cov[0] - 0.5).abs() < 0.02, "got {}", cov[0]);
    }

    #[test]
    fn fill_rules_differ_on_self_overlap() {
        // Two concentric squares wound the same way: nonzero fills the middle,
        // even-odd leaves it hollow.
        let mut p = Path::new();
        p.rect(0.0, 0.0, 10.0, 10.0);
        p.rect(3.0, 3.0, 4.0, 4.0);
        let nz = coverage_of(&p, FillRule::NonZero, 10, 10);
        let eo = coverage_of(&p, FillRule::EvenOdd, 10, 10);
        let center = 5 * 10 + 5;
        assert!(nz[center] > 0.99, "nonzero centre {}", nz[center]);
        assert!(eo[center] < 0.01, "even-odd centre {}", eo[center]);
        // The outer ring is filled under both rules.
        let ring = 1 * 10 + 1;
        assert!(nz[ring] > 0.99 && eo[ring] > 0.99);
    }

    #[test]
    fn circle_area_is_close_to_pi_r_squared() {
        let mut p = Path::new();
        p.circle(50.0, 50.0, 40.0);
        let cov = coverage_of(&p, FillRule::NonZero, 100, 100);
        let area: f32 = cov.iter().sum();
        let expect = std::f32::consts::PI * 40.0 * 40.0;
        let err = (area - expect).abs() / expect;
        assert!(err < 0.01, "area {area} vs {expect} ({:.3}% off)", err * 100.0);
    }

    #[test]
    fn stroke_of_horizontal_line_covers_expected_band() {
        let mut p = Path::new();
        p.move_to(2.0, 10.0);
        p.line_to(18.0, 10.0);
        let style = StrokeStyle { width: 4.0, ..Default::default() };
        let outline = stroke_outline(&p, &Transform::identity(), &style);
        let cov = coverage_of(&outline, FillRule::NonZero, 20, 20);
        // Band spans y in [8, 12), x in [2, 18).
        assert!(cov[10 * 20 + 10] > 0.99, "inside band");
        assert!(cov[8 * 20 + 10] > 0.99, "top edge of band");
        assert!(cov[7 * 20 + 10] < 0.01, "above band");
        assert!(cov[12 * 20 + 10] < 0.01, "below band");
        assert!(cov[10 * 20 + 1] < 0.01, "before butt cap");
    }

    #[test]
    fn overlapping_stroke_segments_do_not_cancel() {
        // A tight zig-zag makes consecutive quads overlap heavily; with
        // inconsistent winding the nonzero rule would punch holes.
        let mut p = Path::new();
        p.move_to(5.0, 5.0);
        p.line_to(15.0, 15.0);
        p.line_to(5.0, 15.0);
        p.line_to(15.0, 5.0);
        let style = StrokeStyle { width: 6.0, join: LineJoin::Round, ..Default::default() };
        let outline = stroke_outline(&p, &Transform::identity(), &style);
        let cov = coverage_of(&outline, FillRule::NonZero, 20, 20);
        assert!(cov[10 * 20 + 10] > 0.99, "crossing point must stay filled");
    }

    #[test]
    fn zero_width_stroke_is_a_hairline() {
        let mut p = Path::new();
        p.move_to(0.0, 5.5);
        p.line_to(10.0, 5.5);
        let style = StrokeStyle { width: 0.0, ..Default::default() };
        let outline = stroke_outline(&p, &Transform::identity(), &style);
        let cov = coverage_of(&outline, FillRule::NonZero, 10, 10);
        assert!(cov[5 * 10 + 5] > 0.9, "hairline must paint one pixel row");
    }

    #[test]
    fn dashes_leave_gaps() {
        let mut p = Path::new();
        p.move_to(0.0, 5.0);
        p.line_to(20.0, 5.0);
        let style = StrokeStyle {
            width: 2.0,
            dash: vec![4.0, 4.0],
            dash_phase: 0.0,
            ..Default::default()
        };
        let outline = stroke_outline(&p, &Transform::identity(), &style);
        let cov = coverage_of(&outline, FillRule::NonZero, 20, 10);
        assert!(cov[5 * 20 + 2] > 0.9, "first dash on");
        assert!(cov[5 * 20 + 6] < 0.1, "first gap off");
        assert!(cov[5 * 20 + 10] > 0.9, "second dash on");
    }

    #[test]
    fn clip_masks_the_fill() {
        let mut canvas = Canvas::new(10, 10);
        canvas.clear([255, 255, 255]);
        let clip = Clip::full(10, 10).intersect_rect(0.0, 0.0, 5.0, 10.0);
        let mut p = Path::new();
        p.rect(0.0, 0.0, 10.0, 10.0);
        canvas.fill_path(&p, FillRule::NonZero, &Paint::Solid([0, 0, 0]), &clip, 1.0);
        let px = |x: usize, y: usize| canvas.data[(y * 10 + x) * 4];
        assert_eq!(px(2, 5), 0, "inside clip should be painted");
        assert_eq!(px(7, 5), 255, "outside clip must stay white");
    }

    #[test]
    fn image_draw_respects_orientation() {
        // 2x1 image: left texel red, right texel blue. Row 0 is the top of the
        // unit square, so with an identity-ish CTM it lands at the top.
        let mut img = Bitmap::new(2, 2);
        let set = |img: &mut Bitmap, x: usize, y: usize, c: [u8; 4]| {
            let i = (y * 2 + x) * 4;
            img.data[i..i + 4].copy_from_slice(&c);
        };
        set(&mut img, 0, 0, [255, 0, 0, 255]);
        set(&mut img, 1, 0, [0, 0, 255, 255]);
        set(&mut img, 0, 1, [0, 255, 0, 255]);
        set(&mut img, 1, 1, [255, 255, 0, 255]);

        let mut canvas = Canvas::new(4, 4);
        canvas.clear([255, 255, 255]);
        // Map the unit square onto the whole canvas with y flipped, which is
        // what the PDF image operator does after `cm`.
        let ctm = Transform::new(4.0, 0.0, 0.0, -4.0, 0.0, 4.0);
        canvas.draw_image(&img, &ctm, &Clip::full(4, 4), 1.0, false);
        let px = |x: usize, y: usize| {
            let i = (y * 4 + x) * 4;
            [canvas.data[i], canvas.data[i + 1], canvas.data[i + 2]]
        };
        assert_eq!(px(0, 0), [255, 0, 0], "top-left texel");
        assert_eq!(px(3, 0), [0, 0, 255], "top-right texel");
        assert_eq!(px(0, 3), [0, 255, 0], "bottom-left texel");
    }
}
