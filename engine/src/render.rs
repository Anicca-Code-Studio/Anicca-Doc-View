//! Layout + rasterization for the new Document model (blocks: paragraphs + tables + images).
//! Uses cosmic-text for text shaping/layout. Embedded fonts from the DOCX are
//! loaded into the FontSystem so character metrics match the original document.

use cosmic_text::{
    Align as CtAlign, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style,
    SwashCache, SwashContent, Weight,
};

use crate::model::{Align, AnchorImage, Block, BorderStyle, Document, ImageFormat, Paragraph, Table, TabAlign, TabLeader};

// Single-spacing line height multiplier. The reference viewer uses 1.15.
const LINE_FACTOR: f32 = 1.15;
const EMU_PER_PT: f64 = 12700.0;

// ── font system ───────────────────────────────────────────────────────────────

pub fn new_font_system() -> FontSystem {
    let mut db = cosmic_text::fontdb::Database::new();
    // Roboto: Google Docs default; documents exported from GDocs declare it.
    db.load_font_data(include_bytes!("../fonts/Roboto-Regular.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/Roboto-Bold.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/Roboto-Italic.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/Roboto-BoldItalic.ttf").to_vec());
    // Liberation Sans/Serif: metric-compatible with Arial / Times New Roman.
    db.load_font_data(include_bytes!("../fonts/LiberationSans-Regular.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/LiberationSans-Bold.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/LiberationSans-Italic.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/LiberationSans-BoldItalic.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/LiberationSerif-Regular.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/LiberationSerif-Bold.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/LiberationSerif-Italic.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/LiberationSerif-BoldItalic.ttf").to_vec());
    // Times New Roman: exact match for documents that declare it.
    db.load_font_data(include_bytes!("../fonts/TimesNewRoman.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/TimesNewRoman-Bold.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/TimesNewRoman-Italic.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/TimesNewRoman-BoldItalic.ttf").to_vec());
    // Carlito: metric-compatible with Calibri.
    db.load_font_data(include_bytes!("../fonts/Carlito-Regular.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/Carlito-Bold.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/Carlito-Italic.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/Carlito-BoldItalic.ttf").to_vec());
    // DejaVu Sans: wide Unicode coverage, last-resort only.
    db.load_font_data(include_bytes!("../fonts/DejaVuSans.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/DejaVuSans-Bold.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/DejaVuSans-Oblique.ttf").to_vec());
    db.load_font_data(include_bytes!("../fonts/DejaVuSans-BoldOblique.ttf").to_vec());
    db.set_sans_serif_family("Liberation Sans");
    db.set_serif_family("Liberation Serif");
    let fs = FontSystem::new_with_locale_and_db("en-US".to_string(), db);
    register_families(&fs);
    fs
}

/// Load document's embedded fonts into an existing FontSystem.
pub fn load_embedded_fonts(fs: &mut FontSystem, fonts: &[Vec<u8>]) {
    for data in fonts {
        if data.len() >= 4 {
            fs.db_mut().load_font_data(data.clone());
        }
    }
    register_families(fs);
}

/// Families actually present in the font db (bundled + embedded). Used so an
/// exact-name match (e.g. a font embedded in the DOCX) always wins before any
/// substitution.
fn family_set() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static S: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    S.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

fn register_families(fs: &FontSystem) {
    if let Ok(mut set) = family_set().lock() {
        for face in fs.db().faces() {
            for (name, _) in &face.families {
                set.insert(name.to_ascii_lowercase());
            }
        }
    }
}

/// True when the name describes a serif face.
fn is_serif_name(lower: &str) -> bool {
    (lower.contains("serif") && !lower.contains("sans"))
        || lower.contains("times")
        || lower.contains("roman")
        || matches!(
            lower,
            "cambria" | "georgia" | "garamond" | "book antiqua" | "palatino"
                | "palatino linotype" | "constantia" | "baskerville"
        )
}

