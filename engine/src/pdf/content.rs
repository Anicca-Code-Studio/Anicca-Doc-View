//! Content stream interpreter (ISO 32000-1 clause 8/9).
//!
//! Walks a page's operator stream and paints into a `raster::Canvas`.
//! Currently covers the graphics-state, path, clipping, colour and XObject
//! operators. Text operators are recognised and their operands consumed; glyph
//! painting arrives with the font module, and image XObjects with the image
//! decoder.

use std::collections::HashSet;
use std::rc::Rc;

use crate::raster::{Canvas, Clip, FillRule, LineCap, LineJoin, Paint, Path, StrokeStyle, Transform};

use super::colorspace::{self, ColorSpace};
use super::font::{Font, FontCache};
use super::lexer::Lexer;
use super::object::Obj;
use super::xref::PdfFile;

/// Nesting cap for form XObjects and patterns.
const MAX_XOBJECT_DEPTH: usize = 12;

/// Text parameters that live in the graphics state (saved and restored by
/// `q`/`Q`), as opposed to the text matrices which reset at every `BT`.
#[derive(Clone)]
pub struct TextParams {
    pub font: Option<Rc<Font>>,
    pub size: f64,
    pub char_spacing: f64,
    pub word_spacing: f64,
    /// Horizontal scaling, already divided by 100.
    pub h_scale: f64,
    pub leading: f64,
    pub rise: f64,
    pub render_mode: i32,
}

impl Default for TextParams {
    fn default() -> Self {
        TextParams {
            font: None,
            size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            h_scale: 1.0,
            leading: 0.0,
            rise: 0.0,
            render_mode: 0,
        }
    }
}

#[derive(Clone)]
pub struct GState {
    pub ctm: Transform,
    pub clip: Clip,

    pub fill_cs: ColorSpace,
    pub fill_comps: Vec<f32>,
    pub fill_rgb: [u8; 3],
    /// True when the fill colour comes from a pattern we cannot evaluate yet.
    pub fill_is_pattern: bool,

    pub stroke_cs: ColorSpace,
    pub stroke_comps: Vec<f32>,
    pub stroke_rgb: [u8; 3],
    pub stroke_is_pattern: bool,

    pub line_width: f64,
    pub line_cap: LineCap,
    pub line_join: LineJoin,
    pub miter_limit: f64,
    pub dash: Vec<f64>,
    pub dash_phase: f64,

    pub fill_alpha: f32,
    pub stroke_alpha: f32,

    pub text: TextParams,
}

impl GState {
    fn new(ctm: Transform, clip: Clip) -> GState {
        GState {
            text: TextParams::default(),
            ctm,
            clip,
            fill_cs: ColorSpace::DeviceGray,
            fill_comps: vec![0.0],
            fill_rgb: [0, 0, 0],
            fill_is_pattern: false,
            stroke_cs: ColorSpace::DeviceGray,
            stroke_comps: vec![0.0],
            stroke_rgb: [0, 0, 0],
            stroke_is_pattern: false,
            line_width: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 10.0,
            dash: Vec::new(),
            dash_phase: 0.0,
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
        }
    }

    fn stroke_style(&self) -> StrokeStyle {
        StrokeStyle {
            width: self.line_width,
            cap: self.line_cap,
            join: self.line_join,
            miter_limit: self.miter_limit,
            dash: self.dash.clone(),
            dash_phase: self.dash_phase,
        }
    }
}

pub struct Renderer<'a> {
    pub file: &'a PdfFile,
    pub canvas: &'a mut Canvas,
    pub gs: GState,
    stack: Vec<GState>,
    /// Current path, in user space (the CTM cannot change mid-path).
    path: Path,
    path_start: (f64, f64),
    pen: (f64, f64),
    /// Clip requested by `W`/`W*`, applied after the next painting operator.
    pending_clip: Option<FillRule>,
    /// Object numbers of forms currently being executed, to break cycles.
    active_forms: HashSet<u32>,
    /// Optional-content groups that are switched off.
    hidden_ocgs: HashSet<(u32, u16)>,
    /// Depth of marked-content sections inside a hidden optional-content block.
    hidden_depth: usize,
    /// Nesting depth of marked-content while hidden, used to find the matching EMC.
    mc_depth: usize,

    /// Text matrix and text line matrix; both reset at `BT`.
    tm: Transform,
    tlm: Transform,
    /// Glyph outlines collected by text render modes 4-7, applied at `ET`.
    text_clip: Option<Path>,
    fonts: FontCache,
    /// Recursion guard for Type 3 glyph procedures.
    type3_depth: usize,
    /// Receives every painted glyph when set, for the selectable text layer.
    pub collector: Option<super::text::Collector>,
    /// Set to false to walk the page for text only, skipping all painting.
    pub painting: bool,
}

