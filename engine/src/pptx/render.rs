//! Slide rasterization: shapes → RGBA canvas, using the in-house `crate::raster`
//! vector engine and cosmic-text for glyphs.
//! Copyright (c) 2026 Anicca Code Studio. MIT licensed.

use cosmic_text::{Align as CtAlign, Attrs, Buffer, Color, FontSystem, Metrics, Shaping, Style, SwashCache, SwashContent, Weight};

use crate::raster::{Bitmap, Canvas, Clip, FillRule, LineCap, LineJoin, Paint, PaintSource, Path, StrokeStyle, Transform};
use crate::render::resolve_family;

use super::geometry::{self, Map};
use super::model::*;
use super::theme::Theme;
use super::PptxDocument;

const LINE_FACTOR: f32 = 1.2;

/// Render a slide to an RGBA8 buffer of size w×h.
pub fn render_slide_rgba(
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    doc: &PptxDocument,
    slide_idx: usize,
    w: usize,
    h: usize,
) -> Vec<u8> {
    let mut canvas = Canvas::new(w, h);
    let slide = match doc.slides.get(slide_idx) {
        Some(s) => s,
        None => return canvas.data,
    };

    // EMU → device pixel scale.
    let sx = w as f64 / doc.slide_w_emu.max(1) as f64;
    let sy = h as f64 / doc.slide_h_emu.max(1) as f64;
    let world = Transform::new(sx, 0.0, 0.0, sy, 0.0, 0.0);
    // Device pixels per point (for font sizes, line widths given in points).
    let ppt = (sx * EMU_PER_PT) as f32;

    // Background.
    match &slide.background {
        Some(Fill::Solid { color, .. }) => canvas.clear(*color),
        Some(Fill::Gradient { stops, angle_deg, radial }) => {
            canvas.clear([255, 255, 255]);
            let path = full_rect_path(w, h);
            fill_gradient(&mut canvas, &path, stops, *angle_deg, *radial, &Clip::full(w, h));
        }
        _ => canvas.clear([255, 255, 255]),
    }

    let ctx = Ctx::new(fonts, swash, &doc.theme, ppt, w, h);
    for shape in &slide.shapes {
        draw_shape(&mut canvas, &ctx, shape, &world);
    }
    canvas.data
}

// The context bundles the mutable font resources plus immutable render params.
struct Ctx<'a> {
    fonts: *mut FontSystem,
    swash: *mut SwashCache,
    theme: &'a Theme,
    ppt: f32,
    w: usize,
    h: usize,
}