/// Resolve a document font name to a loaded family, matching how Google Docs
/// (the layout reference) renders documents. Exact matches (bundled or
/// embedded in the DOCX) win; otherwise substitute a metric-compatible face:
/// Arial-class names get Liberation Sans, Calibri gets Carlito, serif names
/// get Liberation Serif, anything else falls back to Liberation Sans.
fn resolve_family(name: &str) -> Family<'_> {
    let lower = name.to_ascii_lowercase();
    let available = family_set()
        .lock()
        .map(|s| s.contains(&lower))
        .unwrap_or(false);
    if available {
        return Family::Name(name);
    }
    if is_serif_name(&lower) {
        return Family::Name("Liberation Serif");
    }
    match lower.as_str() {
        "calibri" => Family::Name("Carlito"),
        _ => Family::Name("Liberation Sans"),
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn ct_align(a: Align) -> Option<CtAlign> {
    match a {
        Align::Left => None,
        Align::Center => Some(CtAlign::Center),
        Align::Right => Some(CtAlign::End),
        Align::Justify => Some(CtAlign::Justified),
    }
}

/// Effective content width for a paragraph accounting for left/right indents.
fn para_content_w(para: &Paragraph, base_w: f32, scale: f32) -> f32 {
    (base_w - para.indent_left_pt * scale - para.indent_right_pt * scale).max(1.0)
}

/// Page index for an absolute y position. The small epsilon keeps positions
/// that land exactly on a page boundary (e.g. right after a PageBreak sets
/// cursor = n * page_h) from flipping to the previous page due to float error.
#[inline]
fn page_of(y: f32, page_h: f32) -> usize {
    (((y + 0.25) / page_h).floor() as isize).max(0) as usize
}

// ── paragraph layout ──────────────────────────────────────────────────────────

/// Lays out one paragraph. `width_px` is the available content width in pixels.
/// Returns the laid-out Buffer.
fn layout_paragraph(fs: &mut FontSystem, para: &Paragraph, width_px: f32, scale: f32) -> Buffer {
    let has_text = para
        .runs
        .iter()
        .any(|r| r.inline_image.is_none() && !r.text.is_empty());
    // Empty paragraphs take the height of their paragraph mark (w:sz on pPr>rPr).
    let base_pt = if has_text { 11.0 } else { para.mark_style.size_pt };
    let max_pt = para
        .runs
        .iter()
        .filter(|r| r.inline_image.is_none())
        .map(|r| r.style.size_pt)
        .fold(base_pt, f32::max);
    let font_px = (max_pt * scale).max(1.0);
    let line_px = (font_px * LINE_FACTOR * para.line_pct.max(0.5)).max(1.0);

    let mut buffer = Buffer::new(fs, Metrics::new(font_px, line_px));
    buffer.set_size(fs, Some(width_px.max(1.0)), None);

    let default_attrs = Attrs::new().family(Family::SansSerif);

    // Prepend list prefix if present
    let prefix_owned: String = para.list_prefix.clone().unwrap_or_default();
    let mut spans: Vec<(&str, Attrs)> = Vec::new();

    if !prefix_owned.is_empty() {
        let pfx_attrs = if let Some(r) = para.runs.iter().find(|r| !r.text.is_empty()) {
            run_attrs(&r.style, scale)
        } else {
            default_attrs.clone()
        };
        spans.push((prefix_owned.as_str(), pfx_attrs));
    }

    for run in &para.runs {
        if run.inline_image.is_some() || run.text.is_empty() {
            continue;
        }
        spans.push((run.text.as_str(), run_attrs(&run.style, scale)));
    }

    if spans.is_empty() {
        spans.push((" ", run_attrs(&para.mark_style, scale)));
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

fn run_attrs<'a>(style: &'a crate::model::RunStyle, scale: f32) -> Attrs<'a> {
    let sz = (style.size_pt * scale).max(1.0);
    let family = if let Some(ref name) = style.font_name {
        resolve_family(name.as_str())
    } else {
        Family::SansSerif
    };
    let mut a = Attrs::new().family(family);
    if style.bold {
        a = a.weight(Weight::BOLD);
    }
    if style.italic {
        a = a.style(Style::Italic);
    }
    let c = style.color;
    a = a.color(Color::rgb(c[0], c[1], c[2]));
    a = a.metrics(Metrics::new(sz, sz * LINE_FACTOR));
    a
}

fn buffer_height(buffer: &Buffer) -> f32 {
    let lh = buffer.metrics().line_height;
    buffer.layout_runs().count() as f32 * lh
}

/// Measure width of a text string using the given run style.
fn measure_text_width(fs: &mut FontSystem, text: &str, style: &crate::model::RunStyle, scale: f32) -> f32 {
    if text.is_empty() { return 0.0; }
    let font_px = (style.size_pt * scale).max(1.0);
    let mut buf = Buffer::new(fs, Metrics::new(font_px, font_px * LINE_FACTOR));
    buf.set_size(fs, Some(99999.0), None);
    buf.set_text(fs, text, run_attrs(style, scale), Shaping::Advanced);
    buf.shape_until_scroll(fs, false);
    buf.layout_runs().next().map(|r| r.line_w).unwrap_or(0.0)
}

/// Render a single dot (leader) character repeatedly across a horizontal span.
fn draw_dot_leader(
    rgba: &mut Vec<u8>, out_w: usize, out_h: usize,
    fs: &mut FontSystem, swash: &mut SwashCache,
    style: &crate::model::RunStyle,
    x_start: f32, x_end: f32, baseline: f32,
    scale: f32,
) {
    if x_end <= x_start + 1.0 { return; }
    let dot_w = measure_text_width(fs, ".", style, scale).max(1.0);
    let mut x = x_start;
    while x + dot_w <= x_end {
        let font_px = (style.size_pt * scale).max(1.0);
        let mut buf = Buffer::new(fs, Metrics::new(font_px, font_px * LINE_FACTOR));
        buf.set_size(fs, Some(dot_w + 1.0), None);
        buf.set_text(fs, ".", run_attrs(style, scale), Shaping::Advanced);
        buf.shape_until_scroll(fs, false);
        for run in buf.layout_runs() {
            for glyph in run.glyphs.iter() {
                let phys = glyph.physical((0.0, 0.0), 1.0);
                let color = glyph.color_opt.unwrap_or(Color::rgb(0, 0, 0));
                let pen_x = x + phys.x as f32;
                let pen_y = baseline + phys.y as f32;
                if let Some(img) = swash.get_image(fs, phys.cache_key) {
                    blit_glyph(rgba, out_w, out_h, img, pen_x, pen_y, color);
                }
            }
        }
        x += dot_w;
    }
}

/// Height of one paragraph in pixels (at scale).
fn para_height(fs: &mut FontSystem, para: &Paragraph, width_px: f32, scale: f32) -> f32 {
    // Collect total image heights for this paragraph's inline images
    let mut img_height: f32 = 0.0;
    for run in &para.runs {
        if let Some(ref img) = run.inline_image {
            let h_pt = img.height_emu as f64 / EMU_PER_PT;
            img_height += (h_pt as f32 * scale).max(1.0);
        }
    }
    let buf = layout_paragraph(fs, para, width_px, scale);
    (buffer_height(&buf) + img_height).max(scale) // at least 1 line
}

// ── table helpers ─────────────────────────────────────────────────────────────

/// Compute effective table pixel width.
fn table_px(table: &Table, content_w_px: f32, scale: f32) -> f32 {
    if table.width_is_pct && table.width_dxa > 0 {
        // pct type: width_dxa is in 50ths-of-percent (5000 = 100%)
        (content_w_px * table.width_dxa as f32 / 5000.0).min(content_w_px)
    } else if table.width_dxa > 0 {
        // Use declared DXA width directly (fixed-layout tables may intentionally
        // exceed the text column by a few points; clamping would shrink columns
        // and cause text to wrap in narrow header cells).
        table.width_dxa as f32 / 20.0 * scale
    } else {
        content_w_px
    }
}

/// Compute column pixel widths for a table.
/// Returns one width per GRID column (honoring gridSpan for multi-column cells).
fn col_widths(table: &Table, content_w_px: f32, scale: f32) -> Vec<f32> {
    if table.rows.is_empty() {
        return Vec::new();
    }
    let tbl_px = table_px(table, content_w_px, scale);

    // Prefer tblGrid as authoritative column widths
    if !table.grid_col_widths.is_empty() {
        let total_dxa: u32 = table.grid_col_widths.iter().sum();
        if total_dxa > 0 {
            return table.grid_col_widths.iter()
                .map(|&w| tbl_px * w as f32 / total_dxa as f32)
                .collect();
        }
    }

    // Fall back to cell widths from first fully-specified row
    let ncols = table.rows.iter().map(|r| r.cells.len()).max().unwrap_or(0);
    if ncols == 0 {
        return Vec::new();
    }
    let ref_row = table.rows.iter().find(|r| r.cells.len() == ncols);
    if let Some(row) = ref_row {
        let total_dxa: u32 = row.cells.iter().map(|c| c.width_dxa).sum();
        if total_dxa > 0 {
            return row.cells.iter()
                .map(|c| tbl_px * c.width_dxa as f32 / total_dxa as f32)
                .collect();
        }
    }
    // Last fallback: equal widths
    let w = tbl_px / ncols as f32;
    vec![w; ncols]
}

/// Effective margins for a cell (per-cell w:tcMar overrides the table's).
#[inline]
fn cell_margins(table: &Table, cell: &crate::model::TableCell) -> crate::model::CellMargins {
    cell.margins.unwrap_or(table.cell_margins)
}

/// Compute height of a single table row in pixels.
fn row_height(
    fs: &mut FontSystem,
    row: &crate::model::TableRow,
    col_ws: &[f32],
    table: &Table,
    scale: f32,
) -> f32 {
    if row.height_exact && row.height_dxa > 0 {
        return row.height_dxa as f32 / 20.0 * scale;
    }
    let mut max_h: f32 = 0.0;
    let mut grid_col = 0usize;
    for cell in row.cells.iter() {
        let m = cell_margins(table, cell);
        let span = cell.grid_span.max(1) as usize;
        let cw: f32 = (grid_col..grid_col+span).map(|g| col_ws.get(g).copied().unwrap_or(0.0)).sum::<f32>().max(1.0);
        grid_col += span;
        let inner_w = (cw - (m.left + m.right) as f32 / 20.0 * scale).max(1.0);
        let mut cell_h: f32 = (m.top + m.bottom) as f32 / 20.0 * scale;
        for block in &cell.blocks {
            match block {
                Block::Paragraph(p) => {
                    cell_h += p.space_before_pt * scale;
                    cell_h += para_height(fs, p, inner_w, scale);
                    cell_h += p.space_after_pt * scale;
                }
                Block::Table(t) => {
                    cell_h += table_height(fs, t, inner_w, scale);
                }
                Block::PageBreak => {}
            }
        }
        max_h = max_h.max(cell_h);
    }
    if row.height_dxa > 0 {
        max_h.max(row.height_dxa as f32 / 20.0 * scale)
    } else {
        max_h.max(scale * 12.0)
    }
}

fn table_height(fs: &mut FontSystem, table: &Table, content_w_px: f32, scale: f32) -> f32 {
    let col_ws = col_widths(table, content_w_px, scale);
    table
        .rows
        .iter()
        .map(|r| row_height(fs, r, &col_ws, table, scale))
        .sum()
}

// ── body geometry ─────────────────────────────────────────────────────────────

/// Body top offset and body height in px. Word pushes the body down when the
/// header content is taller than the space between the header position and the
/// top margin (and mirrors this at the bottom for footers).
fn body_metrics(fs: &mut FontSystem, doc: &Document, scale: f32) -> (f32, f32) {
    let content_w_px = doc.content_w_pt() * scale;
    let hdr_h = doc
        .header
        .as_ref()
        .map(|h| measure_hf_height(fs, &h.blocks, content_w_px, scale))
        .unwrap_or(0.0);
    let ftr_h = doc
        .footer
        .as_ref()
        .map(|f| measure_hf_height(fs, &f.blocks, content_w_px, scale))
        .unwrap_or(0.0);
    let top = (doc.margin_t_pt * scale).max(doc.header_margin_pt * scale + hdr_h);
    let bottom = (doc.margin_b_pt * scale).max(doc.footer_margin_pt * scale + ftr_h);
    let h = (doc.page_h_pt * scale - top - bottom).max(1.0);
    (top, h)
}

// ── measure (pagination) ──────────────────────────────────────────────────────

/// Compute page count and per-block starting page. Returns (page_count, block_pages).
pub fn measure(fs: &mut FontSystem, doc: &Document) -> (usize, Vec<usize>) {
    let content_w = doc.content_w_pt();
    let (_body_top, content_h) = body_metrics(fs, doc, 1.0);
    let mut cursor = 0.0f32;
    let mut block_pages = Vec::with_capacity(doc.blocks.len());

    for block in &doc.blocks {
        let start_page = page_of(cursor, content_h);
        block_pages.push(start_page);
        match block {
            Block::Paragraph(p) => {
                cursor += p.space_before_pt;
                let eff_w = para_content_w(p, content_w, 1.0);
                // Inline images that don't fit the remaining page move to the
                // next page (mirrored in render_page).
                for run in &p.runs {
                    if let Some(ref img) = run.inline_image {
                        let h = (img.height_emu as f64 / EMU_PER_PT) as f32;
                        if h < 1.0 {
                            continue;
                        }
                        let local = cursor - page_of(cursor, content_h) as f32 * content_h;
                        if local + h > content_h && h <= content_h {
                            cursor = (page_of(cursor, content_h) + 1) as f32 * content_h;
                        }
                        cursor += h;
                    }
                }
                let buf = layout_paragraph(fs, p, eff_w, 1.0);
                cursor += buffer_height(&buf).max(1.0);
                cursor += p.space_after_pt;
            }
            Block::Table(t) => {
                cursor += table_height(fs, t, content_w, 1.0);
            }
            Block::PageBreak => {
                let cur_page = page_of(cursor, content_h);
                cursor = (cur_page + 1) as f32 * content_h;
            }
        }
    }
    let page_count = ((cursor / content_h).ceil() as usize).max(1);
    (page_count, block_pages)
}

// ── render ────────────────────────────────────────────────────────────────────

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
    let margin_l_px = doc.margin_l_pt * scale;
    let content_w_px = doc.content_w_pt() * scale;
    // Body geometry honors tall headers/footers (must match measure()).
    let (margin_t_px, content_h_px) = body_metrics(fs, doc, scale);
    let page_top = page as f32 * content_h_px;

    // Displayed page number honors w:pgNumType w:start (e.g. cover = 0)
    let page_number = (doc.page_num_start + page as i32).max(0) as usize;

    // Page header (first page uses the first section's header when present)
    let hdr = if page == 0 { doc.header_first.as_ref() } else { doc.header.as_ref() };
    if let Some(h) = hdr {
        let y0 = doc.header_margin_pt * scale;
        render_hf_blocks(
            fs, swash, &mut rgba, out_w, out_h,
            &h.blocks, margin_l_px, y0, content_w_px, scale, page_number,
        );
    }

    // Page footer: bottom edge anchored at (page height - footer margin)
    let ftr = if page == 0 { doc.footer_first.as_ref() } else { doc.footer.as_ref() };
    if let Some(f) = ftr {
        let fh = measure_hf_height(fs, &f.blocks, content_w_px, scale);
        let y0 = (doc.page_h_pt - doc.footer_margin_pt) * scale - fh;
        render_hf_blocks(
            fs, swash, &mut rgba, out_w, out_h,
            &f.blocks, margin_l_px, y0, content_w_px, scale, page_number,
        );
    }

    let mut cursor = 0.0f32; // absolute y in pixels across all pages

    for block in &doc.blocks {
        // Quick skip: if this block starts way past the current page, stop
        let block_page = page_of(cursor, content_h_px);
        if block_page > page + 1 {
            break;
        }

        match block {
            Block::PageBreak => {
                let cur_page = page_of(cursor, content_h_px);
                cursor = (cur_page + 1) as f32 * content_h_px;
                continue;
            }
            Block::Paragraph(para) => {
                let cursor_before_para = cursor;
                let abs_y_before = cursor;
                cursor += para.space_before_pt * scale;

                let indent_l_px = para.indent_left_pt * scale;
                let indent_r_px = para.indent_right_pt * scale;
                let eff_content_x = margin_l_px + indent_l_px;
                let eff_content_w = (content_w_px - indent_l_px - indent_r_px).max(1.0);

                // Render inline images
                for run in &para.runs {
                    if let Some(ref img) = run.inline_image {
                        let w_pt = img.width_emu as f64 / EMU_PER_PT;
                        let h_pt = img.height_emu as f64 / EMU_PER_PT;
                        let img_w = (w_pt as f32 * scale) as u32;
                        let img_h = (h_pt as f32 * scale) as u32;

                        // Push to the next page when the image doesn't fit the
                        // remaining space (mirrored in measure()).
                        let local = cursor - page_of(cursor, content_h_px) as f32 * content_h_px;
                        if local + img_h as f32 > content_h_px && (img_h as f32) <= content_h_px {
                            cursor = (page_of(cursor, content_h_px) + 1) as f32 * content_h_px;
                        }

                        let abs_y = cursor;
                        let page_local_y = abs_y - page_top;
                        let render_y = margin_t_px + page_local_y;
                        let render_x = eff_content_x;

                        let on_this_page = abs_y >= page_top - img_h as f32
                            && abs_y < page_top + content_h_px;
                        if on_this_page && img_w > 0 && img_h > 0 {
                            blit_image(&mut rgba, out_w, out_h, &img.data, img.format.clone(), render_x as i32, render_y as i32, img_w, img_h);
                        }
                        cursor += img_h as f32;
                    }
                }

                // Check if this paragraph uses tab stops with tab chars in text
                let has_tabs = !para.tab_stops.is_empty()
                    && para.runs.iter().any(|r| r.text.contains('\t'));

                if has_tabs {
                    // Split at LAST \t: everything before = left text (entry title);
                    // everything after = right text (page number). Intermediate \t → space.
                    let mut left_parts: Vec<(String, crate::model::RunStyle)> = Vec::new();
                    let mut right_parts: Vec<(String, crate::model::RunStyle)> = Vec::new();
                    let mut tab_style: Option<crate::model::RunStyle> = None;
                    let last_tab_run = para.runs.iter().enumerate()
                        .filter(|(_, r)| r.inline_image.is_none() && r.text.contains('\t'))
                        .last().map(|(i, _)| i);
                    if let Some(last_idx) = last_tab_run {
                        let last_tab_pos = para.runs[last_idx].text.rfind('\t').unwrap();
                        for (i, run) in para.runs.iter().enumerate() {
                            if run.inline_image.is_some() { continue; }
                            if i < last_idx {
                                let t = run.text.replace('\t', " ");
                                if !t.is_empty() { left_parts.push((t, run.style.clone())); }
                            } else if i == last_idx {
                                let left = run.text[..last_tab_pos].replace('\t', " ");
                                let right = run.text[last_tab_pos+1..].to_string();
                                if !left.is_empty() { left_parts.push((left, run.style.clone())); }
                                if !right.is_empty() { right_parts.push((right, run.style.clone())); }
                                tab_style = Some(run.style.clone());
                            } else {
                                if !run.text.is_empty() {
                                    right_parts.push((run.text.clone(), run.style.clone()));
                                }
                            }
                        }
                    }

                    // Determine tab stop x position
                    let right_tab = para.tab_stops.iter()
                        .find(|t| t.align == TabAlign::Right);
                    let tab_x = if let Some(ts) = right_tab {
                        let raw = ts.pos_dxa as f32 / 20.0 * scale;
                        if raw > eff_content_w { eff_content_w } else { raw }
                    } else {
                        eff_content_w
                    };
                    let tab_abs_x = eff_content_x + tab_x;

                    // Default style for measurements
                    let def_style = tab_style.unwrap_or_else(|| {
                        if let Some((_, s)) = left_parts.first() { s.clone() }
                        else { crate::model::RunStyle::default() }
                    });

                    // Measure right part width
                    let right_text: String = right_parts.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>().join("");
                    let right_w = if right_text.is_empty() { 0.0 }
                        else { measure_text_width(fs, &right_text, &def_style, scale) };
                    let right_start_x = (tab_abs_x - right_w).max(eff_content_x);

                    // Line height from style
                    let font_px = (def_style.size_pt * scale).max(1.0);
                    let lh = font_px * LINE_FACTOR;
                    let line_abs_top = cursor;
                    let line_pg = page_of(line_abs_top, content_h_px);

                    if line_pg == page {
                        let baseline = margin_t_px + (cursor + lh * 0.85 - page_top);

                        // Render left text: each part rendered consecutively (track x)
                        let mut left_cur_x = eff_content_x;
                        for (text, style) in &left_parts {
                            let tw = measure_text_width(fs, text, style, scale);
                            let mut buf = Buffer::new(fs, Metrics::new(font_px, lh));
                            buf.set_size(fs, Some(tw + 2.0), None);
                            buf.set_text(fs, text.as_str(), run_attrs(style, scale), Shaping::Advanced);
                            buf.shape_until_scroll(fs, false);
                            for run in buf.layout_runs() {
                                for glyph in run.glyphs.iter() {
                                    let phys = glyph.physical((0.0, 0.0), 1.0);
                                    let color = glyph.color_opt.unwrap_or(Color::rgb(0, 0, 0));
                                    let pen_x = left_cur_x + phys.x as f32;
                                    let pen_y = baseline + phys.y as f32;
                                    if let Some(img) = swash.get_image(fs, phys.cache_key) {
                                        blit_glyph(&mut rgba, out_w, out_h, img, pen_x, pen_y, color);
                                    }
                                }
                            }
                            left_cur_x += tw;
                        }

                        // Dot leader if applicable
                        let left_w = left_cur_x - eff_content_x;
                        let leader_align = right_tab.map(|t| &t.leader).unwrap_or(&TabLeader::None);
                        if *leader_align == TabLeader::Dot {
                            draw_dot_leader(
                                &mut rgba, out_w, out_h,
                                fs, swash, &def_style,
                                eff_content_x + left_w + 2.0,
                                right_start_x - 2.0,
                                baseline, scale,
                            );
                        }

                        // Render right text (page number)
                        let mut right_x = right_start_x;
                        for (text, style) in &right_parts {
                            let tw = measure_text_width(fs, text, style, scale);
                            let mut buf = Buffer::new(fs, Metrics::new(font_px, lh));
                            buf.set_size(fs, Some(tw + 1.0), None);
                            buf.set_text(fs, text.as_str(), run_attrs(style, scale), Shaping::Advanced);
                            buf.shape_until_scroll(fs, false);
                            for run in buf.layout_runs() {
                                for glyph in run.glyphs.iter() {
                                    let phys = glyph.physical((0.0, 0.0), 1.0);
                                    let color = glyph.color_opt.unwrap_or(Color::rgb(0, 0, 0));
                                    let pen_x = right_x + phys.x as f32;
                                    let pen_y = baseline + phys.y as f32;
                                    if let Some(img) = swash.get_image(fs, phys.cache_key) {
                                        blit_glyph(&mut rgba, out_w, out_h, img, pen_x, pen_y, color);
                                    }
                                }
                            }
                            right_x += tw;
                        }
                    }
                    cursor += lh;
                } else {
                let buf = layout_paragraph(fs, para, eff_content_w, scale);
                let lh = buf.metrics().line_height;
                let run_count = buf.layout_runs().count();

                for run in buf.layout_runs() {
                    let line_abs_top = cursor + run.line_top;
                    let line_pg = page_of(line_abs_top, content_h_px);
                    if line_pg != page {
                        continue;
                    }
                    let baseline = margin_t_px + (cursor + run.line_y - page_top);
                    for glyph in run.glyphs.iter() {
                        let phys = glyph.physical((0.0, 0.0), 1.0);
                        let color = glyph.color_opt.unwrap_or(Color::rgb(0, 0, 0));
                        let pen_x = eff_content_x + phys.x as f32;
                        let pen_y = baseline + phys.y as f32;
                        if let Some(img) = swash.get_image(fs, phys.cache_key) {
                            blit_glyph(&mut rgba, out_w, out_h, img, pen_x, pen_y, color);
                        }
                    }
                }
                cursor += run_count as f32 * lh;
                } // end else (no tabs)

                // Render anchor/floating images at their absolute positions
                render_anchor_images(
                    &mut rgba, out_w, out_h,
                    &para.anchor_images,
                    cursor_before_para, page, page_top, content_h_px,
                    margin_l_px, margin_t_px,
                    scale,
                );

                cursor += para.space_after_pt * scale;
                let _ = abs_y_before;
            }
            Block::Table(table) => {
                render_table(
                    fs,
                    swash,
                    &mut rgba,
                    out_w,
                    out_h,
                    table,
                    &mut cursor,
                    page,
                    content_h_px,
                    page_top,
                    margin_l_px,
                    margin_t_px,
                    content_w_px,
                    scale,
                );
            }
        }
    }
    rgba
}

