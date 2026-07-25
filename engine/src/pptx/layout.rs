//! Text-layer layout: slide → `LpPage` for selection/search/accessibility.
//! Copyright (c) 2026 Anicca Code Studio. MIT licensed.
//!
//! One `LpFrame` is emitted per text box (and per table cell), positioned in
//! points. Each visual line becomes a `runList` line whose glyph advances come
//! from cosmic-text, matching the raster path's line splitting. Rotation is not
//! applied to the text layer (consistent with the raster renderer).

use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style, Weight};

use crate::raster::Transform;
use crate::render::{
    resolve_family, LpFrame, LpGlyph, LpLine, LpLineContent, LpPage, LpParcel, LpRun, LpRunContent,
    LpRunList, LpTransform,
};

use super::model::*;
use super::PptxDocument;

const LINE_FACTOR: f32 = 1.2;

pub fn layout_slide(fonts: &mut FontSystem, doc: &PptxDocument, slide_idx: usize) -> LpPage {
    let (w_pt, h_pt) = doc.slide_size_pt();
    let mut frames: Vec<LpFrame> = Vec::new();
    let slide = match doc.slides.get(slide_idx) {
        Some(s) => s,
        None => return LpPage { width: w_pt, height: h_pt, frames },
    };
    // Point-space world transform: EMU → points.
    let world = Transform::scale(1.0 / EMU_PER_PT, 1.0 / EMU_PER_PT);
    for shape in &slide.shapes {
        collect_frames(fonts, shape, &world, &mut frames);
    }
    LpPage { width: w_pt, height: h_pt, frames }
}

fn collect_frames(fonts: &mut FontSystem, shape: &Shape, world: &Transform, out: &mut Vec<LpFrame>) {
    let xf = &shape.xfrm;
    let ext_cx = xf.ext_cx.max(1) as f64;
    let ext_cy = xf.ext_cy.max(1) as f64;
    // Placement transform (ignoring rotation for the text layer).
    let place = Transform::translate(xf.off_x as f64, xf.off_y as f64);
    let full = place.then(world);

    match &shape.kind {
        ShapeKind::Group { children } | ShapeKind::Diagram { children } => {
            let (chx, chy, chcx, chcy) = if xf.has_ch {
                (xf.ch_off_x as f64, xf.ch_off_y as f64, xf.ch_ext_cx.max(1) as f64, xf.ch_ext_cy.max(1) as f64)
            } else {
                (0.0, 0.0, ext_cx, ext_cy)
            };
            let child_scale = Transform::new(ext_cx / chcx, 0.0, 0.0, ext_cy / chcy, 0.0, 0.0);
            let child_world = Transform::translate(-chx, -chy).then(&child_scale).then(&place).then(world);
            for c in children {
                collect_frames(fonts, c, &child_world, out);
            }
        }
        ShapeKind::Sp { text: Some(tb), .. } => {
            if let Some(frame) = text_frame(fonts, tb, &full, ext_cx, ext_cy) {
                out.push(frame);
            }
        }
        ShapeKind::Table(table) => {
            collect_table_frames(fonts, table, &full, ext_cx, ext_cy, out);
        }
        _ => {}
    }
}

fn collect_table_frames(
    fonts: &mut FontSystem,
    table: &Table,
    full: &Transform,
    ext_cx: f64,
    ext_cy: f64,
    out: &mut Vec<LpFrame>,
) {
    let total_col: f64 = table.col_widths.iter().map(|w| *w as f64).sum::<f64>().max(1.0);
    let scale_x = ext_cx / total_col;
    let mut col_x = vec![0.0f64];
    let mut acc = 0.0;
    for w in &table.col_widths {
        acc += *w as f64 * scale_x;
        col_x.push(acc);
    }
    let total_row: f64 = table.rows.iter().map(|r| r.height_emu.max(1) as f64).sum::<f64>().max(1.0);
    let scale_y = ext_cy / total_row;
    let mut row_y = vec![0.0f64];
    let mut accy = 0.0;
    for r in &table.rows {
        accy += r.height_emu.max(1) as f64 * scale_y;
        row_y.push(accy);
    }
    for (ri, row) in table.rows.iter().enumerate() {
        let mut ci = 0usize;
        for cell in &row.cells {
            if cell.h_merge || cell.v_merge {
                ci += 1;
                continue;
            }
            let span = cell.grid_span.max(1) as usize;
            let c0 = ci.min(col_x.len() - 1);
            let x0 = col_x[c0];
            let y0 = row_y[ri.min(row_y.len() - 1)];
            let cell_full = Transform::translate(x0, y0).then(full);
            let cw = col_x[(ci + span).min(col_x.len() - 1)] - x0;
            let ch = row_y[(ri + 1).min(row_y.len() - 1)] - y0;
            if let Some(frame) = text_frame(fonts, &cell.text, &cell_full, cw, ch) {
                out.push(frame);
            }
            ci += span;
        }
    }
}