impl<'a> Renderer<'a> {
    pub fn new(file: &'a PdfFile, canvas: &'a mut Canvas, base_ctm: Transform) -> Renderer<'a> {
        let clip = Clip::full(canvas.w, canvas.h);
        Renderer {
            file,
            canvas,
            gs: GState::new(base_ctm, clip),
            stack: Vec::new(),
            path: Path::new(),
            path_start: (0.0, 0.0),
            pen: (0.0, 0.0),
            pending_clip: None,
            active_forms: HashSet::new(),
            hidden_ocgs: collect_hidden_ocgs(file),
            hidden_depth: 0,
            mc_depth: 0,
            tm: Transform::identity(),
            tlm: Transform::identity(),
            text_clip: None,
            fonts: FontCache::new(),
            type3_depth: 0,
            collector: None,
            painting: true,
        }
    }

    // ── main loop ────────────────────────────────────────────────────────────

    pub fn run(&mut self, content: &[u8], resources: &Obj) {
        self.exec(content, resources, 0);
    }

    fn exec(&mut self, content: &[u8], resources: &Obj, depth: usize) {
        if depth > MAX_XOBJECT_DEPTH {
            return;
        }
        let mut lx = Lexer::new(content);
        let mut operands: Vec<Obj> = Vec::new();

        loop {
            lx.skip_ws();
            if lx.eof() {
                break;
            }
            let before = lx.pos;
            if let Some(obj) = lx.parse_obj() {
                // Guard against a parser that made no progress.
                if lx.pos == before {
                    lx.pos += 1;
                    continue;
                }
                if operands.len() < 64 {
                    operands.push(obj);
                }
                continue;
            }
            let op = lx.read_keyword();
            if op.is_empty() {
                lx.pos += 1;
                operands.clear();
                continue;
            }
            if op == b"BI" {
                let next = self.inline_image(content, lx.pos, resources);
                lx.pos = next.min(content.len());
                operands.clear();
                continue;
            }
            self.op(op, &operands, resources, depth);
            operands.clear();
        }
    }

    fn op(&mut self, op: &[u8], args: &[Obj], resources: &Obj, depth: usize) {
        // Inside a hidden optional-content block only the nesting operators
        // matter; everything else is skipped.
        if self.hidden_depth > 0 {
            match op {
                b"BDC" | b"BMC" => self.mc_depth += 1,
                b"EMC" => {
                    self.mc_depth = self.mc_depth.saturating_sub(1);
                    if self.mc_depth < self.hidden_depth {
                        self.hidden_depth = 0;
                    }
                }
                _ => {}
            }
            return;
        }

        let n = |i: usize| -> f64 { args.get(i).and_then(|o| o.as_f64()).unwrap_or(0.0) };

        match op {
            // ── graphics state ───────────────────────────────────────────────
            b"q" => {
                if self.stack.len() < 256 {
                    self.stack.push(self.gs.clone());
                }
            }
            b"Q" => {
                if let Some(g) = self.stack.pop() {
                    self.gs = g;
                }
            }
            b"cm" => {
                if args.len() >= 6 {
                    let m = Transform::new(n(0), n(1), n(2), n(3), n(4), n(5));
                    self.gs.ctm = m.then(&self.gs.ctm);
                }
            }
            b"w" => self.gs.line_width = n(0).max(0.0),
            b"J" => {
                self.gs.line_cap = match n(0) as i32 {
                    1 => LineCap::Round,
                    2 => LineCap::Square,
                    _ => LineCap::Butt,
                }
            }
            b"j" => {
                self.gs.line_join = match n(0) as i32 {
                    1 => LineJoin::Round,
                    2 => LineJoin::Bevel,
                    _ => LineJoin::Miter,
                }
            }
            b"M" => self.gs.miter_limit = n(0).max(1.0),
            b"d" => {
                let pattern = args
                    .first()
                    .and_then(|o| o.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_f64()).filter(|v| *v >= 0.0).collect())
                    .unwrap_or_default();
                self.gs.dash = pattern;
                self.gs.dash_phase = n(1).max(0.0);
            }
            // Flatness and rendering intent do not affect our output.
            b"i" | b"ri" => {}
            b"gs" => {
                if let Some(name) = args.first().and_then(|o| o.as_name()) {
                    self.apply_ext_gstate(name, resources);
                }
            }

            // ── path construction ────────────────────────────────────────────
            b"m" => {
                self.path.move_to(n(0), n(1));
                self.path_start = (n(0), n(1));
                self.pen = (n(0), n(1));
            }
            b"l" => {
                self.path.line_to(n(0), n(1));
                self.pen = (n(0), n(1));
            }
            b"c" => {
                self.path.curve_to(n(0), n(1), n(2), n(3), n(4), n(5));
                self.pen = (n(4), n(5));
            }
            b"v" => {
                // First control point is the current point.
                let p = self.pen;
                self.path.curve_to(p.0, p.1, n(0), n(1), n(2), n(3));
                self.pen = (n(2), n(3));
            }
            b"y" => {
                // Second control point is the end point.
                self.path.curve_to(n(0), n(1), n(2), n(3), n(2), n(3));
                self.pen = (n(2), n(3));
            }
            b"h" => {
                self.path.close();
                self.pen = self.path_start;
            }
            b"re" => {
                self.path.rect(n(0), n(1), n(2), n(3));
                self.path_start = (n(0), n(1));
                self.pen = (n(0), n(1));
            }

            // ── path painting ────────────────────────────────────────────────
            b"n" => self.end_path(false, false, FillRule::NonZero),
            b"f" | b"F" => self.end_path(true, false, FillRule::NonZero),
            b"f*" => self.end_path(true, false, FillRule::EvenOdd),
            b"S" => self.end_path(false, true, FillRule::NonZero),
            b"s" => {
                self.path.close();
                self.end_path(false, true, FillRule::NonZero);
            }
            b"B" => self.end_path(true, true, FillRule::NonZero),
            b"B*" => self.end_path(true, true, FillRule::EvenOdd),
            b"b" => {
                self.path.close();
                self.end_path(true, true, FillRule::NonZero);
            }
            b"b*" => {
                self.path.close();
                self.end_path(true, true, FillRule::EvenOdd);
            }
            b"W" => self.pending_clip = Some(FillRule::NonZero),
            b"W*" => self.pending_clip = Some(FillRule::EvenOdd),

            // ── colour ───────────────────────────────────────────────────────
            b"g" => self.set_color(false, ColorSpace::DeviceGray, &[n(0) as f32]),
            b"G" => self.set_color(true, ColorSpace::DeviceGray, &[n(0) as f32]),
            b"rg" => self.set_color(false, ColorSpace::DeviceRGB, &[n(0) as f32, n(1) as f32, n(2) as f32]),
            b"RG" => self.set_color(true, ColorSpace::DeviceRGB, &[n(0) as f32, n(1) as f32, n(2) as f32]),
            b"k" => self.set_color(
                false,
                ColorSpace::DeviceCMYK,
                &[n(0) as f32, n(1) as f32, n(2) as f32, n(3) as f32],
            ),
            b"K" => self.set_color(
                true,
                ColorSpace::DeviceCMYK,
                &[n(0) as f32, n(1) as f32, n(2) as f32, n(3) as f32],
            ),
            b"cs" | b"CS" => {
                let stroke = op == b"CS";
                if let Some(o) = args.first() {
                    let cs = colorspace::parse(self.file, o, resources, 0);
                    let init = cs.initial_color();
                    let is_pattern = matches!(cs, ColorSpace::Pattern { .. });
                    let rgb = if is_pattern { [0, 0, 0] } else { cs.to_rgb(&init) };
                    if stroke {
                        self.gs.stroke_cs = cs;
                        self.gs.stroke_comps = init;
                        self.gs.stroke_rgb = rgb;
                        self.gs.stroke_is_pattern = is_pattern;
                    } else {
                        self.gs.fill_cs = cs;
                        self.gs.fill_comps = init;
                        self.gs.fill_rgb = rgb;
                        self.gs.fill_is_pattern = is_pattern;
                    }
                }
            }
            b"sc" | b"scn" | b"SC" | b"SCN" => {
                let stroke = op == b"SC" || op == b"SCN";
                self.set_color_components(stroke, args);
            }

            // ── XObjects and shading ─────────────────────────────────────────
            b"Do" => {
                if let Some(name) = args.first().and_then(|o| o.as_name()) {
                    self.do_xobject(name, resources, depth);
                }
            }
            // Shadings need the function evaluator; ignored for now rather than
            // painting a wrong flat colour over the page.
            b"sh" => {}

            // ── marked content and optional content ──────────────────────────
            b"BDC" => {
                self.mc_depth += 1;
                if args.first().and_then(|o| o.as_name()) == Some("OC")
                    && self.is_hidden_oc(args.get(1), resources)
                {
                    self.hidden_depth = self.mc_depth;
                }
            }
            b"BMC" => self.mc_depth += 1,
            b"EMC" => self.mc_depth = self.mc_depth.saturating_sub(1),
            b"MP" | b"DP" | b"BX" | b"EX" => {}

            // ── text ─────────────────────────────────────────────────────────
            b"BT" => {
                self.tm = Transform::identity();
                self.tlm = Transform::identity();
            }
            b"ET" => {
                if let Some(clip_path) = self.text_clip.take() {
                    // Modes 4-7 intersect the clip with the glyph outlines. An
                    // empty set clips everything away, which is what a
                    // clip-only text object with no glyphs means.
                    self.gs.clip = self.gs.clip.intersect_path(&clip_path, FillRule::NonZero);
                }
            }
            b"Tf" => {
                self.gs.text.size = n(1);
                if let Some(name) = args.first().and_then(|o| o.as_name()) {
                    self.gs.text.font = self.fonts.get(self.file, resources, name);
                }
            }
            b"Td" => self.text_newline(n(0), n(1)),
            b"TD" => {
                self.gs.text.leading = -n(1);
                self.text_newline(n(0), n(1));
            }
            b"Tm" => {
                if args.len() >= 6 {
                    self.tlm = Transform::new(n(0), n(1), n(2), n(3), n(4), n(5));
                    self.tm = self.tlm;
                }
            }
            b"T*" => {
                let ty = -self.gs.text.leading;
                self.text_newline(0.0, ty);
            }
            b"TL" => self.gs.text.leading = n(0),
            b"Tc" => self.gs.text.char_spacing = n(0),
            b"Tw" => self.gs.text.word_spacing = n(0),
            b"Tz" => self.gs.text.h_scale = n(0) / 100.0,
            b"Ts" => self.gs.text.rise = n(0),
            b"Tr" => self.gs.text.render_mode = n(0) as i32,
            b"Tj" => {
                if let Some(s) = args.first().and_then(|o| o.as_str_bytes()) {
                    let s = s.to_vec();
                    self.show_text(&s, resources, depth);
                }
            }
            b"'" => {
                let ty = -self.gs.text.leading;
                self.text_newline(0.0, ty);
                if let Some(s) = args.first().and_then(|o| o.as_str_bytes()) {
                    let s = s.to_vec();
                    self.show_text(&s, resources, depth);
                }
            }
            b"\"" => {
                self.gs.text.word_spacing = n(0);
                self.gs.text.char_spacing = n(1);
                let ty = -self.gs.text.leading;
                self.text_newline(0.0, ty);
                if let Some(s) = args.get(2).and_then(|o| o.as_str_bytes()) {
                    let s = s.to_vec();
                    self.show_text(&s, resources, depth);
                }
            }
            b"TJ" => {
                let items: Vec<Obj> = args
                    .first()
                    .and_then(|o| o.as_array().map(|a| a.to_vec()))
                    .unwrap_or_default();
                for item in items {
                    match item {
                        Obj::Str(s) => self.show_text(&s, resources, depth),
                        other => {
                            if let Some(adj) = other.as_f64() {
                                // A positive number moves left by adj/1000 em.
                                let tx = -adj / 1000.0 * self.gs.text.size * self.gs.text.h_scale;
                                self.tm = Transform::translate(tx, 0.0).then(&self.tm);
                            }
                        }
                    }
                }
            }
            // Type 3 glyph metrics; the advance comes from /Widths instead.
            b"d0" | b"d1" => {}

            _ => {}
        }
    }

    // ── text ─────────────────────────────────────────────────────────────────

    fn text_newline(&mut self, tx: f64, ty: f64) {
        self.tlm = Transform::translate(tx, ty).then(&self.tlm);
        self.tm = self.tlm;
    }

    fn show_text(&mut self, bytes: &[u8], resources: &Obj, depth: usize) {
        let font = match self.gs.text.font.clone() {
            Some(f) => f,
            None => return,
        };
        let t = self.gs.text.clone();
        // Render modes (Tr): 0 fill, 1 stroke, 2 both, 3 invisible,
        // 4-6 the same plus clip, 7 clip only.
        let mode = t.render_mode;
        let fills = matches!(mode, 0 | 2 | 4 | 6);
        let strokes = matches!(mode, 1 | 2 | 5 | 6);
        let clips = (4..=7).contains(&mode);
        let needs_outline = fills || strokes || clips;
        // Mode 7 with no glyphs still clips everything away, so the accumulator
        // must exist even before the first glyph.
        if mode == 7 {
            self.text_clip.get_or_insert_with(Path::new);
        }

        for (code, cid, nbytes) in font.decode(bytes) {
            // Glyph space -> text space -> user space -> device space.
            let trm = Transform::new(t.size * t.h_scale, 0.0, 0.0, t.size, 0.0, t.rise)
                .then(&self.tm)
                .then(&self.gs.ctm);

            let w0 = font.advance(code, cid);

            if needs_outline && self.painting {
                if font.type3.is_some() {
                    self.draw_type3_glyph(&font, code, &trm, resources, depth);
                } else if let Some(outline) = font.outline(code, cid) {
                    if !outline.is_empty() {
                        let device = outline.transformed(&trm);
                        if fills {
                            let paint = Paint::Solid(self.gs.fill_rgb);
                            if !self.gs.fill_is_pattern {
                                self.canvas.fill_path(
                                    &device,
                                    FillRule::NonZero,
                                    &paint,
                                    &self.gs.clip,
                                    self.gs.fill_alpha,
                                );
                            }
                        }
                        if strokes && !self.gs.stroke_is_pattern {
                            // Line width is a user-space quantity; convert it to
                            // the glyph's own space so the stroked outline comes
                            // out the right thickness on screen.
                            let mut style = self.gs.stroke_style();
                            let user_to_dev = self.gs.ctm.mean_scale();
                            let glyph_to_dev = trm.mean_scale().max(1e-9);
                            style.width = self.gs.line_width * user_to_dev / glyph_to_dev;
                            style.dash.clear();
                            let paint = Paint::Solid(self.gs.stroke_rgb);
                            self.canvas.stroke_path(
                                &outline,
                                &trm,
                                &style,
                                &paint,
                                &self.gs.clip,
                                self.gs.stroke_alpha,
                            );
                        }
                        if clips {
                            let acc = self.text_clip.get_or_insert_with(Path::new);
                            acc.segs.extend(device.segs);
                        }
                    }
                }
            }

            if self.collector.is_some() {
                let text_to_device = self.tm.then(&self.gs.ctm);
                if let Some(c) = self.collector.as_mut() {
                    c.push_glyph(&font, code, cid, w0, &t, &trm, &text_to_device);
                }
            }

            // Word spacing applies to the single-byte code 32 only.
            let word = if nbytes == 1 && code == 32 { t.word_spacing } else { 0.0 };
            if font.vertical {
                let ty = -(w0 * t.size + t.char_spacing + word);
                self.tm = Transform::translate(0.0, ty).then(&self.tm);
            } else {
                let tx = (w0 * t.size + t.char_spacing + word) * t.h_scale;
                self.tm = Transform::translate(tx, 0.0).then(&self.tm);
            }
        }
    }

    /// Runs a Type 3 glyph procedure, which is a content stream in glyph space.
    fn draw_type3_glyph(
        &mut self,
        font: &Rc<Font>,
        code: u32,
        trm: &Transform,
        resources: &Obj,
        depth: usize,
    ) {
        if self.type3_depth >= 4 {
            return;
        }
        let t3 = match &font.type3 {
            Some(t) => t,
            None => return,
        };
        let name = match font.glyph_name_for_type3(code) {
            Some(n) => n,
            None => return,
        };
        let proc_stream = match t3.char_procs.get(&name).map(|o| self.file.resolve(o)) {
            Some(o) => o,
            None => return,
        };
        let stream = match proc_stream.as_stream() {
            Some(s) => s,
            None => return,
        };
        let data = match self.file.stream_data_of(stream) {
            Some(d) => d,
            None => return,
        };

        let res = if t3.resources.as_dict().is_some() {
            t3.resources.clone()
        } else {
            resources.clone()
        };

        let saved_gs = self.gs.clone();
        let saved_stack = self.stack.len();
        let saved_tm = self.tm;
        let saved_tlm = self.tlm;
        let saved_path = std::mem::take(&mut self.path);

        self.gs.ctm = t3.matrix.then(trm);
        self.type3_depth += 1;
        self.exec(&data, &res, depth + 1);
        self.type3_depth -= 1;

        self.path = saved_path;
        self.tm = saved_tm;
        self.tlm = saved_tlm;
        self.stack.truncate(saved_stack);
        self.gs = saved_gs;
    }

    // ── painting ─────────────────────────────────────────────────────────────

    fn end_path(&mut self, fill: bool, stroke: bool, rule: FillRule) {
        let path = std::mem::take(&mut self.path);
        let (fill, stroke) = if self.painting { (fill, stroke) } else { (false, false) };
        if !path.is_empty() {
            if fill && !self.gs.fill_is_pattern {
                let device = path.transformed(&self.gs.ctm);
                let paint = Paint::Solid(self.gs.fill_rgb);
                self.canvas
                    .fill_path(&device, rule, &paint, &self.gs.clip, self.gs.fill_alpha);
            }
            if stroke && !self.gs.stroke_is_pattern {
                let style = self.gs.stroke_style();
                let paint = Paint::Solid(self.gs.stroke_rgb);
                self.canvas.stroke_path(
                    &path,
                    &self.gs.ctm,
                    &style,
                    &paint,
                    &self.gs.clip,
                    self.gs.stroke_alpha,
                );
            }
            if let Some(clip_rule) = self.pending_clip.take() {
                let device = path.transformed(&self.gs.ctm);
                self.gs.clip = self.gs.clip.intersect_path(&device, clip_rule);
            }
        } else {
            self.pending_clip = None;
        }
        self.pen = (0.0, 0.0);
        self.path_start = (0.0, 0.0);
    }

    fn set_color(&mut self, stroke: bool, cs: ColorSpace, comps: &[f32]) {
        let rgb = cs.to_rgb(comps);
        if stroke {
            self.gs.stroke_cs = cs;
            self.gs.stroke_comps = comps.to_vec();
            self.gs.stroke_rgb = rgb;
            self.gs.stroke_is_pattern = false;
        } else {
            self.gs.fill_cs = cs;
            self.gs.fill_comps = comps.to_vec();
            self.gs.fill_rgb = rgb;
            self.gs.fill_is_pattern = false;
        }
    }

    /// `sc`/`scn`: numeric components in the current space, or a pattern name.
    fn set_color_components(&mut self, stroke: bool, args: &[Obj]) {
        let cs = if stroke { self.gs.stroke_cs.clone() } else { self.gs.fill_cs.clone() };

        if let Some(_name) = args.last().and_then(|o| o.as_name()) {
            // Pattern colour. Until patterns are evaluated, remember that this
            // is a pattern so painting is skipped rather than filled black.
            if stroke {
                self.gs.stroke_is_pattern = true;
            } else {
                self.gs.fill_is_pattern = true;
            }
            return;
        }

        let comps: Vec<f32> = args.iter().filter_map(|o| o.as_f32()).collect();
        if comps.is_empty() {
            return;
        }
        let rgb = cs.to_rgb(&comps);
        if stroke {
            self.gs.stroke_comps = comps;
            self.gs.stroke_rgb = rgb;
            self.gs.stroke_is_pattern = false;
        } else {
            self.gs.fill_comps = comps;
            self.gs.fill_rgb = rgb;
            self.gs.fill_is_pattern = false;
        }
    }

    fn apply_ext_gstate(&mut self, name: &str, resources: &Obj) {
        let table = match self.file.oget(resources, "ExtGState") {
            Some(t) => t,
            None => return,
        };
        let gs = match table.as_dict().and_then(|d| self.file.dget(d, name)) {
            Some(g) => g,
            None => return,
        };
        let d = match gs.as_dict() {
            Some(d) => d,
            None => return,
        };
        if let Some(v) = self.file.dget(d, "LW").and_then(|o| o.as_f64()) {
            self.gs.line_width = v.max(0.0);
        }
        if let Some(v) = self.file.dget(d, "LC").and_then(|o| o.as_i64()) {
            self.gs.line_cap = match v {
                1 => LineCap::Round,
                2 => LineCap::Square,
                _ => LineCap::Butt,
            };
        }
        if let Some(v) = self.file.dget(d, "LJ").and_then(|o| o.as_i64()) {
            self.gs.line_join = match v {
                1 => LineJoin::Round,
                2 => LineJoin::Bevel,
                _ => LineJoin::Miter,
            };
        }
        if let Some(v) = self.file.dget(d, "ML").and_then(|o| o.as_f64()) {
            self.gs.miter_limit = v.max(1.0);
        }
        if let Some(arr) = self.file.dget(d, "D").and_then(|o| o.as_array().map(|a| a.to_vec())) {
            if let Some(pattern) = arr.first().and_then(|o| self.file.resolve(o).as_array().map(|a| a.to_vec())) {
                self.gs.dash = pattern.iter().filter_map(|v| v.as_f64()).filter(|v| *v >= 0.0).collect();
            }
            self.gs.dash_phase = arr.get(1).and_then(|o| o.as_f64()).unwrap_or(0.0).max(0.0);
        }
        if let Some(v) = self.file.dget(d, "ca").and_then(|o| o.as_f64()) {
            self.gs.fill_alpha = v.clamp(0.0, 1.0) as f32;
        }
        if let Some(v) = self.file.dget(d, "CA").and_then(|o| o.as_f64()) {
            self.gs.stroke_alpha = v.clamp(0.0, 1.0) as f32;
        }
    }

    // ── XObjects ─────────────────────────────────────────────────────────────

    fn do_xobject(&mut self, name: &str, resources: &Obj, depth: usize) {
        let table = match self.file.oget(resources, "XObject") {
            Some(t) => t,
            None => return,
        };
        let entry = match table.as_dict().and_then(|d| d.get(name)) {
            Some(e) => e.clone(),
            None => return,
        };
        let obj_num = entry.as_ref_id().map(|(n, _)| n);
        let xobj = self.file.resolve(&entry);
        let stream = match xobj.as_stream() {
            Some(s) => s,
            None => return,
        };
        let subtype = stream.dict.get("Subtype").and_then(|o| o.as_name()).unwrap_or("");

        // A form whose optional-content group is off is skipped entirely.
        if let Some(oc) = stream.dict.get("OC") {
            if self.oc_ref_hidden(oc) {
                return;
            }
        }

        match subtype {
            "Form" => {
                if let Some(num) = obj_num {
                    if !self.active_forms.insert(num) {
                        return; // already executing: cycle
                    }
                }
                self.run_form(stream, resources, depth);
                if let Some(num) = obj_num {
                    self.active_forms.remove(&num);
                }
            }
            "Image" => self.draw_image_xobject(stream, resources),
            _ => {}
        }
    }

    fn draw_image_xobject(&mut self, stream: &super::object::Stream, resources: &Obj) {
        if !self.painting {
            return;
        }
        let bitmap = match super::image::decode(self.file, stream, resources, self.gs.fill_rgb) {
            Some(b) => b,
            None => return,
        };
        // The image operator maps the unit square onto the CTM. Sampling is
        // always filtered, matching what browser viewers do; `draw_image`
        // switches to a box filter on heavy downscales by itself.
        self.canvas
            .draw_image(&bitmap, &self.gs.ctm, &self.gs.clip, self.gs.fill_alpha, true);
    }

    fn run_form(&mut self, stream: &super::object::Stream, parent_res: &Obj, depth: usize) {
        let data = match self.file.stream_data_of(stream) {
            Some(d) => d,
            None => return,
        };
        let saved = self.gs.clone();
        let saved_stack_len = self.stack.len();

        if let Some(m) = stream.dict.get("Matrix").and_then(|o| self.file.resolve(o).as_array().map(|a| a.to_vec())) {
            if m.len() >= 6 {
                let v: Vec<f64> = m.iter().map(|o| o.as_f64().unwrap_or(0.0)).collect();
                let fm = Transform::new(v[0], v[1], v[2], v[3], v[4], v[5]);
                self.gs.ctm = fm.then(&self.gs.ctm);
            }
        }
        if let Some(bbox) = stream
            .dict
            .get("BBox")
            .map(|o| self.file.resolve(o))
            .as_ref()
            .and_then(super::object::rect_from)
        {
            let mut p = Path::new();
            p.rect(bbox[0], bbox[1], bbox[2] - bbox[0], bbox[3] - bbox[1]);
            let device = p.transformed(&self.gs.ctm);
            self.gs.clip = self.gs.clip.intersect_path(&device, FillRule::NonZero);
        }

        let res = self
            .file
            .dget(&stream.dict, "Resources")
            .filter(|r| r.as_dict().is_some())
            .unwrap_or_else(|| parent_res.clone());

        // A form's own path state must not leak into the caller.
        let saved_path = std::mem::take(&mut self.path);
        let saved_pending = self.pending_clip.take();
        self.exec(&data, &res, depth + 1);
        self.path = saved_path;
        self.pending_clip = saved_pending;

        self.stack.truncate(saved_stack_len);
        self.gs = saved;
    }

    // ── inline images ────────────────────────────────────────────────────────

    /// Draws `BI … ID <samples> EI` and returns the offset just past `EI`.
    fn inline_image(&mut self, content: &[u8], start: usize, resources: &Obj) -> usize {
        let (stream, after) = match super::image::parse_inline(self.file, content, start, resources)
        {
            Some(v) => v,
            None => return content.len(),
        };
        if !self.painting {
            return after;
        }
        if let Some(bitmap) =
            super::image::decode(self.file, &stream, resources, self.gs.fill_rgb)
        {
            self.canvas
                .draw_image(&bitmap, &self.gs.ctm, &self.gs.clip, self.gs.fill_alpha, true);
        }
        after
    }

    // ── optional content ─────────────────────────────────────────────────────

    fn is_hidden_oc(&self, arg: Option<&Obj>, resources: &Obj) -> bool {
        let arg = match arg {
            Some(a) => a,
            None => return false,
        };
        // Either an inline dictionary or a name into /Properties.
        if let Some(name) = arg.as_name() {
            let props = match self.file.oget(resources, "Properties") {
                Some(p) => p,
                None => return false,
            };
            if let Some(entry) = props.as_dict().and_then(|d| d.get(name)) {
                return self.oc_ref_hidden(entry);
            }
            return false;
        }
        self.oc_ref_hidden(arg)
    }

    /// True when the referenced OCG (or every member of an OCMD) is off.
    fn oc_ref_hidden(&self, obj: &Obj) -> bool {
        if let Some(id) = obj.as_ref_id() {
            if self.hidden_ocgs.contains(&id) {
                return true;
            }
        }
        let resolved = self.file.resolve(obj);
        let d = match resolved.as_dict() {
            Some(d) => d,
            None => return false,
        };
        if d.get("Type").and_then(|o| o.as_name()) == Some("OCMD") {
            if let Some(ocgs) = d.get("OCGs") {
                return match ocgs {
                    Obj::Array(a) => {
                        !a.is_empty()
                            && a.iter().all(|o| {
                                o.as_ref_id().map(|id| self.hidden_ocgs.contains(&id)).unwrap_or(false)
                            })
                    }
                    other => other
                        .as_ref_id()
                        .map(|id| self.hidden_ocgs.contains(&id))
                        .unwrap_or(false),
                };
            }
        }
        false
    }
}

