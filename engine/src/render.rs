//! Layout + rasterization for the new Document model (blocks: paragraphs + tables + images).
//! Uses cosmic-text for text shaping/layout. Embedded fonts from the DOCX are
//! loaded into the FontSystem so character metrics match the original document.

use cosmic_text::{
    Align as CtAlign, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style,
    SwashCache, SwashContent, Weight,
};
use serde::Serialize;

use crate::model::{Align, AnchorImage, Block, BorderStyle, Document, ImageFormat, Paragraph, Table, TabAlign, TabLeader, VAlign};

// Single-spacing line height multiplier. The reference viewer uses 1.15.
const LINE_FACTOR: f32 = 1.15;
const EMU_PER_PT: f64 = 12700.0;

// â”€â”€ font system â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

pub fn new_font_system() -> FontSystem {
    let mut db = cosmic_text::fontdb::Database::new();
    // Bundled faces (see fonts_bundled.rs): Roboto (Google Docs default),
    // Liberation Sans/Serif (metric-compatible with Arial / Times New Roman),
    // Times New Roman, Carlito (Calibri-compatible), DejaVu Sans for wide
    // Unicode coverage, and Noto faces for multilingual fallback. cosmic_text
    // auto-selects a Noto face when a glyph is missing from the primary font.
    for data in crate::fonts_bundled::ALL {
        db.load_font_data(data.to_vec());
    }
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
pub(crate) fn resolve_family(name: &str) -> Family<'_> {
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

// â”€â”€ helpers â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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

// â”€â”€ paragraph layout â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Lays out one paragraph. `width_px` is the available content width in pixels.
/// Returns the laid-out Buffer.
fn layout_paragraph(fs: &mut FontSystem, para: &Paragraph, width_px: f32, scale: f32) -> Buffer {
    let base_pt = para.mark_style.size_pt;
    let max_pt = para
        .runs
        .iter()
        .filter(|r| r.inline_image.is_none())
        .map(|r| r.style.size_pt)
        .fold(base_pt, f32::max);
    let font_px = (max_pt * scale).max(1.0);
    let line_px = if let Some(exact_pt) = para.line_height_exact_pt {
        // w:lineRule="exact": absolute line height, ignore LINE_FACTOR and line_pct
        (exact_pt * scale).max(1.0)
    } else {
        let factor = LINE_FACTOR.max(para.line_pct.max(0.5));
        let natural = (font_px * factor).max(1.0);
        if let Some(atleast_pt) = para.line_height_atleast_pt {
            // w:lineRule="atLeast": natural height or minimum, whichever is larger
            natural.max(atleast_pt * scale)
        } else {
            natural
        }
    };

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
        spans.push((run.text.as_str(), run_attrs_for(&run.style, run.text.as_str(), scale)));
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

fn is_cjk_dominant(s: &str) -> bool {
    let mut cjk = 0u32;
    let mut total = 0u32;
    for c in s.chars() {
        total += 1;
        if matches!(c,
            '\u{3000}'..='\u{9FFF}' |
            '\u{F900}'..='\u{FAFF}' |
            '\u{AC00}'..='\u{D7FF}' |
            '\u{20000}'..='\u{2FA1F}'
        ) {
            cjk += 1;
        }
    }
    total > 0 && cjk * 2 >= total
}

fn run_attrs_for<'a>(style: &'a crate::model::RunStyle, text: &str, scale: f32) -> Attrs<'a> {
    let sz = if style.size_cs_pt.is_some() && is_complex_script(text) {
        (style.size_cs_pt.unwrap() * scale).max(1.0)
    } else {
        (style.size_pt * scale).max(1.0)
    };
    let family = if is_cjk_dominant(text) {
        if let Some(ref ea) = style.font_name_east_asia {
            resolve_family(ea.as_str())
        } else if let Some(ref name) = style.font_name {
            resolve_family(name.as_str())
        } else {
            Family::SansSerif
        }
    } else if let Some(ref name) = style.font_name {
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

fn is_complex_script(s: &str) -> bool {
    s.chars().any(|c| matches!(c,
        '\u{0600}'..='\u{06FF}' |   // Arabic
        '\u{0590}'..='\u{05FF}' |   // Hebrew
        '\u{0900}'..='\u{097F}' |   // Devanagari
        '\u{0E00}'..='\u{0E7F}'     // Thai
    ))
}

fn run_attrs<'a>(style: &'a crate::model::RunStyle, scale: f32) -> Attrs<'a> {
    run_attrs_for(style, "", scale)
}

fn buffer_height(buffer: &Buffer) -> f32 {
    let lh = buffer.metrics().line_height;
    buffer.layout_runs().count() as f32 * lh
}