// SAFETY shim: cosmic-text needs &mut FontSystem/SwashCache while we recurse
// through shapes with shared canvas access. We hold raw pointers and reborrow
// at each glyph call; single-threaded, no aliasing of the same &mut across live
// borrows.
impl<'a> Ctx<'a> {
    fn new(fonts: &'a mut FontSystem, swash: &'a mut SwashCache, theme: &'a Theme, ppt: f32, w: usize, h: usize) -> Ctx<'a> {
        Ctx { fonts: fonts as *mut _, swash: swash as *mut _, theme, ppt, w, h }
    }
    #[allow(clippy::mut_from_ref)]
    fn fonts(&self) -> &mut FontSystem {
        unsafe { &mut *self.fonts }
    }
    #[allow(clippy::mut_from_ref)]
    fn swash(&self) -> &mut SwashCache {
        unsafe { &mut *self.swash }
    }
}

fn draw_shape(canvas: &mut Canvas, ctx: &Ctx, shape: &Shape, world: &Transform) {
    let xf = &shape.xfrm;
    if !xf.has_size() {
        // A group can still have a size via chExt only; groups handled below.
        if !matches!(shape.kind, ShapeKind::Group { .. } | ShapeKind::Diagram { .. }) {
            // Nothing to place.
        }
    }
    let ext_cx = xf.ext_cx.max(1) as f64;
    let ext_cy = xf.ext_cy.max(1) as f64;
    let shape_ctm = shape_transform(xf, ext_cx, ext_cy);
    let full = shape_ctm.then(world);

    match &shape.kind {
        ShapeKind::Group { children } | ShapeKind::Diagram { children } => {
            // Child coordinate space → current space.
            let (chx, chy, chcx, chcy) = if xf.has_ch {
                (xf.ch_off_x as f64, xf.ch_off_y as f64, xf.ch_ext_cx.max(1) as f64, xf.ch_ext_cy.max(1) as f64)
            } else {
                (0.0, 0.0, ext_cx, ext_cy)
            };
            let child_scale = Transform::new(ext_cx / chcx, 0.0, 0.0, ext_cy / chcy, 0.0, 0.0);
            let child_to_local = Transform::translate(-chx, -chy).then(&child_scale);
            let child_world = child_to_local.then(&shape_ctm).then(world);
            for c in children {
                draw_shape(canvas, ctx, c, &child_world);
            }
        }
        ShapeKind::Sp { geom, fill, line, text } => {
            let map: Box<Map> = Box::new(move |lx: f64, ly: f64| full.apply(lx, ly));
            let built = match geom {
                Geom::Custom { paths } => geometry::build_custom(paths, ext_cx, ext_cy, &*map),
                Geom::Preset { name, adj } => geometry::build_preset(name, adj, ext_cx, ext_cy, &*map),
                Geom::None => geometry::build_preset("rect", &[], ext_cx, ext_cy, &*map),
            };
            let clip = Clip::full(ctx.w, ctx.h);
            if built.closed {
                paint_fill(canvas, &built.path, fill, ctx.theme, &clip);
            }
            paint_line(canvas, &built.path, line, world, &clip);
            if let Some(tb) = text {
                draw_text_body(canvas, ctx, tb, &full, ext_cx, ext_cy);
            }
        }
        ShapeKind::Pic { image, line } => {
            let clip = Clip::full(ctx.w, ctx.h);
            if let Some(img) = image {
                draw_bitmap(canvas, img, &full, ext_cx, ext_cy, &clip);
            } else {
                // Missing/undecodable image: light placeholder box.
                let map: Box<Map> = Box::new(move |lx: f64, ly: f64| full.apply(lx, ly));
                let built = geometry::build_preset("rect", &[], ext_cx, ext_cy, &*map);
                paint_fill(canvas, &built.path, &Fill::Solid { color: [235, 235, 235], alpha: 1.0 }, ctx.theme, &clip);
            }
            let map: Box<Map> = Box::new(move |lx: f64, ly: f64| full.apply(lx, ly));
            let built = geometry::build_preset("rect", &[], ext_cx, ext_cy, &*map);
            paint_line(canvas, &built.path, line, world, &clip);
        }
        ShapeKind::Table(table) => {
            draw_table(canvas, ctx, table, &full, ext_cx, ext_cy, world);
        }
        ShapeKind::Chart(chart) => {
            chart::draw_chart(canvas, ctx, chart, &full, ext_cx, ext_cy);
        }
    }
}

/// Build the shape placement transform mapping local [0..ext] coords to the
/// current coordinate space, including rotation about the center and flips.
fn shape_transform(xf: &Xfrm, ext_cx: f64, ext_cy: f64) -> Transform {
    let cx = ext_cx * 0.5;
    let cy = ext_cy * 0.5;
    let fh = if xf.flip_h { -1.0 } else { 1.0 };
    let fv = if xf.flip_v { -1.0 } else { 1.0 };
    let theta = xf.rot as f64 / 60000.0 * std::f64::consts::PI / 180.0;
    let (c, s) = (theta.cos(), theta.sin());
    let rot = Transform::new(c, s, -s, c, 0.0, 0.0);
    Transform::translate(-cx, -cy)
        .then(&Transform::scale(fh, fv))
        .then(&rot)
        .then(&Transform::translate(xf.off_x as f64 + cx, xf.off_y as f64 + cy))
}

// ── fills / strokes ──────────────────────────────────────────────────────────────

fn paint_fill(canvas: &mut Canvas, path: &Path, fill: &Fill, _theme: &Theme, clip: &Clip) {
    match fill {
        Fill::Solid { color, alpha } => {
            canvas.fill_path(path, FillRule::NonZero, &Paint::Solid(*color), clip, *alpha);
        }
        Fill::Gradient { stops, angle_deg, radial } => {
            fill_gradient(canvas, path, stops, *angle_deg, *radial, clip);
        }
        Fill::Blip { .. } | Fill::None => {}
    }
}

fn paint_line(canvas: &mut Canvas, path: &Path, line: &Line, world: &Transform, clip: &Clip) {
    let color = match &line.fill {
        Fill::Solid { color, .. } => *color,
        _ => return,
    };
    if matches!(line.fill, Fill::None) {
        return;
    }
    let scale = world.mean_scale();
    let width_px = if line.width_emu > 0 {
        (line.width_emu as f64) * scale
    } else {
        // Default 1pt outline when a stroke color is present but no width.
        (EMU_PER_PT) * scale
    };
    let dash = match line.dash {
        DashKind::Solid => Vec::new(),
        DashKind::Dash => vec![width_px * 3.0, width_px * 2.0],
        DashKind::Dot => vec![width_px, width_px * 2.0],
        DashKind::DashDot => vec![width_px * 3.0, width_px * 2.0, width_px, width_px * 2.0],
    };
    let style = StrokeStyle {
        width: width_px.max(0.75),
        cap: LineCap::Butt,
        join: LineJoin::Miter,
        miter_limit: 4.0,
        dash,
        dash_phase: 0.0,
    };
    // Path is already in device space; stroke with identity CTM.
    canvas.stroke_path(path, &Transform::identity(), &style, &Paint::Solid(color), clip, 1.0);
}

fn full_rect_path(w: usize, h: usize) -> Path {
    let mut p = Path::new();
    p.rect(0.0, 0.0, w as f64, h as f64);
    p
}

// ── gradient ─────────────────────────────────────────────────────────────────────

struct LinearGrad {
    stops: Vec<GradStop>,
    x0: f64,
    y0: f64,
    dx: f64,
    dy: f64,
    len2: f64,
}

impl PaintSource for LinearGrad {
    fn color_at(&self, x: f64, y: f64) -> Option<([u8; 3], f32)> {
        let t = if self.len2 > 1e-9 {
            ((x - self.x0) * self.dx + (y - self.y0) * self.dy) / self.len2
        } else {
            0.0
        };
        Some(sample_stops(&self.stops, t as f32))
    }
}

struct RadialGrad {
    stops: Vec<GradStop>,
    cx: f64,
    cy: f64,
    r: f64,
}

impl PaintSource for RadialGrad {
    fn color_at(&self, x: f64, y: f64) -> Option<([u8; 3], f32)> {
        let d = ((x - self.cx).powi(2) + (y - self.cy).powi(2)).sqrt();
        let t = if self.r > 1e-9 { d / self.r } else { 0.0 };
        Some(sample_stops(&self.stops, t as f32))
    }
}

fn fill_gradient(canvas: &mut Canvas, path: &Path, stops: &[GradStop], angle_deg: f32, radial: bool, clip: &Clip) {
    if stops.is_empty() {
        return;
    }
    let bounds = match path.bounds() {
        Some(b) => b,
        None => return,
    };
    let (x0b, y0b, x1b, y1b) = bounds;
    if radial {
        let cx = (x0b + x1b) * 0.5;
        let cy = (y0b + y1b) * 0.5;
        let r = ((x1b - x0b).max(y1b - y0b)) * 0.5;
        let src = RadialGrad { stops: stops.to_vec(), cx, cy, r };
        canvas.fill_path(path, FillRule::NonZero, &Paint::Source(&src), clip, 1.0);
    } else {
        let ang = angle_deg as f64 * std::f64::consts::PI / 180.0;
        let (dx, dy) = (ang.cos(), ang.sin());
        // Project the bounding box onto the direction to find start point + length.
        let corners = [(x0b, y0b), (x1b, y0b), (x0b, y1b), (x1b, y1b)];
        let mut min_p = f64::INFINITY;
        let mut max_p = f64::NEG_INFINITY;
        for (x, y) in corners {
            let p = x * dx + y * dy;
            min_p = min_p.min(p);
            max_p = max_p.max(p);
        }
        let x0 = dx * min_p;
        let y0 = dy * min_p;
        let len = (max_p - min_p).max(1.0);
        let src = LinearGrad { stops: stops.to_vec(), x0, y0, dx, dy, len2: len };
        canvas.fill_path(path, FillRule::NonZero, &Paint::Source(&src), clip, 1.0);
    }
}

fn sample_stops(stops: &[GradStop], t: f32) -> ([u8; 3], f32) {
    let t = t.clamp(0.0, 1.0);
    if t <= stops[0].pos {
        return (stops[0].color, stops[0].alpha);
    }
    if t >= stops[stops.len() - 1].pos {
        let s = &stops[stops.len() - 1];
        return (s.color, s.alpha);
    }
    for pair in stops.windows(2) {
        let a = &pair[0];
        let b = &pair[1];
        if t >= a.pos && t <= b.pos {
            let span = (b.pos - a.pos).max(1e-6);
            let f = (t - a.pos) / span;
            let col = [
                lerp(a.color[0], b.color[0], f),
                lerp(a.color[1], b.color[1], f),
                lerp(a.color[2], b.color[2], f),
            ];
            let alpha = a.alpha + (b.alpha - a.alpha) * f;
            return (col, alpha);
        }
    }
    (stops[0].color, stops[0].alpha)
}

fn lerp(a: u8, b: u8, f: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * f).round().clamp(0.0, 255.0) as u8
}