/// Reads `/OCProperties /D /OFF` from the catalog: the groups that start
/// hidden in the default configuration.
fn collect_hidden_ocgs(file: &PdfFile) -> HashSet<(u32, u16)> {
    let mut out = HashSet::new();
    let catalog = match file.catalog_ref() {
        Some(c) => c,
        None => return out,
    };
    let props = match file.oget(&catalog, "OCProperties") {
        Some(p) => p,
        None => return out,
    };
    let d = match file.oget(&props, "D") {
        Some(d) => d,
        None => return out,
    };
    if let Some(off) = d.get("OFF").map(|o| file.resolve(o)) {
        if let Some(a) = off.as_array() {
            for item in a {
                if let Some(id) = item.as_ref_id() {
                    out.insert(id);
                }
            }
        }
    }
    // BaseState /OFF hides everything except the explicit /ON list.
    if d.get("BaseState").and_then(|o| o.as_name()) == Some("OFF") {
        let on: HashSet<(u32, u16)> = d
            .get("ON")
            .map(|o| file.resolve(o))
            .and_then(|o| o.as_array().map(|a| a.iter().filter_map(|i| i.as_ref_id()).collect()))
            .unwrap_or_default();
        if let Some(all) = props.get("OCGs").map(|o| file.resolve(o)) {
            if let Some(a) = all.as_array() {
                for item in a {
                    if let Some(id) = item.as_ref_id() {
                        if !on.contains(&id) {
                            out.insert(id);
                        }
                    }
                }
            }
        }
    }
    out
}