// ── anchor image rendering ────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn render_anchor_images(
    rgba: &mut Vec<u8>,
    out_w: usize,
    out_h: usize,
    anchors: &[AnchorImage],
    cursor_before_para: f32,
    page: usize,
    page_top: f32,
    content_h_px: f32,
    margin_l_px: f32,
    margin_t_px: f32,
    scale: f32,
) {
    // Anchor images are owned by their containing paragraph's page.
    // Only render them on the same page as the paragraph, not on subsequent pages.
    let para_page = page_of(cursor_before_para, content_h_px);
    if para_page != page {
        return;
    }

    // behindDoc images first so foreground images (e.g. logos) stay visible
    let ordered = anchors
        .iter()
        .filter(|a| a.behind_doc)
        .chain(anchors.iter().filter(|a| !a.behind_doc));

    for anchor in ordered {
        let w_pt = anchor.width_emu as f64 / EMU_PER_PT;
        let h_pt = anchor.height_emu as f64 / EMU_PER_PT;
        let img_w = (w_pt as f32 * scale) as u32;
        let img_h = (h_pt as f32 * scale) as u32;
        if img_w == 0 || img_h == 0 {
            continue;
        }

        let x_pt = anchor.pos_x_emu as f64 / EMU_PER_PT;
        let render_x = match anchor.pos_ref_h {
            1 => x_pt as f32 * scale,               // page-relative: from page left edge
            _ => margin_l_px + x_pt as f32 * scale, // column-relative (default)
        };

        let y_pt = anchor.pos_y_emu as f64 / EMU_PER_PT;
        let abs_y = match anchor.pos_ref_v {
            1 => y_pt as f32 * scale,                       // page-relative
            _ => cursor_before_para + y_pt as f32 * scale,  // paragraph-relative
        };
        let page_local_y = abs_y - page_top;
        let render_y = margin_t_px + page_local_y;

        blit_image(rgba, out_w, out_h, &anchor.data, anchor.format.clone(),
                   render_x as i32, render_y as i32, img_w, img_h);
    }
}

