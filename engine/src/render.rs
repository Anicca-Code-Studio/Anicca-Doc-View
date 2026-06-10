//! Text layout + rasterization using cosmic-text. Lays each paragraph out in a
//! single column, paginates by accumulated height, and draws glyphs into an
//! RGBA buffer. Fonts are bundled (DejaVu Sans) so rendering is deterministic
//! and self-contained in WASM (no system fonts).

use cosmic_text::{
    Align as CtAlign, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style,
    SwashCache, SwashContent, Weight,
};

use crate::model::{Align, Document, Paragraph};

const LINE_FACTOR: f32 = 1.15;

pub fn new_font_system() -> FontSystem {
    let mut db = cosmic_text::fontdb::Database::new();
    db.load_font_data(include_bytes!("../fonts/DejaVuSans.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/DejaVuSans-Bold.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/DejaVuSans-Oblique.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/DejaVuSans-BoldOblique.ttf").to_vec());
    db.set_sans_serif_family("DejaVu Sans");
    FontSystem::new_with_locale_and_db("en-US".to_string(), db)
}

fn ct_align(a: Align) -> Option<CtAlign> {
    match a {
        Align::Left => None,
        Align::Center => Some(CtAlign::Center),
        Align::Right => Some(CtAlign::End),
        Align::Justify => Some(CtAlign::Justified),
    }
}

/// Lays out one paragraph into a Buffer at `scale` pixels-per-point.
fn layout_paragraph(fs: &mut FontSystem, para: &Paragraph, width_px: f32, scale: f32) -> Buffer {
    let max_pt = para
        .runs
        .iter()
        .map(|r| r.style.size_pt)
        .fold(11.0_f32, f32::max);
    let font_px = (max_pt * scale).max(1.0);
    let line_px = (font_px * LINE_FACTOR * para.line_pct.max(0.5)).max(1.0);

    let mut buffer = Buffer::new(fs, Metrics::new(font_px, line_px));
    buffer.set_size(fs, Some(width_px.max(1.0)), None);

    let default_attrs = Attrs::new().family(Family::SansSerif);

    // Build spans borrowing each run's text. A paragraph with no runs still
    // needs one (empty) line so blank paragraphs occupy vertical space.
    let mut spans: Vec<(&str, Attrs)> = Vec::new();
    for run in &para.runs {
        if run.text.is_empty() {
            continue;
        }
        let sz = (run.style.size_pt * scale).max(1.0);
        let mut a = Attrs::new().family(Family::SansSerif);
        if run.style.bold {
            a = a.weight(Weight::BOLD);
        }
        if run.style.italic {
            a = a.style(Style::Italic);
        }
        let c = run.style.color;
        a = a.color(Color::rgb(c[0], c[1], c[2]));
        a = a.metrics(Metrics::new(sz, sz * LINE_FACTOR));
        spans.push((run.text.as_str(), a));
    }
    if spans.is_empty() {
        spans.push((" ", default_attrs.clone()));
    }

    buffer.set_rich_text(
        fs,
        spans.iter().map(|(t, a)| (*t, a.clone())),
        default_attrs.clone(),
        Shaping::Advanced,
    );
    let align = ct_align(para.align);
    for line in buffer.lines.iter_mut() {
        line.set_align(align);
    }
    buffer.shape_until_scroll(fs, false);
    buffer
}

fn buffer_height(buffer: &Buffer) -> f32 {
    let lh = buffer.metrics().line_height;
    buffer.layout_runs().count() as f32 * lh
}

/// Counts pages at unit scale (1px = 1pt) and records each paragraph's start page.
pub fn measure(fs: &mut FontSystem, doc: &Document) -> (usize, Vec<usize>) {
    let width = doc.content_w_pt();
    let content_h = doc.content_h_pt();
    let mut cursor = 0.0f32;
    let mut pages = Vec::with_capacity(doc.paragraphs.len());
    for para in &doc.paragraphs {
        cursor += para.space_before_pt;
        let start_page = (cursor / content_h).floor() as usize;
        pages.push(start_page);
        let buffer = layout_paragraph(fs, para, width, 1.0);
        cursor += buffer_height(&buffer) + para.space_after_pt;
    }
    let page_count = ((cursor / content_h).ceil() as usize).max(1);
    (page_count, pages)
}