/// Base transform mapping PDF user space to device pixels for one page:
/// flips the y axis, shifts the crop box to the origin, applies `/Rotate`,
/// and scales to the requested pixel size.
pub fn page_ctm(crop: [f64; 4], rotate: i32, out_w: usize, out_h: usize) -> Transform {
    let bw = (crop[2] - crop[0]).max(1.0);
    let bh = (crop[3] - crop[1]).max(1.0);
    // Size after rotation, in points.
    let (rw, rh) = if rotate == 90 || rotate == 270 { (bh, bw) } else { (bw, bh) };
    let sx = out_w as f64 / rw;
    let sy = out_h as f64 / rh;

    // Move the crop box origin to (0,0).
    let m = Transform::translate(-crop[0], -crop[1]);
    // Flip y: PDF origin is bottom-left, the canvas is top-left.
    let m = m.then(&Transform::new(1.0, 0.0, 0.0, -1.0, 0.0, bh));
    // Rotate clockwise about the page, keeping content in the positive quadrant.
    let m = match rotate {
        90 => m.then(&Transform::new(0.0, 1.0, -1.0, 0.0, bh, 0.0)),
        180 => m.then(&Transform::new(-1.0, 0.0, 0.0, -1.0, bw, bh)),
        270 => m.then(&Transform::new(0.0, -1.0, 1.0, 0.0, 0.0, bw)),
        _ => m,
    };
    m.then(&Transform::scale(sx, sy))
}