// ── header/footer rendering ───────────────────────────────────────────────────

/// Total height of header/footer content in pixels.
fn measure_hf_height(fs: &mut FontSystem, blocks: &[Block], content_w_px: f32, scale: f32) -> f32 {
    let mut h = 0.0f32;
    for block in blocks {
        match block {
            Block::Paragraph(p) => {
                h += p.space_before_pt * scale;
                h += para_height(fs, p, para_content_w(p, content_w_px, scale), scale);
                h += p.space_after_pt * scale;
            }
            Block::Table(t) => {
                h += table_height(fs, t, content_w_px, scale);
            }
            Block::PageBreak => {}
        }
    }
    h
}

/// Render one header/footer paragraph at absolute canvas position.
/// Returns the height consumed in pixels.
#[allow(clippy::too_many_arguments)]
fn render_hf_paragraph(
    fs: &mut FontSystem,
    swash: &mut SwashCache,
    rgba: &mut Vec<u8>,
    out_w: usize,
    out_h: usize,
    para: &Paragraph,
    x_base: f32,
    y: f32,
    content_w_px: f32,
    scale: f32,
    page_number: usize,
) -> f32 {
    // Substitute PAGE field values with the actual page number
    let mut p = para.clone();
    for run in p.runs.iter_mut() {
        if run.is_page_number {
            run.text = page_number.to_string();
        }
    }

    let indent_l = p.indent_left_pt * scale;
    let indent_r = p.indent_right_pt * scale;
    let eff_x = x_base + indent_l;
    let eff_w = (content_w_px - indent_l - indent_r).max(1.0);

    // Anchored images (e.g. logo in header), positioned relative to this paragraph.
    // behindDoc images first so foreground images stay visible.
    let ordered_anchors = p.anchor_images.iter().filter(|a| a.behind_doc)
        .chain(p.anchor_images.iter().filter(|a| !a.behind_doc));
    for anchor in ordered_anchors {
        let w_pt = anchor.width_emu as f64 / EMU_PER_PT;
        let h_pt = anchor.height_emu as f64 / EMU_PER_PT;
        let img_w = (w_pt as f32 * scale) as u32;
        let img_h = (h_pt as f32 * scale) as u32;
        if img_w == 0 || img_h == 0 {
            continue;
        }
        let x_pt = anchor.pos_x_emu as f64 / EMU_PER_PT;
        let rx = match anchor.pos_ref_h {
            1 => x_pt as f32 * scale,
            _ => x_base + x_pt as f32 * scale,
        };
        let y_pt = anchor.pos_y_emu as f64 / EMU_PER_PT;
        let ry = match anchor.pos_ref_v {
            1 => y_pt as f32 * scale,
            _ => y + y_pt as f32 * scale,
        };
        blit_image(rgba, out_w, out_h, &anchor.data, anchor.format.clone(),
                   rx as i32, ry as i32, img_w, img_h);
    }

    let mut used_h = 0.0f32;

    // Inline images
    for run in &p.runs {
        if let Some(ref img) = run.inline_image {
            let w_pt = img.width_emu as f64 / EMU_PER_PT;
            let h_pt = img.height_emu as f64 / EMU_PER_PT;
            let img_w = (w_pt as f32 * scale) as u32;
            let img_h = (h_pt as f32 * scale) as u32;
            if img_w > 0 && img_h > 0 {
                blit_image(rgba, out_w, out_h, &img.data, img.format.clone(),
                           eff_x as i32, (y + used_h) as i32, img_w, img_h);
                used_h += img_h as f32;
            }
        }
    }

    let has_tab = p.runs.iter().any(|r| r.inline_image.is_none() && r.text.contains('\t'));

    if has_tab {
        // Split at LAST \t: left = title text, right = page number. Intermediate \t → space.
        let mut left_parts: Vec<(String, crate::model::RunStyle)> = Vec::new();
        let mut right_parts: Vec<(String, crate::model::RunStyle)> = Vec::new();
        let mut tab_style: Option<crate::model::RunStyle> = None;
        let last_tab_run = p.runs.iter().enumerate()
            .filter(|(_, r)| r.inline_image.is_none() && r.text.contains('\t'))
            .last().map(|(i, _)| i);
        if let Some(last_idx) = last_tab_run {
            let last_tab_pos = p.runs[last_idx].text.rfind('\t').unwrap();
            for (i, run) in p.runs.iter().enumerate() {
                if run.inline_image.is_some() { continue; }
                if i < last_idx {
                    let t = run.text.replace('\t', " ");
                    if !t.is_empty() { left_parts.push((t, run.style.clone())); }
                } else if i == last_idx {
                    let left = run.text[..last_tab_pos].replace('\t', " ");
                    let right = run.text[last_tab_pos+1..].to_string();
                    if !left.is_empty() { left_parts.push((left, run.style.clone())); }
                    if !right.is_empty() { right_parts.push((right, run.style.clone())); }
                    tab_style = Some(run.style.clone());
                } else {
                    if !run.text.is_empty() { right_parts.push((run.text.clone(), run.style.clone())); }
                }
            }
        }

        let right_tab = p.tab_stops.iter().find(|t| t.align == TabAlign::Right);
        let tab_x = match right_tab {
            Some(ts) => (ts.pos_dxa as f32 / 20.0 * scale).min(eff_w),
            None => eff_w,
        };
        let tab_abs_x = eff_x + tab_x;

        let def_style = tab_style.unwrap_or_else(|| {
            if let Some((_, s)) = left_parts.first() {
                s.clone()
            } else {
                crate::model::RunStyle::default()
            }
        });

        let font_px = (def_style.size_pt * scale).max(1.0);
        let lh = font_px * LINE_FACTOR;
        let baseline = y + used_h + lh * 0.85;

        // Left text
        let mut left_x = eff_x;
        for (text, style) in &left_parts {
            let tw = measure_text_width(fs, text, style, scale);
            let mut buf = Buffer::new(fs, Metrics::new(font_px, lh));
            buf.set_size(fs, Some(tw + 2.0), None);
            buf.set_text(fs, text.as_str(), run_attrs(style, scale), Shaping::Advanced);
            buf.shape_until_scroll(fs, false);
            for run in buf.layout_runs() {
                for glyph in run.glyphs.iter() {
                    let phys = glyph.physical((0.0, 0.0), 1.0);
                    let color = glyph.color_opt.unwrap_or(Color::rgb(0, 0, 0));
                    if let Some(img) = swash.get_image(fs, phys.cache_key) {
                        blit_glyph(rgba, out_w, out_h, img, left_x + phys.x as f32, baseline + phys.y as f32, color);
                    }
                }
            }
            left_x += tw;
        }

        // Right text: measure per run for accurate mixed-style width
        let right_w: f32 = right_parts.iter()
            .map(|(t, s)| measure_text_width(fs, t, s, scale))
            .sum();
        let right_start_x = (tab_abs_x - right_w).max(left_x);

        // Dot leader
        let leader = right_tab.map(|t| &t.leader).unwrap_or(&TabLeader::None);
        if *leader == TabLeader::Dot {
            draw_dot_leader(rgba, out_w, out_h, fs, swash, &def_style,
                            left_x + 2.0, right_start_x - 2.0, baseline, scale);
        }

        let mut right_x = right_start_x;
        for (text, style) in &right_parts {
            let tw = measure_text_width(fs, text, style, scale);
            let mut buf = Buffer::new(fs, Metrics::new(font_px, lh));
            buf.set_size(fs, Some(tw + 2.0), None);
            buf.set_text(fs, text.as_str(), run_attrs(style, scale), Shaping::Advanced);
            buf.shape_until_scroll(fs, false);
            for run in buf.layout_runs() {
                for glyph in run.glyphs.iter() {
                    let phys = glyph.physical((0.0, 0.0), 1.0);
                    let color = glyph.color_opt.unwrap_or(Color::rgb(0, 0, 0));
                    if let Some(img) = swash.get_image(fs, phys.cache_key) {
                        blit_glyph(rgba, out_w, out_h, img, right_x + phys.x as f32, baseline + phys.y as f32, color);
                    }
                }
            }
            right_x += tw;
        }
        used_h += lh;
    } else {
        let buf = layout_paragraph(fs, &p, eff_w, scale);
        let lh = buf.metrics().line_height;
        let run_count = buf.layout_runs().count();
        for run in buf.layout_runs() {
            let baseline = y + used_h + run.line_y;
            for glyph in run.glyphs.iter() {
                let phys = glyph.physical((0.0, 0.0), 1.0);
                let color = glyph.color_opt.unwrap_or(Color::rgb(0, 0, 0));
                if let Some(img) = swash.get_image(fs, phys.cache_key) {
                    blit_glyph(rgba, out_w, out_h, img, eff_x + phys.x as f32, baseline + phys.y as f32, color);
                }
            }
        }
        used_h += run_count as f32 * lh;
    }

    used_h
}

