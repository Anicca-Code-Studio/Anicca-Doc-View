//! Native chart plotting (bar/column/line/pie/area) built on `crate::raster`.
//! Copyright (c) 2026 Anicca Code Studio. MIT licensed.
//!
//! No plotting library: axes, bars, slices and gridlines are drawn directly.

use crate::raster::{Canvas, Clip, FillRule, LineCap, LineJoin, Paint, Path, StrokeStyle, Transform};

use super::super::model::*;
use super::{draw_text_body, Ctx};

pub fn draw_chart(canvas: &mut Canvas, ctx: &Ctx, chart: &Chart, full: &Transform, ext_cx: f64, ext_cy: f64) {
    // Plot area in local EMU space, leaving margins for title/axis labels.
    let map = |lx: f64, ly: f64| full.apply(lx, ly);
    let clip = Clip::full(ctx.w, ctx.h);

    let title_h = if chart.title.is_some() { ext_cy * 0.12 } else { 0.0 };
    let axis_h = ext_cy * 0.10;
    let axis_w = ext_cx * 0.10;
    let plot_x0 = axis_w;
    let plot_y0 = title_h;
    let plot_x1 = ext_cx;
    let plot_y1 = ext_cy - axis_h;
    let plot_w = (plot_x1 - plot_x0).max(1.0);
    let plot_h = (plot_y1 - plot_y0).max(1.0);

    // Title.
    if let Some(t) = &chart.title {
        draw_label(canvas, ctx, full, 0.0, 0.0, ext_cx, title_h, t, TextAlign::Center, 14.0, [40, 40, 40], Anchor::Center);
    }

    match chart.kind {
        ChartKind::Pie => draw_pie(canvas, ctx, chart, &map, &clip, plot_x0, plot_y0, plot_w, plot_h),
        ChartKind::Line | ChartKind::Scatter => {
            draw_axes(canvas, &map, &clip, plot_x0, plot_y0, plot_x1, plot_y1);
            draw_line_chart(canvas, chart, &map, &clip, plot_x0, plot_y0, plot_w, plot_h);
        }
        ChartKind::Area => {
            draw_axes(canvas, &map, &clip, plot_x0, plot_y0, plot_x1, plot_y1);
            draw_area_chart(canvas, chart, &map, &clip, plot_x0, plot_y0, plot_w, plot_h);
        }
        _ => {
            draw_axes(canvas, &map, &clip, plot_x0, plot_y0, plot_x1, plot_y1);
            let horizontal = chart.kind == ChartKind::Bar;
            draw_bars(canvas, ctx, chart, full, &map, &clip, plot_x0, plot_y0, plot_w, plot_h, horizontal);
        }
    }
}

fn value_range(chart: &Chart) -> (f64, f64) {
    let mut min = 0.0f64;
    let mut max = 0.0f64;
    for s in &chart.series {
        for &v in &s.values {
            min = min.min(v);
            max = max.max(v);
        }
    }
    if (max - min).abs() < 1e-9 {
        max = min + 1.0;
    }
    (min, max)
}

fn n_categories(chart: &Chart) -> usize {
    chart.series.iter().map(|s| s.values.len()).max().unwrap_or(0).max(chart.categories.len())
}

fn series_color(chart: &Chart, i: usize) -> [u8; 3] {
    chart.series.get(i).and_then(|s| s.color).unwrap_or(FALLBACK[i % FALLBACK.len()])
}

const FALLBACK: [[u8; 3]; 6] = [
    [68, 114, 196],
    [237, 125, 49],
    [165, 165, 165],
    [255, 192, 0],
    [91, 155, 213],
    [112, 173, 71],
];

fn draw_axes(canvas: &mut Canvas, map: &dyn Fn(f64, f64) -> (f64, f64), clip: &Clip, x0: f64, y0: f64, x1: f64, y1: f64) {
    let mut p = Path::new();
    let (ax, ay) = map(x0, y0);
    let (bx, by) = map(x0, y1);
    let (cx, cy) = map(x1, y1);
    p.move_to(ax, ay);
    p.line_to(bx, by);
    p.line_to(cx, cy);
    stroke(canvas, &p, clip, [150, 150, 150], 1.0);
}