// ── images ───────────────────────────────────────────────────────────────────────

fn draw_bitmap(canvas: &mut Canvas, img: &ImageData, full: &Transform, ext_cx: f64, ext_cy: f64, clip: &Clip) {
    if img.w == 0 || img.h == 0 {
        return;
    }
    let bmp = Bitmap { w: img.w, h: img.h, data: img.rgba.clone() };
    // Map the image unit square to the shape rectangle. draw_image treats row 0
    // as v=1 (top), so map v=1 → local y=0.
    let unit_to_local = Transform::new(ext_cx, 0.0, 0.0, -ext_cy, 0.0, ext_cy);
    let ctm = unit_to_local.then(full);
    canvas.draw_image(&bmp, &ctm, clip, 1.0, true);
}

// ── text ─────────────────────────────────────────────────────────────────────────

fn ct_align(a: TextAlign) -> Option<CtAlign> {
    match a {
        TextAlign::Left => Some(CtAlign::Left),
        TextAlign::Center => Some(CtAlign::Center),
        TextAlign::Right => Some(CtAlign::Right),
        TextAlign::Justify => Some(CtAlign::Justified),
    }
}

fn run_attrs<'r>(run: &'r Run, theme: &Theme, ppt: f32, font_scale: f32) -> (Attrs<'r>, f32) {
    let size_px = (run.size_pt * ppt * font_scale).max(1.0);
    let _ = theme;
    let family = match &run.font {
        Some(f) => resolve_family(f),
        None => cosmic_text::Family::SansSerif,
    };
    let mut a = Attrs::new().family(family);
    if run.bold {
        a = a.weight(Weight::BOLD);
    }
    if run.italic {
        a = a.style(Style::Italic);
    }
    a = a.color(Color::rgb(run.color[0], run.color[1], run.color[2]));
    a = a.metrics(Metrics::new(size_px, size_px * LINE_FACTOR));
    (a, size_px)
}