/// Compute effective line height for a paragraph in points (scale=1.0).
fn para_line_height_pt(para: &Paragraph) -> f32 {
    let base_pt = para.mark_style.size_pt;
    let max_pt = para.runs.iter().filter(|r| r.inline_image.is_none())
        .map(|r| r.style.size_pt).fold(base_pt, f32::max);
    if let Some(exact_pt) = para.line_height_exact_pt {
        exact_pt
    } else {
        let factor = LINE_FACTOR.max(para.line_pct.max(0.5));
        let natural = max_pt * factor;
        if let Some(atleast_pt) = para.line_height_atleast_pt {
            natural.max(atleast_pt)
        } else {
            natural
        }
    }
}

/// Space before paragraph in pixels, resolving beforeLines if set.
fn para_space_before(para: &Paragraph, scale: f32) -> f32 {
    if let Some(lines) = para.space_before_lines {
        lines * para_line_height_pt(para) * scale
    } else {
        para.space_before_pt * scale
    }
}

/// Space after paragraph in pixels, resolving afterLines if set.
fn para_space_after(para: &Paragraph, scale: f32) -> f32 {
    if let Some(lines) = para.space_after_lines {
        lines * para_line_height_pt(para) * scale
    } else {
        para.space_after_pt * scale
    }
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
                    blit_glyph(rgba, out_w, out_h, img, pen_x, pen_y, color, None);
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

// â”€â”€ table helpers â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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

    // Actual grid columns used = max sum-of-gridSpan across all rows.
    // This is the ground truth for how many col_ws entries cells will index into.
    let max_grid_used = table.rows.iter()
        .map(|r| r.cells.iter().map(|c| c.grid_span.max(1) as usize).sum::<usize>())
        .max()
        .unwrap_or(0);

    // Prefer tblGrid as authoritative column widths.
    if !table.grid_col_widths.is_empty() {
        let cols = &table.grid_col_widths;
        // LibreOffice/Word export bug: some tools emit the same N columns twice.
        // Detect ONLY when: count is exactly 2x actual grid usage AND halves are identical.
        // This avoids misidentifying legitimate equal-width tables (e.g. [3000, 3000]).
        let effective: &[u32] = {
            let n = cols.len();
            let h = n / 2;
            if n % 2 == 0 && h == max_grid_used && h > 0 && cols[..h] == cols[h..] {
                &cols[..h]
            } else {
                cols
            }
        };
        let total_dxa: u32 = effective.iter().sum();
        if total_dxa > 0 {
            return effective.iter()
                .map(|&w| tbl_px * w as f32 / total_dxa as f32)
                .collect();
        }
    }

    // Fall back to cell widths from first fully-specified row.
    // ncols from gridSpan sums (not physical cell count) to handle merged-cell rows.
    let ncols = max_grid_used;
    if ncols == 0 {
        return Vec::new();
    }
    let ref_row = table.rows.iter().find(|r| {
        r.cells.iter().map(|c| c.grid_span.max(1) as usize).sum::<usize>() == ncols
    });
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
        // no_wrap cells render at layout_w=99999 (single line); measure at same width.
        let meas_w = if cell.no_wrap { 99999.0 } else { inner_w };
        let mut cell_h: f32 = (m.top + m.bottom) as f32 / 20.0 * scale;
        for block in &cell.blocks {
            match block {
                Block::Paragraph(p) => {
                    cell_h += para_space_before(p, scale);
                    cell_h += para_height(fs, p, meas_w, scale);
                    cell_h += para_space_after(p, scale);
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

// â”€â”€ body geometry â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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

// â”€â”€ measure (pagination) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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
                cursor += para_space_before(p, 1.0);
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
                cursor += para_space_after(p, 1.0);
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

/// For XLSX documents, recompute page_dims heights from actual rendered row heights.
/// XLSX parse() uses declared row heights for canvas allocation. Auto-height rows
/// (height_exact=false) expand during render. This two-pass fix measures actual
/// heights using the same FontSystem as rendering so canvas is sized correctly.
pub fn measure_xlsx_page_heights(fs: &mut FontSystem, doc: &mut Document) {
    if doc.doc_format != "xlsx" {
        return;
    }
    let mut page_idx = 0usize;
    for block in &doc.blocks {
        match block {
            Block::Table(table) => {
                // Measure at scale=4.0 to minimize pixel-rounding error in text wrap
                // decisions. Dividing back by 4.0 gives points. Measuring at scale=1.0
                // causes cosmic-text rounding to undercount wrap lines vs higher scales.
                let meas_scale = 4.0f32;
                let content_w_px = table.width_dxa as f32 / 20.0 * meas_scale;
                let actual_h = table_height(fs, table, content_w_px, meas_scale) / meas_scale;
                if let Some(dims) = doc.page_dims.get_mut(page_idx) {
                    // Expand only: declared height is minimum, content may need more
                    dims.1 = dims.1.max(actual_h);
                }
                page_idx += 1;
            }
            Block::PageBreak => {}
            _ => {}
        }
    }
}

// â”€â”€ render â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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

    // Per-page dimension override (used by XLSX where each sheet has its own size).
    let (page_w_pt, page_h_pt) = doc
        .page_dims
        .get(page)
        .copied()
        .unwrap_or((doc.page_w_pt, doc.page_h_pt));

    let scale = out_w as f32 / page_w_pt;
    let margin_l_px = doc.margin_l_pt * scale;
    // For XLSX (margin=0), content fills the full page width.
    let content_w_px = (page_w_pt - doc.margin_l_pt - doc.margin_r_pt).max(1.0) * scale;
    // Body geometry honors tall headers/footers (must match measure()).
    // For XLSX (no headers/footers, no margins), body_metrics gives (0, page_h_pt*scale).
    let (margin_t_px, content_h_px) = if doc.page_dims.is_empty() {
        body_metrics(fs, doc, scale)
    } else {
        let top = doc.margin_t_pt * scale;
        let bottom = doc.margin_b_pt * scale;
        let h = (page_h_pt * scale - top - bottom).max(1.0);
        (top, h)
    };
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
        let y0 = (page_h_pt - doc.footer_margin_pt) * scale - fh;
        render_hf_blocks(
            fs, swash, &mut rgba, out_w, out_h,
            &f.blocks, margin_l_px, y0, content_w_px, scale, page_number,
        );
    }

    // XLSX fast path: each page is one independent sheet; render only that table.
    // This bypasses the pagination logic which assumes uniform page heights.
    if !doc.page_dims.is_empty() {
        let clip_h = page_h_pt * scale;
        let mut cursor_y = 0.0f32;
        for (i, block) in doc.blocks.iter().enumerate() {
            let bp = doc.block_pages.get(i).copied().unwrap_or(0);
            if bp != page {
                continue;
            }
            if let Block::Table(table) = block {
                render_table(
                    fs, swash, &mut rgba, out_w, out_h,
                    table, &mut cursor_y,
                    0, clip_h, 0.0,
                    margin_l_px, 0.0, content_w_px, scale,
                );
            }
        }
        return rgba;
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
                cursor += para_space_before(para, scale);

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
                        let render_x = match para.align {
                            Align::Center => eff_content_x + (eff_content_w - img_w as f32) / 2.0,
                            Align::Right => eff_content_x + eff_content_w - img_w as f32,
                            _ => eff_content_x,
                        };

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
                    // everything after = right text (page number). Intermediate \t â†’ space.
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
                        let actual_line_y = {
                            let mut tmp = Buffer::new(fs, Metrics::new(font_px, lh));
                            tmp.set_size(fs, Some(font_px * 2.0), None);
                            tmp.set_text(fs, "X", run_attrs(&def_style, scale), Shaping::Advanced);
                            tmp.shape_until_scroll(fs, false);
                            tmp.layout_runs().next().map(|r| r.line_y).unwrap_or(lh * 0.85)
                        };
                        let baseline = margin_t_px + (cursor + actual_line_y - page_top);

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
                                        blit_glyph(&mut rgba, out_w, out_h, img, pen_x, pen_y, color, None);
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
                                        blit_glyph(&mut rgba, out_w, out_h, img, pen_x, pen_y, color, None);
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
                            blit_glyph(&mut rgba, out_w, out_h, img, pen_x, pen_y, color, None);
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

                cursor += para_space_after(para, scale);
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

// â”€â”€ anchor image rendering â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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
        let render_x = if anchor.align_h == 1 {
            // center: relative to reference area
            let area_w = match anchor.pos_ref_h {
                1 => out_w as f32,              // page width
                _ => out_w as f32 - margin_l_px * 2.0, // content/column width
            };
            let x_origin = if anchor.pos_ref_h == 1 { 0.0 } else { margin_l_px };
            x_origin + (area_w - img_w as f32) / 2.0
        } else if anchor.align_h == 2 {
            // right
            let area_w = match anchor.pos_ref_h {
                1 => out_w as f32,
                _ => out_w as f32 - margin_l_px * 2.0,
            };
            let x_origin = if anchor.pos_ref_h == 1 { 0.0 } else { margin_l_px };
            x_origin + area_w - img_w as f32
        } else {
            match anchor.pos_ref_h {
                1 => x_pt as f32 * scale,               // page-relative: from page left edge
                _ => margin_l_px + x_pt as f32 * scale, // column-relative (default)
            }
        };

        let y_pt = anchor.pos_y_emu as f64 / EMU_PER_PT;
        let abs_y = if anchor.align_v == 1 {
            // center vertically on page
            let page_h = content_h_px;
            let page_top_abs = page as f32 * page_h;
            page_top_abs + (page_h - img_h as f32) / 2.0
        } else {
            match anchor.pos_ref_v {
                1 => y_pt as f32 * scale,                       // page-relative
                _ => cursor_before_para + y_pt as f32 * scale,  // paragraph-relative
            }
        };
        let page_local_y = abs_y - page_top;
        let render_y = margin_t_px + page_local_y;

        blit_image(rgba, out_w, out_h, &anchor.data, anchor.format.clone(),
                   render_x as i32, render_y as i32, img_w, img_h);
    }
}

// â”€â”€ header/footer rendering â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Total height of header/footer content in pixels.
fn measure_hf_height(fs: &mut FontSystem, blocks: &[Block], content_w_px: f32, scale: f32) -> f32 {
    let mut h = 0.0f32;
    for block in blocks {
        match block {
            Block::Paragraph(p) => {
                h += para_space_before(p, scale);
                h += para_height(fs, p, para_content_w(p, content_w_px, scale), scale);
                h += para_space_after(p, scale);
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
        // Split at LAST \t: left = title text, right = page number. Intermediate \t â†’ space.
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
                        blit_glyph(rgba, out_w, out_h, img, left_x + phys.x as f32, baseline + phys.y as f32, color, None);
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
                        blit_glyph(rgba, out_w, out_h, img, right_x + phys.x as f32, baseline + phys.y as f32, color, None);
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
                    blit_glyph(rgba, out_w, out_h, img, eff_x + phys.x as f32, baseline + phys.y as f32, color, None);
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
                y += para_space_before(para, scale);
                y += render_hf_paragraph(
                    fs, swash, rgba, out_w, out_h, para,
                    margin_l_px, y, content_w_px, scale, page_number,
                );
                y += para_space_after(para, scale);
            }
            Block::PageBreak => {}
        }
    }
}

// â”€â”€ table rendering â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Measure total rendered height of cell content blocks (used for v_align offset).
fn measure_cell_content_h(
    fs: &mut FontSystem,
    blocks: &[Block],
    layout_w: f32,
    scale: f32,
) -> f32 {
    let mut h = 0.0f32;
    for block in blocks {
        match block {
            Block::Paragraph(p) => {
                h += para_space_before(p, scale);
                h += para_height(fs, p, layout_w, scale);
                h += para_space_after(p, scale);
            }
            Block::Table(t) => {
                h += table_height(fs, t, layout_w, scale);
            }
            Block::PageBreak => {}
        }
    }
    h
}

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
                // For no-wrap cells, layout with a very large width so text stays on one line.
                let layout_w = if cell.no_wrap { 99999.0 } else { inner_w };
                // Clip right boundary for no-wrap: glyphs past cell edge are skipped.
                let clip_right = if cell.no_wrap { Some(cell_x + cw) } else { None };
                // Vertical alignment offset (only meaningful for exact-height rows).
                let v_offset = if cell.v_align != VAlign::Top && rh > scale {
                    let inner_h = (rh - (m.top + m.bottom) as f32 / 20.0 * scale).max(0.0);
                    let content_h = measure_cell_content_h(fs, &cell.blocks, layout_w, scale);
                    match cell.v_align {
                        VAlign::Center => ((inner_h - content_h) / 2.0).max(0.0),
                        VAlign::Bottom => (inner_h - content_h).max(0.0),
                        VAlign::Top => 0.0,
                    }
                } else {
                    0.0
                };
                let mut cur_abs = row_abs_y + m.top as f32 / 20.0 * scale + v_offset;
                // Clip text at the row's bottom boundary to prevent overflow into adjacent rows.
                // row_end is the absolute Y of this row's bottom; convert to canvas Y.
                let clip_bottom = {
                    let row_bottom_abs = row_end.min(page_top + content_h_px);
                    let cell_bottom_abs = row_bottom_abs - m.bottom as f32 / 20.0 * scale;
                    Some((margin_t_px + (cell_bottom_abs - page_top)).max(0.0))
                };
                render_cell_blocks(
                    fs, swash, rgba, out_w, out_h,
                    &cell.blocks, inner_x, inner_w, layout_w, clip_right, clip_bottom, &mut cur_abs,
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
/// `layout_w`: buffer width for paragraph layout (use 99999 for no-wrap cells).
/// `clip_right`: when Some(x), glyphs whose left edge >= x are skipped (no-wrap clip).
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
    layout_w: f32,
    clip_right: Option<f32>,
    clip_bottom: Option<f32>,
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
                *cur_abs += para_space_before(p, scale);

                let indent_l = p.indent_left_pt * scale;
                let eff_x = inner_x + indent_l;
                let eff_w = (inner_w - indent_l - p.indent_right_pt * scale).max(1.0);
                // For no-wrap mode: left-aligned text uses large layout_w so it doesn't wrap
                // and overflows past the cell boundary (clipped by clip_right).
                // Center/right-aligned text always uses eff_w so alignment stays correct.
                let shape_w = if layout_w > inner_w && p.align == Align::Left {
                    layout_w
                } else {
                    eff_w
                };

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
                                let render_x = match p.align {
                                    Align::Center => eff_x + (eff_w - img_w as f32) / 2.0,
                                    Align::Right  => eff_x + eff_w - img_w as f32,
                                    _             => eff_x,
                                };
                                blit_image(rgba, out_w, out_h, &img.data, img.format.clone(),
                                           render_x as i32, ry as i32, img_w, img_h);
                            }
                            *cur_abs += img_h as f32;
                        }
                    }
                }

                let buf = layout_paragraph(fs, p, shape_w, scale);
                let lh = buf.metrics().line_height;
                let run_count = buf.layout_runs().count();
                for run in buf.layout_runs() {
                    let line_abs = *cur_abs + run.line_top;
                    // Skip lines below the cell's bottom clip boundary (fixed-height rows).
                    if let Some(cb) = clip_bottom {
                        let line_canvas_y = margin_t_px + (line_abs - page_top);
                        if line_canvas_y >= cb { continue; }
                    }
                    if page_of(line_abs, content_h_px) != page {
                        continue;
                    }
                    let baseline = margin_t_px + (*cur_abs + run.line_y - page_top);
                    for glyph in run.glyphs.iter() {
                        let phys = glyph.physical((0.0, 0.0), 1.0);
                        let color = glyph.color_opt.unwrap_or(Color::rgb(0, 0, 0));
                        let pen_x = eff_x + phys.x as f32;
                        // Clip glyphs that start past the cell right boundary (no-wrap mode)
                        if let Some(cr) = clip_right {
                            if pen_x >= cr { continue; }
                        }
                        let pen_y = baseline + phys.y as f32;
                        if let Some(img) = swash.get_image(fs, phys.cache_key) {
                            let cb_px = clip_bottom.map(|cb| cb as i32);
                            blit_glyph(rgba, out_w, out_h, img, pen_x, pen_y, color, cb_px);
                        }
                    }
                }
                *cur_abs += run_count as f32 * lh;
                *cur_abs += para_space_after(p, scale);
            }
            Block::Table(nested) => {
                // clip_bottom not propagated into nested tables â€” they manage their own row heights
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

// â”€â”€ drawing primitives â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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

// â”€â”€ image decoding â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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

// â”€â”€ glyph blit â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn blit_glyph(
    rgba: &mut [u8],
    w: usize,
    h: usize,
    img: &cosmic_text::SwashImage,
    pen_x: f32,
    pen_y: f32,
    color: Color,
    clip_bottom_px: Option<i32>,
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
    let cb_color = color.b();
    // Pixel rows below this y are clipped (cell bottom boundary).
    let max_j = if let Some(cb) = clip_bottom_px {
        (cb - y0).min(ph)
    } else {
        ph
    };
    if max_j <= 0 {
        return;
    }

    match img.content {
        SwashContent::Mask | SwashContent::SubpixelMask => {
            for j in 0..max_j {
                for i in 0..pw {
                    let a = img.data[(j * pw + i) as usize];
                    if a == 0 {
                        continue;
                    }
                    put(rgba, w, h, x0 + i, y0 + j, cr, cg, cb_color, a);
                }
            }
        }
        SwashContent::Color => {
            for j in 0..max_j {
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

// â”€â”€ layout page structs â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LpTransform {
    pub scale_x: f32, pub skew_y: f32, pub skew_x: f32, pub scale_y: f32,
    pub translate_x: f32, pub translate_y: f32,
}
impl LpTransform {
    pub fn identity() -> Self {
        LpTransform { scale_x: 1.0, skew_y: 0.0, skew_x: 0.0, scale_y: 1.0, translate_x: 0.0, translate_y: 0.0 }
    }
}

#[derive(Serialize)]
pub struct LpGlyph { pub x: f32, pub y: f32, pub advance: f32, pub offset: usize }

#[derive(Serialize)]
#[serde(tag = "type")]
pub enum LpRunContent {
    #[serde(rename = "glyphs")]
    Glyphs {
        text: String,
        #[serde(rename = "fontSize")] font_size: f32,
        ascent: f32, descent: f32,
        glyphs: Vec<LpGlyph>,
    },
    #[serde(rename = "space")]
    Space { advance: f32, #[serde(rename = "fontSize")] font_size: f32, ascent: f32, descent: f32 },
    #[serde(rename = "tab")]
    Tab { advance: f32, #[serde(rename = "fontSize")] font_size: f32, ascent: f32, descent: f32 },
    #[serde(rename = "paragraphEnd")]
    ParagraphEnd { advance: f32 },
    #[serde(rename = "break")]
    Break,
    #[serde(rename = "inlineDrawing")]
    InlineDrawing { width: f32, height: f32 },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LpRun { pub x: f32, pub width: f32, pub transform: LpTransform, pub content: LpRunContent }

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LpRunList { pub baseline: f32, pub width: f32, pub height: f32, pub runs: Vec<LpRun> }

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LpTableColumn { pub x: f32, pub width: f32 }

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LpTableCell {
    pub col_index: usize, pub col_span: usize, pub row_span: usize,
    pub x: f32, pub y: f32, pub width: f32, pub height: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parcel: Option<LpParcel>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LpTableRow { pub y: f32, pub height: f32, pub cells: Vec<LpTableCell> }

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LpTable {
    pub width: f32, pub height: f32,
    pub columns: Vec<LpTableColumn>, pub rows: Vec<LpTableRow>,
}

#[derive(Serialize)]
#[serde(tag = "type")]
pub enum LpLineContent {
    #[serde(rename = "runList")]
    RunList(LpRunList),
    #[serde(rename = "table")]
    Table(LpTable),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LpLine {
    pub y: f32, pub width: f32, pub height: f32,
    pub space_before: f32, pub space_after: f32,
    pub is_first_line_of_para: bool, pub is_last_line_of_para: bool,
    pub content: LpLineContent,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LpParcel { pub x: f32, pub y: f32, pub width: f32, pub height: f32, pub lines: Vec<LpLine> }

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LpFrame { pub transform: LpTransform, pub parcel: LpParcel }

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LpPage { pub width: f32, pub height: f32, pub frames: Vec<LpFrame> }

// â”€â”€ layout_page: extract text positions for text layer â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

pub fn layout_page(fs: &mut FontSystem, doc: &Document, page_idx: usize) -> LpPage {
    // Fixed 2px-per-point scale for sub-point glyph precision; output converted back to points.
    // Not tied to any canvas size â€” same result regardless of viewer zoom or page size.
    let scale = 2.0_f32;
    let inv = 0.5_f32;
    let margin_l_px = doc.margin_l_pt * scale;
    let content_w_px = doc.content_w_pt() * scale;
    let (margin_t_px, content_h_px) = body_metrics(fs, doc, scale);
    let page_top = page_idx as f32 * content_h_px;

    let mut lines: Vec<LpLine> = Vec::new();
    let mut cursor = 0.0f32;

    for block in &doc.blocks {
        let block_page = page_of(cursor, content_h_px);
        if block_page > page_idx + 1 { break; }

        match block {
            Block::PageBreak => {
                cursor = (page_of(cursor, content_h_px) + 1) as f32 * content_h_px;
            }
            Block::Paragraph(para) => {
                cursor += para_space_before(para, scale);
                let indent_l = para.indent_left_pt * scale;
                let first_line_indent = para.indent_first_line_pt * scale;
                let indent_r = para.indent_right_pt * scale;
                let hanging = para.list_hanging_pt * scale;
                let eff_w = (content_w_px - indent_l - indent_r).max(1.0);
                let eff_x = margin_l_px + indent_l;

                // Advance cursor for inline images (same logic as render_page)
                for run in &para.runs {
                    if let Some(ref img) = run.inline_image {
                        let ih = (img.height_emu as f64 / EMU_PER_PT) as f32 * scale;
                        if ih >= 1.0 {
                            let local = cursor - page_of(cursor, content_h_px) as f32 * content_h_px;
                            if local + ih > content_h_px && ih <= content_h_px {
                                cursor = (page_of(cursor, content_h_px) + 1) as f32 * content_h_px;
                            }
                            cursor += ih;
                        }
                    }
                }

                let _ = (first_line_indent, hanging); // used for positioning in render_page
                let buf = layout_paragraph(fs, para, eff_w, scale);
                let lh = buf.metrics().line_height;
                let all_runs: Vec<_> = buf.layout_runs().collect();
                let n = all_runs.len();

                for (li, lr) in all_runs.iter().enumerate() {
                    let line_abs_top = cursor + lr.line_top;
                    if page_of(line_abs_top, content_h_px) != page_idx { continue; }

                    let line_page_y = margin_t_px + (line_abs_top - page_top);
                    let baseline_offset = lr.line_y - lr.line_top;

                    let font_sz_px = lr.glyphs.first().map(|g| g.font_size)
                        .unwrap_or(para.mark_style.size_pt * scale);
                    let font_sz_pt = font_sz_px * inv;
                    let ascent_px = lr.line_y - lr.line_top;
                    let descent_px = lh - ascent_px;

                    let glyphs: Vec<LpGlyph> = lr.glyphs.iter().map(|g| LpGlyph {
                        x: g.x * inv,
                        y: 0.0,
                        advance: g.w * inv,
                        offset: g.start,
                    }).collect();

                    let lp_run = LpRun {
                        x: eff_x * inv,
                        width: lr.line_w * inv,
                        transform: LpTransform::identity(),
                        content: LpRunContent::Glyphs {
                            text: lr.text.to_string(),
                            font_size: font_sz_pt,
                            ascent: ascent_px * inv,
                            descent: descent_px * inv,
                            glyphs,
                        },
                    };

                    lines.push(LpLine {
                        y: line_page_y * inv,
                        width: lr.line_w * inv,
                        height: lh * inv,
                        space_before: 0.0,
                        space_after: 0.0,
                        is_first_line_of_para: li == 0,
                        is_last_line_of_para: li + 1 == n,
                        content: LpLineContent::RunList(LpRunList {
                            baseline: baseline_offset * inv,
                            width: lr.line_w * inv,
                            height: lh * inv,
                            runs: vec![lp_run],
                        }),
                    });
                }

                cursor += n as f32 * lh;
                cursor += para_space_after(para, scale);
            }
            Block::Table(table) => {
                if let Some(tbl_line) = lp_table_line(fs, table, &mut cursor, page_idx,
                    content_h_px, page_top, margin_l_px, margin_t_px, content_w_px, scale, inv)
                {
                    lines.push(tbl_line);
                }
            }
        }
    }

    let parcel = LpParcel {
        x: 0.0, y: 0.0,
        width: doc.page_w_pt, height: doc.page_h_pt,
        lines,
    };
    LpPage {
        width: doc.page_w_pt,
        height: doc.page_h_pt,
        frames: vec![LpFrame { transform: LpTransform::identity(), parcel }],
    }
}

fn lp_table_line(
    fs: &mut FontSystem, table: &Table, cursor: &mut f32, page_idx: usize,
    content_h_px: f32, page_top: f32, margin_l_px: f32, margin_t_px: f32,
    content_w_px: f32, scale: f32, inv: f32,
) -> Option<LpLine> {
    let col_ws = col_widths(table, content_w_px, scale);
    let tbl_indent_px = table.indent_dxa as f32 / 20.0 * scale;
    let tbl_y_abs = *cursor;

    let total_h: f32 = table.rows.iter()
        .map(|r| row_height(fs, r, &col_ws, table, scale))
        .sum();

    let tbl_page_y_px = margin_t_px + (tbl_y_abs - page_top);

    let mut col_x_acc = margin_l_px + tbl_indent_px;
    let columns: Vec<LpTableColumn> = col_ws.iter().map(|&w| {
        let col = LpTableColumn { x: col_x_acc * inv, width: w * inv };
        col_x_acc += w;
        col
    }).collect();

    let mut lp_rows: Vec<LpTableRow> = Vec::new();
    let mut row_cursor = *cursor;

    for row in &table.rows {
        let rh = row_height(fs, row, &col_ws, table, scale);
        let row_y_in_tbl = row_cursor - tbl_y_abs;
        let mut lp_cells: Vec<LpTableCell> = Vec::new();
        let mut cell_x = margin_l_px + tbl_indent_px;
        let mut grid_col = 0usize;

        for (ci, cell) in row.cells.iter().enumerate() {
            let span = cell.grid_span.max(1) as usize;
            let cw: f32 = (grid_col..grid_col + span)
                .map(|g| col_ws.get(g).copied().unwrap_or(0.0))
                .sum::<f32>().max(1.0);
            grid_col += span;

            let m = cell_margins(table, cell);
            let inner_x = cell_x + m.left as f32 / 20.0 * scale;
            let inner_w = (cw - (m.left + m.right) as f32 / 20.0 * scale).max(1.0);
            let mut cell_abs = row_cursor + m.top as f32 / 20.0 * scale;

            let cell_parcel = lp_cell_parcel(
                fs, &cell.blocks, inner_x, inner_w, cell.no_wrap, &mut cell_abs,
                page_idx, content_h_px, page_top, margin_t_px, scale, inv,
            );

            lp_cells.push(LpTableCell {
                col_index: ci,
                col_span: span,
                row_span: 1,
                x: (cell_x - margin_l_px - tbl_indent_px) * inv,
                y: row_y_in_tbl * inv,
                width: cw * inv,
                height: rh * inv,
                parcel: cell_parcel,
            });
            cell_x += cw;
        }

        lp_rows.push(LpTableRow { y: row_y_in_tbl * inv, height: rh * inv, cells: lp_cells });
        row_cursor += rh;
    }

    *cursor += total_h;

    let tbl_w_px = col_ws.iter().sum::<f32>();
    Some(LpLine {
        y: tbl_page_y_px * inv,
        width: tbl_w_px * inv,
        height: total_h * inv,
        space_before: 0.0,
        space_after: 0.0,
        is_first_line_of_para: true,
        is_last_line_of_para: true,
        content: LpLineContent::Table(LpTable {
            width: tbl_w_px * inv,
            height: total_h * inv,
            columns,
            rows: lp_rows,
        }),
    })
}

fn lp_cell_parcel(
    fs: &mut FontSystem, blocks: &[Block],
    inner_x: f32, inner_w: f32, no_wrap: bool, cur_abs: &mut f32,
    _page_idx: usize, _content_h_px: f32, _page_top: f32,
    _margin_t_px: f32, scale: f32, inv: f32,
) -> Option<LpParcel> {
    let parcel_start = *cur_abs;
    let mut lines: Vec<LpLine> = Vec::new();

    for block in blocks {
        match block {
            Block::Paragraph(p) => {
                *cur_abs += para_space_before(p, scale);
                let indent_l = p.indent_left_pt * scale;
                let eff_x = inner_x + indent_l;
                let eff_w = (inner_w - indent_l - p.indent_right_pt * scale).max(1.0);
                let shape_w = if no_wrap && p.align == crate::model::Align::Left { 99999.0 } else { eff_w };

                let buf = layout_paragraph(fs, p, shape_w, scale);
                let lh = buf.metrics().line_height;
                let all_runs: Vec<_> = buf.layout_runs().collect();
                let n = all_runs.len();

                for (li, lr) in all_runs.iter().enumerate() {
                    let line_abs_top = *cur_abs + lr.line_top;
                    let line_y_in_parcel = (line_abs_top - parcel_start) * inv;
                    let baseline_off = (lr.line_y - lr.line_top) * inv;
                    let font_sz_px = lr.glyphs.first().map(|g| g.font_size)
                        .unwrap_or(p.mark_style.size_pt * scale);
                    let font_sz_pt = font_sz_px * inv;
                    let ascent_px = lr.line_y - lr.line_top;
                    let descent_px = lh - ascent_px;

                    let glyphs: Vec<LpGlyph> = lr.glyphs.iter().map(|g| LpGlyph {
                        x: g.x * inv, y: 0.0, advance: g.w * inv, offset: g.start,
                    }).collect();

                    let lp_run = LpRun {
                        x: (eff_x - inner_x) * inv,
                        width: lr.line_w * inv,
                        transform: LpTransform::identity(),
                        content: LpRunContent::Glyphs {
                            text: lr.text.to_string(),
                            font_size: font_sz_pt,
                            ascent: ascent_px * inv,
                            descent: descent_px * inv,
                            glyphs,
                        },
                    };
                    lines.push(LpLine {
                        y: line_y_in_parcel,
                        width: lr.line_w * inv,
                        height: lh * inv,
                        space_before: 0.0,
                        space_after: 0.0,
                        is_first_line_of_para: li == 0,
                        is_last_line_of_para: li + 1 == n,
                        content: LpLineContent::RunList(LpRunList {
                            baseline: baseline_off,
                            width: lr.line_w * inv,
                            height: lh * inv,
                            runs: vec![lp_run],
                        }),
                    });
                }
                *cur_abs += n as f32 * lh;
                *cur_abs += para_space_after(p, scale);
            }
            Block::Table(_) => {}
            Block::PageBreak => {}
        }
    }

    if lines.is_empty() {
        None
    } else {
        Some(LpParcel {
            x: 0.0, y: 0.0,
            width: inner_w * inv,
            height: (*cur_abs - parcel_start) * inv,
            lines,
        })
    }
}