/// Render header or footer blocks starting at absolute canvas y position.
#[allow(clippy::too_many_arguments)]
fn render_hf_blocks(
    fs: &mut FontSystem,
    swash: &mut SwashCache,
    rgba: &mut Vec<u8>,
    out_w: usize,
    out_h: usize,
    blocks: &[Block],
    margin_l_px: f32,
    y_start: f32,
    content_w_px: f32,
    scale: f32,
    page_number: usize,
) {
    let mut y = y_start;
    for block in blocks {
        match block {
            Block::Table(table) => {
                // Reuse render_table with a single virtual page anchored at y.
                let mut cursor = 0.0f32;
                render_table(
                    fs, swash, rgba, out_w, out_h, table,
                    &mut cursor, 0, 1.0e9, 0.0, margin_l_px, y, content_w_px, scale,
                );
                y += cursor;
            }
            Block::Paragraph(para) => {
                y += para.space_before_pt * scale;
                y += render_hf_paragraph(
                    fs, swash, rgba, out_w, out_h, para,
                    margin_l_px, y, content_w_px, scale, page_number,
                );
                y += para.space_after_pt * scale;
            }
            Block::PageBreak => {}
        }
    }
}

// ── table rendering ───────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn render_table(
    fs: &mut FontSystem,
    swash: &mut SwashCache,
    rgba: &mut Vec<u8>,
    out_w: usize,
    out_h: usize,
    table: &Table,
    cursor: &mut f32,
    page: usize,
    content_h_px: f32,
    page_top: f32,
    margin_l_px: f32,
    margin_t_px: f32,
    content_w_px: f32,
    scale: f32,
) {
    let col_ws = col_widths(table, content_w_px, scale);
    let table_indent_px = table.indent_dxa as f32 / 20.0 * scale;
    let n_rows = table.rows.len();

    for (ri, row) in table.rows.iter().enumerate() {
        let rh = row_height(fs, row, &col_ws, table, scale);
        let row_abs_y = *cursor;
        let row_end = row_abs_y + rh;
        let row_start_pg = page_of(row_abs_y, content_h_px);
        let row_end_pg = page_of((row_end - 1.0).max(row_abs_y), content_h_px).max(row_start_pg);

        // A row may span multiple pages; render its segment on each page it touches.
        if page >= row_start_pg && page <= row_end_pg {
            let seg_start = row_abs_y.max(page_top);
            let seg_end = row_end.min(page_top + content_h_px);
            let seg_y = margin_t_px + (seg_start - page_top);
            let seg_h = (seg_end - seg_start).max(0.0);
            let mut cell_x = margin_l_px + table_indent_px;

            let mut grid_col = 0usize;
            for (ci, cell) in row.cells.iter().enumerate() {
                let span = cell.grid_span.max(1) as usize;
                let cw: f32 = (grid_col..grid_col+span).map(|g| col_ws.get(g).copied().unwrap_or(0.0)).sum::<f32>().max(1.0);
                grid_col += span;

                // Cell background (only this page's segment)
                if let Some(bg) = cell.bg_color {
                    fill_rect(
                        rgba,
                        out_w,
                        out_h,
                        cell_x as i32,
                        seg_y as i32,
                        cw as i32,
                        seg_h as i32,
                        bg,
                    );
                }

                // Effective borders: explicit cell border (even "nil") overrides
                // the table-level border for that edge.
                let pick = |cell_b: &crate::model::BorderLine, tbl_b: &crate::model::BorderLine| {
                    if cell_b.explicit { cell_b.clone() } else { tbl_b.clone() }
                };
                let tbl_top = if ri == 0 { &table.borders.top } else { &table.borders.inside_h };
                let tbl_bottom = if ri + 1 == n_rows { &table.borders.bottom } else { &table.borders.inside_h };
                let tbl_left = if ci == 0 { &table.borders.left } else { &table.borders.inside_v };
                let tbl_right = if ci + 1 == row.cells.len() { &table.borders.right } else { &table.borders.inside_v };
                let b_top = pick(&cell.borders.top, tbl_top);
                let b_bottom = pick(&cell.borders.bottom, tbl_bottom);
                let b_left = pick(&cell.borders.left, tbl_left);
                let b_right = pick(&cell.borders.right, tbl_right);
                draw_vert_line(rgba, out_w, out_h, cell_x as i32, seg_y as i32, seg_h as i32, &b_left, scale);
                draw_vert_line(rgba, out_w, out_h, (cell_x + cw) as i32, seg_y as i32, seg_h as i32, &b_right, scale);
                if page == row_start_pg {
                    draw_horiz_line(rgba, out_w, out_h, cell_x as i32, seg_y as i32, cw as i32, &b_top, scale);
                }
                if page == row_end_pg {
                    draw_horiz_line(rgba, out_w, out_h, cell_x as i32, (seg_y + seg_h) as i32, cw as i32, &b_bottom, scale);
                }

                // Cell content flows in absolute coordinates; blocks landing on
                // a later page render there (row splitting across pages).
                let m = cell_margins(table, cell);
                let inner_x = cell_x + m.left as f32 / 20.0 * scale;
                let inner_w = (cw - (m.left + m.right) as f32 / 20.0 * scale).max(1.0);
                let mut cur_abs = row_abs_y + m.top as f32 / 20.0 * scale;
                render_cell_blocks(
                    fs, swash, rgba, out_w, out_h,
                    &cell.blocks, inner_x, inner_w, &mut cur_abs,
                    page, page_top, content_h_px, margin_t_px, scale,
                );
                cell_x += cw;
            }
        }

        *cursor += rh;
    }
}