/// Build a text-layer frame for a text body positioned by `full` (EMU→pt),
/// whose local box is ext_cx × ext_cy EMU.
fn text_frame(fonts: &mut FontSystem, tb: &TextBody, full: &Transform, ext_cx: f64, ext_cy: f64) -> Option<LpFrame> {
    if tb.paragraphs.iter().all(|p| p.runs.iter().all(|r| r.text.trim().is_empty())) {
        return None;
    }
    // Content box origin/size in points.
    let (ox, oy) = full.apply(0.0, 0.0);
    let box_w_pt = (ext_cx / EMU_PER_PT) as f32;
    let box_h_pt = (ext_cy / EMU_PER_PT) as f32;
    let content_x = ox as f32 + tb.inset_l_pt;
    let content_y = oy as f32 + tb.inset_t_pt;
    let content_w = (box_w_pt - tb.inset_l_pt - tb.inset_r_pt).max(1.0);
    let content_h = (box_h_pt - tb.inset_t_pt - tb.inset_b_pt).max(1.0);

    let mut lines: Vec<LpLine> = Vec::new();
    let mut total_h = 0.0f32;

    // Two passes are avoided; we compute lines then set vertical offset via the
    // frame transform below using anchor.
    struct Tmp {
        line: LpLine,
        height: f32,
    }
    let mut tmps: Vec<Tmp> = Vec::new();

    for para in &tb.paragraphs {
        let vlines = split_lines(para);
        let n = vlines.len();
        for (li, runs) in vlines.iter().enumerate() {
            let (run, line_w, line_h, baseline) = shape_line(fonts, para, runs, tb.font_scale, content_w);
            let rl = LpRunList { baseline, width: line_w, height: line_h, runs: run.into_iter().collect() };
            let line = LpLine {
                y: total_h,
                width: line_w,
                height: line_h,
                space_before: if li == 0 { para.space_before_pt } else { 0.0 },
                space_after: if li == n - 1 { para.space_after_pt } else { 0.0 },
                is_first_line_of_para: li == 0,
                is_last_line_of_para: li == n - 1,
                content: LpLineContent::RunList(rl),
            };
            total_h += line_h + line.space_before + line.space_after;
            tmps.push(Tmp { line, height: line_h });
        }
    }

    // Vertical anchor offset within the content box.
    let anchor_off = match tb.anchor {
        Anchor::Top => 0.0,
        Anchor::Center => ((content_h - total_h) * 0.5).max(0.0),
        Anchor::Bottom => (content_h - total_h).max(0.0),
    };

    // Re-emit lines with cumulative y already stored; adjust y by anchor.
    let mut cursor = 0.0f32;
    for t in tmps {
        let mut line = t.line;
        cursor += line.space_before;
        line.y = cursor;
        cursor += t.height + line.space_after;
        lines.push(line);
    }

    let parcel = LpParcel {
        x: 0.0,
        y: anchor_off,
        width: content_w,
        height: content_h,
        lines,
    };
    Some(LpFrame {
        transform: LpTransform {
            scale_x: 1.0,
            skew_y: 0.0,
            skew_x: 0.0,
            scale_y: 1.0,
            translate_x: content_x,
            translate_y: content_y,
        },
        parcel,
    })
}

fn split_lines(para: &Para) -> Vec<Vec<Run>> {
    let mut lines: Vec<Vec<Run>> = vec![Vec::new()];
    for run in &para.runs {
        let normalized = run.text.replace("\r\n", "\n").replace('\r', "\n").replace('\u{b}', "\n");
        for (i, part) in normalized.split('\n').enumerate() {
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

/// Shape one visual line, returning (run, width, height, baseline) in points.
fn shape_line(
    fonts: &mut FontSystem,
    para: &Para,
    line_runs: &[Run],
    font_scale: f32,
    width_pt: f32,
) -> (Option<LpRun>, f32, f32, f32) {
    let runs_max = line_runs.iter().map(|r| r.size_pt).fold(0.0f32, f32::max);
    let base_pt = if runs_max > 0.0 { runs_max } else { para.default_size_pt }.max(1.0) * font_scale;
    let line_h = base_pt * LINE_FACTOR;
    let mut buffer = Buffer::new(fonts, Metrics::new(base_pt, line_h));
    buffer.set_size(fonts, Some(width_pt.max(1.0)), None);

    let mut owned: Vec<(String, Attrs)> = Vec::new();
    let text: String = line_runs.iter().map(|r| r.text.as_str()).collect();
    for run in line_runs {
        if run.text.is_empty() {
            continue;
        }
        owned.push((run.text.clone(), attrs_for(run, font_scale)));
    }
    if owned.is_empty() {
        return (None, 0.0, line_h, base_pt * 0.8);
    }
    buffer.set_rich_text(
        fonts,
        owned.iter().map(|(t, a)| (t.as_str(), a.clone())),
        Attrs::new(),
        Shaping::Advanced,
    );
    buffer.shape_until_scroll(fonts, false);

    let mut glyphs: Vec<LpGlyph> = Vec::new();
    let mut line_w = 0.0f32;
    let mut baseline = base_pt * 0.8;
    if let Some(run) = buffer.layout_runs().next() {
        baseline = run.line_y;
        line_w = run.line_w;
        for g in run.glyphs.iter() {
            glyphs.push(LpGlyph { x: g.x, y: 0.0, advance: g.w, offset: g.start });
        }
    }

    let lp_run = LpRun {
        x: 0.0,
        width: line_w,
        transform: LpTransform::identity(),
        content: LpRunContent::Glyphs {
            text,
            font_size: base_pt,
            ascent: base_pt * 0.8,
            descent: base_pt * 0.2,
            glyphs,
        },
    };
    (Some(lp_run), line_w, line_h, baseline)
}

fn attrs_for<'r>(run: &'r Run, font_scale: f32) -> Attrs<'r> {
    let size = (run.size_pt * font_scale).max(1.0);
    let family = match &run.font {
        Some(f) => resolve_family(f),
        None => Family::SansSerif,
    };
    let mut a = Attrs::new().family(family);
    if run.bold {
        a = a.weight(Weight::BOLD);
    }
    if run.italic {
        a = a.style(Style::Italic);
    }
    a = a.color(Color::rgb(run.color[0], run.color[1], run.color[2]));
    a = a.metrics(Metrics::new(size, size * LINE_FACTOR));
    a
}