/// Renders one page into a fresh canvas.
///
/// `apply_rotation` bakes `/Rotate` into the output. The viewer wants it left
/// out, because it reports the rotation separately and turns the canvas with
/// CSS; standalone callers that just want a picture want it applied.
pub fn render_page(
    file: &PdfFile,
    page: &super::page::Page,
    out_w: usize,
    out_h: usize,
    apply_rotation: bool,
) -> Canvas {
    let mut canvas = Canvas::new(out_w.max(1), out_h.max(1));
    canvas.clear([255, 255, 255]);
    let rotate = if apply_rotation { page.rotate } else { 0 };
    let ctm = page_ctm(page.crop, rotate, out_w.max(1), out_h.max(1));
    let content = super::page::content_bytes(file, page);
    if content.is_empty() {
        return canvas;
    }
    let resources = page.resources.clone();
    let mut r = Renderer::new(file, &mut canvas, ctm);
    r.run(&content, &resources);
    canvas
}

/// Convenience for callers that only need the raw pixels.
pub fn render_page_rgba(
    file: &PdfFile,
    page: &super::page::Page,
    out_w: usize,
    out_h: usize,
    apply_rotation: bool,
) -> Vec<u8> {
    render_page(file, page, out_w, out_h, apply_rotation).data
}

/// Runs the page for its text only, and returns the viewer's layout structure.
///
/// Nothing is painted: the canvas is a one-pixel stub, the CTM maps user space
/// straight to points, and only the glyph collector's output is kept. Rotation
/// is left out to match `render_page` in viewer mode.
pub fn layout_page(file: &PdfFile, page: &super::page::Page) -> crate::render::LpPage {
    let (w_pt, h_pt) = page.size_unrotated();
    // One device unit per point, so collected positions are already in points.
    let ctm = points_ctm(page.crop);
    let mut canvas = Canvas::new(1, 1);
    let content = super::page::content_bytes(file, page);

    let mut collector = super::text::Collector::new();
    if !content.is_empty() {
        let resources = page.resources.clone();
        let mut r = Renderer::new(file, &mut canvas, ctm);
        r.painting = false;
        r.collector = Some(std::mem::take(&mut collector));
        r.run(&content, &resources);
        collector = r.collector.take().unwrap_or_default();
    }
    collector.layout_page(w_pt, h_pt)
}