/// Draw a text body inside the shape box. Text is rendered axis-aligned using
/// the box's device bounding rectangle (rotation of the box is not applied to
/// glyphs in this version).
fn draw_text_body(canvas: &mut Canvas, ctx: &Ctx, tb: &TextBody, full: &Transform, ext_cx: f64, ext_cy: f64) {
    // Device bounding rect of the shape box.
    let corners = [full.apply(0.0, 0.0), full.apply(ext_cx, 0.0), full.apply(0.0, ext_cy), full.apply(ext_cx, ext_cy)];
    let (mut bx0, mut by0, mut bx1, mut by1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for (x, y) in corners {
        bx0 = bx0.min(x);
        by0 = by0.min(y);
        bx1 = bx1.max(x);
        by1 = by1.max(y);
    }
    let ppt = ctx.ppt;
    let il = tb.inset_l_pt * ppt;
    let ir = tb.inset_r_pt * ppt;
    let it = tb.inset_t_pt * ppt;
    let ib = tb.inset_b_pt * ppt;
    let content_x = bx0 as f32 + il;
    let content_y = by0 as f32 + it;
    let content_w = ((bx1 - bx0) as f32 - il - ir).max(1.0);
    let content_h = ((by1 - by0) as f32 - it - ib).max(1.0);

    let fonts = ctx.fonts();

    // Each paragraph is split into visual lines at embedded '\n'; every visual
    // line becomes its own buffer (cosmic-text mishandles embedded newlines in
    // a rich-text span, so we never feed it one). Long lines still wrap.
    struct Laid {
        buffer: Buffer,
        height: f32,
        space_before: f32,
        space_after: f32,
    }
    let wrap_w = if tb.wrap { content_w } else { 100000.0 };
    let mut laid: Vec<Laid> = Vec::new();
    for para in &tb.paragraphs {
        let lines = split_visual_lines(para);
        for (li, line_runs) in lines.iter().enumerate() {
            let buf = build_line_buffer(fonts, para, line_runs, ctx.theme, ppt, tb.font_scale, wrap_w, li == 0);
            let height = buffer_height(&buf);
            laid.push(Laid {
                buffer: buf,
                height,
                space_before: if li == 0 { para.space_before_pt * ppt } else { 0.0 },
                space_after: if li == lines.len() - 1 { para.space_after_pt * ppt } else { 0.0 },
            });
        }
    }

    let total_h: f32 = laid.iter().map(|l| l.height + l.space_before + l.space_after).sum();
    let mut cursor_y = match tb.anchor {
        Anchor::Top => content_y,
        Anchor::Center => content_y + ((content_h - total_h) * 0.5).max(0.0),
        Anchor::Bottom => content_y + (content_h - total_h).max(0.0),
    };

    let swash = ctx.swash();
    for l in &laid {
        cursor_y += l.space_before;
        for run in l.buffer.layout_runs() {
            let baseline = cursor_y + run.line_y;
            for glyph in run.glyphs.iter() {
                let phys = glyph.physical((0.0, 0.0), 1.0);
                let color = glyph.color_opt.unwrap_or(Color::rgb(0, 0, 0));
                let pen_x = content_x + phys.x as f32;
                let pen_y = baseline + phys.y as f32;
                if let Some(img) = swash.get_image(fonts, phys.cache_key) {
                    blit_glyph(canvas, img, pen_x, pen_y, color);
                }
            }
        }
        cursor_y += l.height + l.space_after;
    }
}

/// Split a paragraph's runs into visual lines at embedded newlines. A run whose
/// text spans multiple lines is broken across the boundary, preserving style.
fn split_visual_lines(para: &Para) -> Vec<Vec<Run>> {
    let mut lines: Vec<Vec<Run>> = vec![Vec::new()];
    for run in &para.runs {
        let normalized = run.text.replace("\r\n", "\n").replace('\r', "\n").replace('\u{b}', "\n");
        let parts: Vec<&str> = normalized.split('\n').collect();
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                lines.push(Vec::new());
            }
            if !part.is_empty() {
                let mut r = run.clone();
                r.text = part.to_string();
                lines.last_mut().unwrap().push(r);
            }
        }
    }
    lines
}