fn draw_bars(
    canvas: &mut Canvas,
    ctx: &Ctx,
    chart: &Chart,
    full: &Transform,
    map: &dyn Fn(f64, f64) -> (f64, f64),
    clip: &Clip,
    px0: f64,
    py0: f64,
    pw: f64,
    ph: f64,
    horizontal: bool,
) {
    let n = n_categories(chart);
    if n == 0 {
        return;
    }
    let series_n = chart.series.len().max(1);
    let (vmin, vmax) = value_range(chart);
    let span = (vmax - vmin).max(1e-9);

    if horizontal {
        let group_h = ph / n as f64;
        let bar_h = group_h * 0.8 / series_n as f64;
        for ci in 0..n {
            for (si, s) in chart.series.iter().enumerate() {
                let v = s.values.get(ci).copied().unwrap_or(0.0);
                let frac = ((v - vmin) / span).clamp(0.0, 1.0);
                let bw = pw * frac;
                let y = py0 + ci as f64 * group_h + group_h * 0.1 + si as f64 * bar_h;
                rect_fill(canvas, map, clip, px0, y, px0 + bw, y + bar_h, series_color(chart, si));
            }
            if let Some(cat) = chart.categories.get(ci) {
                draw_label(canvas, ctx, full, 0.0, py0 + ci as f64 * group_h, px0, group_h, cat, TextAlign::Right, 9.0, [90, 90, 90], Anchor::Center);
            }
        }
    } else {
        let group_w = pw / n as f64;
        let bar_w = group_w * 0.8 / series_n as f64;
        let base_frac = ((0.0 - vmin) / span).clamp(0.0, 1.0);
        for ci in 0..n {
            for (si, s) in chart.series.iter().enumerate() {
                let v = s.values.get(ci).copied().unwrap_or(0.0);
                let frac = ((v - vmin) / span).clamp(0.0, 1.0);
                let x = px0 + ci as f64 * group_w + group_w * 0.1 + si as f64 * bar_w;
                let y_top = py0 + ph * (1.0 - frac);
                let y_base = py0 + ph * (1.0 - base_frac);
                rect_fill(canvas, map, clip, x, y_top.min(y_base), x + bar_w, y_top.max(y_base), series_color(chart, si));
            }
            if let Some(cat) = chart.categories.get(ci) {
                draw_label(canvas, ctx, full, px0 + ci as f64 * group_w, py0 + ph, group_w, ph * 0.12, cat, TextAlign::Center, 9.0, [90, 90, 90], Anchor::Top);
            }
        }
    }
}

fn draw_line_chart(
    canvas: &mut Canvas,
    chart: &Chart,
    map: &dyn Fn(f64, f64) -> (f64, f64),
    clip: &Clip,
    px0: f64,
    py0: f64,
    pw: f64,
    ph: f64,
) {
    let n = n_categories(chart);
    if n < 1 {
        return;
    }
    let (vmin, vmax) = value_range(chart);
    let span = (vmax - vmin).max(1e-9);
    let step = if n > 1 { pw / (n - 1) as f64 } else { pw };
    for (si, s) in chart.series.iter().enumerate() {
        let mut p = Path::new();
        for (ci, &v) in s.values.iter().enumerate() {
            let frac = ((v - vmin) / span).clamp(0.0, 1.0);
            let x = px0 + ci as f64 * step;
            let y = py0 + ph * (1.0 - frac);
            let (dx, dy) = map(x, y);
            if ci == 0 {
                p.move_to(dx, dy);
            } else {
                p.line_to(dx, dy);
            }
        }
        stroke(canvas, &p, clip, series_color(chart, si), 2.0);
    }
}

fn draw_area_chart(
    canvas: &mut Canvas,
    chart: &Chart,
    map: &dyn Fn(f64, f64) -> (f64, f64),
    clip: &Clip,
    px0: f64,
    py0: f64,
    pw: f64,
    ph: f64,
) {
    let n = n_categories(chart);
    if n < 1 {
        return;
    }
    let (vmin, vmax) = value_range(chart);
    let span = (vmax - vmin).max(1e-9);
    let step = if n > 1 { pw / (n - 1) as f64 } else { pw };
    for (si, s) in chart.series.iter().enumerate() {
        let mut p = Path::new();
        let (sx, sy) = map(px0, py0 + ph);
        p.move_to(sx, sy);
        for (ci, &v) in s.values.iter().enumerate() {
            let frac = ((v - vmin) / span).clamp(0.0, 1.0);
            let x = px0 + ci as f64 * step;
            let y = py0 + ph * (1.0 - frac);
            let (dx, dy) = map(x, y);
            p.line_to(dx, dy);
        }
        let (ex, ey) = map(px0 + (s.values.len().saturating_sub(1)) as f64 * step, py0 + ph);
        p.line_to(ex, ey);
        p.close();
        let c = series_color(chart, si);
        canvas.fill_path(&p, FillRule::NonZero, &Paint::Solid(c), clip, 0.55);
    }
}