/// User space to page points: origin at the crop box's top-left, y downward.
pub fn points_ctm(crop: [f64; 4]) -> Transform {
    let bh = (crop[3] - crop[1]).max(1.0);
    Transform::translate(-crop[0], -crop[1]).then(&Transform::new(1.0, 0.0, 0.0, -1.0, 0.0, bh))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_ctm_maps_corners() {
        // A 200x100 pt page rendered at 400x200 px: PDF (0,0) is bottom-left,
        // so it must land at device (0, 200).
        let m = page_ctm([0.0, 0.0, 200.0, 100.0], 0, 400, 200);
        let (x, y) = m.apply(0.0, 0.0);
        assert!((x - 0.0).abs() < 1e-6 && (y - 200.0).abs() < 1e-6, "got {x},{y}");
        let (x, y) = m.apply(200.0, 100.0);
        assert!((x - 400.0).abs() < 1e-6 && (y - 0.0).abs() < 1e-6, "got {x},{y}");
    }

    #[test]
    fn page_ctm_honors_crop_origin() {
        // Crop box not at the origin: its lower-left corner maps to device
        // bottom-left.
        let m = page_ctm([10.0, 20.0, 110.0, 120.0], 0, 100, 100);
        let (x, y) = m.apply(10.0, 20.0);
        assert!((x - 0.0).abs() < 1e-6 && (y - 100.0).abs() < 1e-6, "got {x},{y}");
    }

    #[test]
    fn page_ctm_rotation_90() {
        // With /Rotate 90 the page is turned clockwise: the PDF bottom-left
        // corner ends up at the device top-left.
        let m = page_ctm([0.0, 0.0, 200.0, 100.0], 90, 100, 200);
        let (x, y) = m.apply(0.0, 0.0);
        assert!((x - 0.0).abs() < 1e-6 && (y - 0.0).abs() < 1e-6, "origin -> {x},{y}");
        let (x, y) = m.apply(200.0, 100.0);
        assert!((x - 100.0).abs() < 1e-6 && (y - 200.0).abs() < 1e-6, "far corner -> {x},{y}");
    }
}