fn build_line_buffer(
    fonts: &mut FontSystem,
    para: &Para,
    line_runs: &[Run],
    theme: &Theme,
    ppt: f32,
    font_scale: f32,
    width_px: f32,
    first_line: bool,
) -> Buffer {
    // Line height comes from the actual runs on this line; the paragraph
    // default only applies to an empty line (blank paragraph spacer).
    let runs_max = line_runs.iter().map(|r| r.size_pt).fold(0.0f32, f32::max);
    let base_pt = if runs_max > 0.0 { runs_max } else { para.default_size_pt };
    let base_size = base_pt.max(1.0) * ppt * font_scale;
    let mut buffer = Buffer::new(fonts, Metrics::new(base_size, base_size * LINE_FACTOR));
    buffer.set_size(fonts, Some(width_px.max(1.0)), None);

    let default_attrs = Attrs::new();
    let mut owned: Vec<(String, Attrs)> = Vec::new();

    if first_line {
        if let Some(b) = &para.bullet {
            let (mut a, _) = line_runs
                .iter()
                .find(|r| !r.text.is_empty())
                .map(|r| run_attrs(r, theme, ppt, font_scale))
                .unwrap_or((Attrs::new(), base_size));
            if let Some(c) = para.bullet_color {
                a = a.color(Color::rgb(c[0], c[1], c[2]));
            }
            owned.push((format!("{}  ", b), a));
        }
    }

    for run in line_runs {
        if run.text.is_empty() {
            continue;
        }
        let (a, _) = run_attrs(run, theme, ppt, font_scale);
        owned.push((run.text.clone(), a));
    }
    if owned.is_empty() {
        owned.push((" ".to_string(), default_attrs.clone()));
    }

    buffer.set_rich_text(
        fonts,
        owned.iter().map(|(t, a)| (t.as_str(), a.clone())),
        default_attrs.clone(),
        Shaping::Advanced,
    );
    let align = ct_align(para.align);
    for line in buffer.lines.iter_mut() {
        line.set_align(align);
    }
    buffer.shape_until_scroll(fonts, false);
    buffer
}