/// Renders one page to an RGBA8 buffer of size `out_w * out_h * 4`.
pub fn render_page(
    fs: &mut FontSystem,
    swash: &mut SwashCache,
    doc: &Document,
    page: usize,
    out_w: usize,
    out_h: usize,
) -> Vec<u8> {
    let mut rgba = vec![255u8; out_w * out_h * 4];
    if out_w == 0 || out_h == 0 {
        return rgba;
    }

    let scale = out_w as f32 / doc.page_w_pt;
    let margin_px = doc.margin_pt * scale;
    let content_w_px = doc.content_w_pt() * scale;
    let content_h_px = doc.content_h_pt() * scale;
    let page_top = page as f32 * content_h_px;

    let mut cursor = 0.0f32;
    for para in &doc.paragraphs {
        cursor += para.space_before_pt * scale;
        let buffer = layout_paragraph(fs, para, content_w_px, scale);
        let lh = buffer.metrics().line_height;
        let run_count = buffer.layout_runs().count();

        for run in buffer.layout_runs() {
            let line_abs_top = cursor + run.line_top;
            let line_page = (line_abs_top / content_h_px).floor() as i32;
            if line_page != page as i32 {
                continue;
            }
            let baseline = margin_px + (cursor + run.line_y - page_top);
            for glyph in run.glyphs.iter() {
                let phys = glyph.physical((0.0, 0.0), 1.0);
                let color = glyph.color_opt.unwrap_or(Color::rgb(0, 0, 0));
                let pen_x = margin_px + phys.x as f32;
                let pen_y = baseline + phys.y as f32;
                if let Some(img) = swash.get_image(fs, phys.cache_key) {
                    blit(&mut rgba, out_w, out_h, img, pen_x, pen_y, color);
                }
            }
        }
        cursor += run_count as f32 * lh + para.space_after_pt * scale;
    }
    rgba
}

fn blit(
    rgba: &mut [u8],
    w: usize,
    h: usize,
    img: &cosmic_text::SwashImage,
    pen_x: f32,
    pen_y: f32,
    color: Color,
) {
    let pw = img.placement.width as i32;
    let ph = img.placement.height as i32;
    if pw <= 0 || ph <= 0 {
        return;
    }
    let x0 = pen_x.round() as i32 + img.placement.left;
    let y0 = pen_y.round() as i32 - img.placement.top;
    let cr = color.r();
    let cg = color.g();
    let cb = color.b();

    match img.content {
        SwashContent::Mask | SwashContent::SubpixelMask => {
            for j in 0..ph {
                for i in 0..pw {
                    let a = img.data[(j * pw + i) as usize];
                    if a == 0 {
                        continue;
                    }
                    put(rgba, w, h, x0 + i, y0 + j, cr, cg, cb, a);
                }
            }
        }
        SwashContent::Color => {
            for j in 0..ph {
                for i in 0..pw {
                    let idx = ((j * pw + i) * 4) as usize;
                    let r = img.data[idx];
                    let g = img.data[idx + 1];
                    let b = img.data[idx + 2];
                    let a = img.data[idx + 3];
                    if a == 0 {
                        continue;
                    }
                    put(rgba, w, h, x0 + i, y0 + j, r, g, b, a);
                }
            }
        }
    }
}

#[inline]
fn put(rgba: &mut [u8], w: usize, h: usize, x: i32, y: i32, r: u8, g: u8, b: u8, a: u8) {
    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
        return;
    }
    let idx = ((y as usize) * w + x as usize) * 4;
    let af = a as f32 / 255.0;
    let inv = 1.0 - af;
    rgba[idx] = (r as f32 * af + rgba[idx] as f32 * inv) as u8;
    rgba[idx + 1] = (g as f32 * af + rgba[idx + 1] as f32 * inv) as u8;
    rgba[idx + 2] = (b as f32 * af + rgba[idx + 2] as f32 * inv) as u8;
    rgba[idx + 3] = 255;
}