/// Render blocks inside a table cell. `cur_abs` flows in absolute document
/// coordinates so content crossing a page boundary appears on the next page.
#[allow(clippy::too_many_arguments)]
fn render_cell_blocks(
    fs: &mut FontSystem,
    swash: &mut SwashCache,
    rgba: &mut Vec<u8>,
    out_w: usize,
    out_h: usize,
    blocks: &[Block],
    inner_x: f32,
    inner_w: f32,
    cur_abs: &mut f32,
    page: usize,
    page_top: f32,
    content_h_px: f32,
    margin_t_px: f32,
    scale: f32,
) {
    for block in blocks {
        match block {
            Block::Paragraph(p) => {
                *cur_abs += p.space_before_pt * scale;

                let indent_l = p.indent_left_pt * scale;
                let eff_x = inner_x + indent_l;
                let eff_w = (inner_w - indent_l - p.indent_right_pt * scale).max(1.0);

                // Inline images (e.g. signature images inside cells)
                for run in &p.runs {
                    if let Some(ref img) = run.inline_image {
                        let w_pt = img.width_emu as f64 / EMU_PER_PT;
                        let h_pt = img.height_emu as f64 / EMU_PER_PT;
                        let img_w = (w_pt as f32 * scale) as u32;
                        let img_h = (h_pt as f32 * scale) as u32;
                        if img_w > 0 && img_h > 0 {
                            if page_of(*cur_abs, content_h_px) == page {
                                let ry = margin_t_px + (*cur_abs - page_top);
                                blit_image(rgba, out_w, out_h, &img.data, img.format.clone(),
                                           eff_x as i32, ry as i32, img_w, img_h);
                            }
                            *cur_abs += img_h as f32;
                        }
                    }
                }

                let buf = layout_paragraph(fs, p, eff_w, scale);
                let lh = buf.metrics().line_height;
                let run_count = buf.layout_runs().count();
                for run in buf.layout_runs() {
                    let line_abs = *cur_abs + run.line_top;
                    if page_of(line_abs, content_h_px) != page {
                        continue;
                    }
                    let baseline = margin_t_px + (*cur_abs + run.line_y - page_top);
                    for glyph in run.glyphs.iter() {
                        let phys = glyph.physical((0.0, 0.0), 1.0);
                        let color = glyph.color_opt.unwrap_or(Color::rgb(0, 0, 0));
                        let pen_x = eff_x + phys.x as f32;
                        let pen_y = baseline + phys.y as f32;
                        if let Some(img) = swash.get_image(fs, phys.cache_key) {
                            blit_glyph(rgba, out_w, out_h, img, pen_x, pen_y, color);
                        }
                    }
                }
                *cur_abs += run_count as f32 * lh;
                *cur_abs += p.space_after_pt * scale;
            }
            Block::Table(nested) => {
                render_table(
                    fs, swash, rgba, out_w, out_h, nested,
                    cur_abs, page, content_h_px, page_top,
                    inner_x, margin_t_px, inner_w, scale,
                );
            }
            Block::PageBreak => {}
        }
    }
}