fn buffer_height(buffer: &Buffer) -> f32 {
    let lh = buffer.metrics().line_height;
    (buffer.layout_runs().count() as f32 * lh).max(lh)
}

fn blit_glyph(canvas: &mut Canvas, img: &cosmic_text::SwashImage, pen_x: f32, pen_y: f32, color: Color) {
    let pw = img.placement.width as i32;
    let ph = img.placement.height as i32;
    if pw <= 0 || ph <= 0 {
        return;
    }
    let x0 = pen_x.round() as i32 + img.placement.left;
    let y0 = pen_y.round() as i32 - img.placement.top;
    match img.content {
        SwashContent::Mask | SwashContent::SubpixelMask => {
            for j in 0..ph {
                for i in 0..pw {
                    let a = img.data[(j * pw + i) as usize];
                    if a == 0 {
                        continue;
                    }
                    put(canvas, x0 + i, y0 + j, [color.r(), color.g(), color.b()], a as f32 / 255.0);
                }
            }
        }
        SwashContent::Color => {
            for j in 0..ph {
                for i in 0..pw {
                    let idx = ((j * pw + i) * 4) as usize;
                    let a = img.data[idx + 3];
                    if a == 0 {
                        continue;
                    }
                    put(canvas, x0 + i, y0 + j, [img.data[idx], img.data[idx + 1], img.data[idx + 2]], a as f32 / 255.0);
                }
            }
        }
    }
}

#[inline]
fn put(canvas: &mut Canvas, x: i32, y: i32, color: [u8; 3], alpha: f32) {
    if x < 0 || y < 0 || x >= canvas.w as i32 || y >= canvas.h as i32 {
        return;
    }
    canvas.blend(x as usize, y as usize, color, alpha);
}