fn draw_pie(
    canvas: &mut Canvas,
    ctx: &Ctx,
    chart: &Chart,
    map: &dyn Fn(f64, f64) -> (f64, f64),
    clip: &Clip,
    px0: f64,
    py0: f64,
    pw: f64,
    ph: f64,
) {
    let _ = ctx;
    // Pie uses the first series' values.
    let series = match chart.series.first() {
        Some(s) => s,
        None => return,
    };
    let total: f64 = series.values.iter().map(|v| v.abs()).sum();
    if total < 1e-9 {
        return;
    }
    let cx = px0 + pw * 0.5;
    let cy = py0 + ph * 0.5;
    let r = pw.min(ph) * 0.45;
    let mut a0 = -std::f64::consts::FRAC_PI_2;
    for (i, &v) in series.values.iter().enumerate() {
        let frac = v.abs() / total;
        let a1 = a0 + frac * std::f64::consts::TAU;
        let mut p = Path::new();
        let (c0, c1) = map(cx, cy);
        p.move_to(c0, c1);
        let steps = ((a1 - a0).abs() / 0.15).ceil().max(1.0) as usize;
        for st in 0..=steps {
            let a = a0 + (a1 - a0) * st as f64 / steps as f64;
            let (dx, dy) = map(cx + r * a.cos(), cy + r * a.sin());
            p.line_to(dx, dy);
        }
        p.close();
        canvas.fill_path(&p, FillRule::NonZero, &Paint::Solid(color_for_slice(chart, i)), clip, 1.0);
        a0 = a1;
    }
}

fn color_for_slice(chart: &Chart, i: usize) -> [u8; 3] {
    // Prefer the series' single color varied by index, else fallback palette.
    if let Some(c) = chart.series.first().and_then(|s| s.color) {
        // Slightly vary lightness per slice for readability.
        let f = 1.0 - (i as f32 % 6.0) * 0.08;
        [
            (c[0] as f32 * f) as u8,
            (c[1] as f32 * f) as u8,
            (c[2] as f32 * f) as u8,
        ]
    } else {
        FALLBACK[i % FALLBACK.len()]
    }
}

// ── primitives ─────────────────────────────────────────────────────────────────

fn rect_fill(canvas: &mut Canvas, map: &dyn Fn(f64, f64) -> (f64, f64), clip: &Clip, x0: f64, y0: f64, x1: f64, y1: f64, color: [u8; 3]) {
    let mut p = Path::new();
    let (a, b) = map(x0, y0);
    let (c, d) = map(x1, y0);
    let (e, f) = map(x1, y1);
    let (g, h) = map(x0, y1);
    p.move_to(a, b);
    p.line_to(c, d);
    p.line_to(e, f);
    p.line_to(g, h);
    p.close();
    canvas.fill_path(&p, FillRule::NonZero, &Paint::Solid(color), clip, 1.0);
}

fn stroke(canvas: &mut Canvas, path: &Path, clip: &Clip, color: [u8; 3], width: f64) {
    let style = StrokeStyle {
        width,
        cap: LineCap::Round,
        join: LineJoin::Round,
        miter_limit: 4.0,
        dash: Vec::new(),
        dash_phase: 0.0,
    };
    canvas.stroke_path(path, &Transform::identity(), &style, &Paint::Solid(color), clip, 1.0);
}

#[allow(clippy::too_many_arguments)]
fn draw_label(
    canvas: &mut Canvas,
    ctx: &Ctx,
    full: &Transform,
    lx: f64,
    ly: f64,
    lw: f64,
    lh: f64,
    text: &str,
    align: TextAlign,
    size_pt: f32,
    color: [u8; 3],
    anchor: Anchor,
) {
    let mut tb = TextBody { wrap: true, font_scale: 1.0, anchor, ..Default::default() };
    tb.inset_l_pt = 1.0;
    tb.inset_r_pt = 1.0;
    tb.inset_t_pt = 0.0;
    tb.inset_b_pt = 0.0;
    let mut para = Para { align, ..Default::default() };
    para.default_size_pt = size_pt;
    para.runs.push(Run { text: text.to_string(), size_pt, color, ..Default::default() });
    tb.paragraphs.push(para);
    // Sub-transform: local label box → device.
    let sub = Transform::translate(lx, ly).then(full);
    draw_text_body(canvas, ctx, &tb, &sub, lw, lh);
}