// ── drawing primitives ────────────────────────────────────────────────────────

fn fill_rect(
    rgba: &mut [u8],
    w: usize,
    h: usize,
    x: i32,
    y: i32,
    rw: i32,
    rh: i32,
    color: [u8; 3],
) {
    for dy in 0..rh {
        for dx in 0..rw {
            put(rgba, w, h, x + dx, y + dy, color[0], color[1], color[2], 255);
        }
    }
}

fn draw_borders(
    rgba: &mut [u8],
    w: usize,
    h: usize,
    borders: &crate::model::Borders,
    x: i32,
    y: i32,
    bw: i32,
    bh: i32,
    scale: f32,
) {
    draw_horiz_line(rgba, w, h, x, y, bw, &borders.top, scale);
    draw_horiz_line(rgba, w, h, x, y + bh, bw, &borders.bottom, scale);
    draw_vert_line(rgba, w, h, x, y, bh, &borders.left, scale);
    draw_vert_line(rgba, w, h, x + bw, y, bh, &borders.right, scale);
}

fn draw_horiz_line(
    rgba: &mut [u8],
    w: usize,
    h: usize,
    x: i32,
    y: i32,
    len: i32,
    border: &crate::model::BorderLine,
    scale: f32,
) {
    if border.style == BorderStyle::None || border.size_eighth_pt == 0 {
        return;
    }
    let thickness = ((border.size_eighth_pt as f32 / 8.0) * scale).max(1.0) as i32;
    for t in 0..thickness {
        for dx in 0..len {
            put(rgba, w, h, x + dx, y + t, border.color[0], border.color[1], border.color[2], 255);
        }
    }
}