// ── table ────────────────────────────────────────────────────────────────────────

fn draw_table(canvas: &mut Canvas, ctx: &Ctx, table: &Table, full: &Transform, ext_cx: f64, ext_cy: f64, world: &Transform) {
    let total_col: f64 = table.col_widths.iter().map(|w| *w as f64).sum::<f64>().max(1.0);
    // Column x positions in local EMU space.
    let mut col_x: Vec<f64> = Vec::with_capacity(table.col_widths.len() + 1);
    let mut acc = 0.0;
    let scale_x = ext_cx / total_col;
    for w in &table.col_widths {
        col_x.push(acc);
        acc += *w as f64 * scale_x;
    }
    col_x.push(acc);

    let total_row: f64 = table.rows.iter().map(|r| r.height_emu.max(1) as f64).sum::<f64>().max(1.0);
    let scale_y = ext_cy / total_row;
    let mut row_y: Vec<f64> = Vec::with_capacity(table.rows.len() + 1);
    let mut accy = 0.0;
    for r in &table.rows {
        row_y.push(accy);
        accy += r.height_emu.max(1) as f64 * scale_y;
    }
    row_y.push(accy);

    let clip = Clip::full(ctx.w, ctx.h);
    for (ri, row) in table.rows.iter().enumerate() {
        let mut ci = 0usize;
        for cell in &row.cells {
            if cell.h_merge || cell.v_merge {
                ci += 1;
                continue;
            }
            let span = cell.grid_span.max(1) as usize;
            let c0 = ci.min(col_x.len() - 1);
            let c1 = (ci + span).min(col_x.len() - 1);
            let x0 = col_x[c0];
            let x1 = col_x[c1];
            let y0 = row_y[ri.min(row_y.len() - 1)];
            let rspan = cell.row_span.max(1) as usize;
            let y1 = row_y[(ri + rspan).min(row_y.len() - 1)];

            let map: Box<Map> = Box::new(move |lx: f64, ly: f64| full.apply(lx, ly));
            let mut cellpath = Path::new();
            let (px0, py0) = map(x0, y0);
            let (px1, py0b) = map(x1, y0);
            let (px1b, py1) = map(x1, y1);
            let (px0b, py1b) = map(x0, y1);
            cellpath.move_to(px0, py0);
            cellpath.line_to(px1, py0b);
            cellpath.line_to(px1b, py1);
            cellpath.line_to(px0b, py1b);
            cellpath.close();

            paint_fill(canvas, &cellpath, &cell.fill, ctx.theme, &clip);

            // Borders.
            draw_border(canvas, &cell.border_t, map_line(&map, x0, y0, x1, y0), world, &clip);
            draw_border(canvas, &cell.border_b, map_line(&map, x0, y1, x1, y1), world, &clip);
            draw_border(canvas, &cell.border_l, map_line(&map, x0, y0, x0, y1), world, &clip);
            draw_border(canvas, &cell.border_r, map_line(&map, x1, y0, x1, y1), world, &clip);

            // Cell text: build a sub-transform whose local box is the cell.
            let cell_w = x1 - x0;
            let cell_h = y1 - y0;
            let cell_full = Transform::translate(x0, y0).then(full);
            draw_text_body(canvas, ctx, &cell.text, &cell_full, cell_w, cell_h);

            ci += span;
        }
    }
}

fn map_line(map: &Map, x0: f64, y0: f64, x1: f64, y1: f64) -> Path {
    let mut p = Path::new();
    let (a, b) = map(x0, y0);
    let (c, d) = map(x1, y1);
    p.move_to(a, b);
    p.line_to(c, d);
    p
}

fn draw_border(canvas: &mut Canvas, line: &Line, path: Path, world: &Transform, clip: &Clip) {
    if matches!(line.fill, Fill::None) {
        return;
    }
    paint_line(canvas, &path, line, world, clip);
}

pub mod chart;