fn draw_vert_line(
    rgba: &mut [u8],
    w: usize,
    h: usize,
    x: i32,
    y: i32,
    len: i32,
    border: &crate::model::BorderLine,
    scale: f32,
) {
    if border.style == BorderStyle::None || border.size_eighth_pt == 0 {
        return;
    }
    let thickness = ((border.size_eighth_pt as f32 / 8.0) * scale).max(1.0) as i32;
    for t in 0..thickness {
        for dy in 0..len {
            put(rgba, w, h, x + t, y + dy, border.color[0], border.color[1], border.color[2], 255);
        }
    }
}

// ── image decoding ────────────────────────────────────────────────────────────

fn blit_image(
    rgba: &mut Vec<u8>,
    out_w: usize,
    out_h: usize,
    data: &[u8],
    _format: ImageFormat,
    x: i32,
    y: i32,
    target_w: u32,
    target_h: u32,
) {
    let img = match image::load_from_memory(data) {
        Ok(i) => i,
        Err(_) => return,
    };
    let img = img.resize_exact(target_w, target_h, image::imageops::FilterType::Lanczos3);
    let buf = img.to_rgba8();
    let (iw, ih) = (buf.width() as i32, buf.height() as i32);
    for iy in 0..ih {
        for ix in 0..iw {
            let px = buf.get_pixel(ix as u32, iy as u32);
            put(rgba, out_w, out_h, x + ix, y + iy, px[0], px[1], px[2], px[3]);
        }
    }
}

// ── glyph blit ────────────────────────────────────────────────────────────────

fn blit_glyph(
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
    if a == 255 {
        rgba[idx] = r;
        rgba[idx + 1] = g;
        rgba[idx + 2] = b;
        rgba[idx + 3] = 255;
    } else {
        let af = a as f32 / 255.0;
        let inv = 1.0 - af;
        rgba[idx] = (r as f32 * af + rgba[idx] as f32 * inv) as u8;
        rgba[idx + 1] = (g as f32 * af + rgba[idx + 1] as f32 * inv) as u8;
        rgba[idx + 2] = (b as f32 * af + rgba[idx + 2] as f32 * inv) as u8;
        rgba[idx + 3] = 255;
    }
}
