//! XLSX (OOXML SpreadsheetML) parser.

use std::collections::{HashMap, HashSet};
use std::io::Read;

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::model::{
    Align, Block, BorderLine, BorderStyle, CellMargins, Document, PageGroup,
    Paragraph, Run, RunStyle, Table, TableCell, TableRow, VAlign,
};

const TWIPS_PER_PT: f32 = 20.0;
const DEFAULT_ROW_PT: f32 = 15.0;
// Default column char units per Excel spec
const DEFAULT_COL_CHARS: f64 = 8.43;
// Grid line color (light gray, like Excel default grid)
const GRID_COLOR: [u8; 3] = [217, 217, 217];
// Max Digit Width in pixels for Calibri 11pt at 96 DPI (ECMA-376 reference baseline)
const CALIBRI_11_MDW_PX: f64 = 7.0;

/// MDW (Max Digit Width) in pixels at 11pt, 96 DPI for common spreadsheet fonts.
/// Values measured empirically at 96 DPI. Unknown fonts fall back to Calibri baseline.
fn font_mdw_11pt(name: &str) -> f64 {
    match name.to_ascii_lowercase().as_str() {
        "calibri" | "" => 7.0,
        "arial" | "arial narrow" | "helvetica" | "helvetica neue" => 7.0,
        "times new roman" | "times" => 5.9,
        "courier new" | "courier" => 8.4,
        "verdana" => 7.5,
        "tahoma" => 6.5,
        "trebuchet ms" => 6.5,
        "georgia" => 6.4,
        "comic sans ms" => 7.5,
        "century gothic" => 7.0,
        "garamond" => 5.5,
        "book antiqua" | "palatino linotype" => 6.0,
        "cambria" => 6.8,
        "consolas" => 8.2,
        "lucida console" => 7.8,
        "impact" => 8.0,
        "symbol" => 7.0,
        _ => 7.0,
    }
}

// ── text helpers ─────────────────────────────────────────────────────────────

// Some XLSX files store double-encoded entities: &amp;amp; → &amp; → &
// or &amp;#039; → &#039; → '. Apply a second unescape pass if needed.
fn unescape_double(s: std::borrow::Cow<str>) -> String {
    if s.contains('&') {
        match quick_xml::escape::unescape(s.as_ref()) {
            Ok(decoded) => decoded.into_owned(),
            Err(_) => s.into_owned(),
        }
    } else {
        s.into_owned()
    }
}

// Excel "auto color": when font color = lt1 (white, theme[1]), Excel shows black on light
// backgrounds and white on dark backgrounds. This matches the "Automatic" text color behavior.
fn auto_color(font_color: [u8; 3], fill_bg: Option<[u8; 3]>) -> [u8; 3] {
    if font_color != [255, 255, 255] {
        return font_color;
    }
    // font is white (theme lt1 = automatic) — pick black or white by background luminance
    let bg = fill_bg.unwrap_or([255, 255, 255]);
    // perceived luminance: simple 0-255 average (fast, good enough for threshold)
    let lum = (bg[0] as u32 + bg[1] as u32 + bg[2] as u32) / 3;
    if lum > 128 {
        [0, 0, 0]   // light bg → black text
    } else {
        [255, 255, 255] // dark bg → keep white
    }
}

// ── attribute helper ─────────────────────────────────────────────────────────

fn attr_str(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    for a in e.attributes().flatten() {
        if a.key.local_name().as_ref() == key {
            return Some(String::from_utf8_lossy(&a.value).into_owned());
        }
    }
    None
}

// ── unit conversion ──────────────────────────────────────────────────────────

/// Excel character units → points.
/// Uses the ECMA-376 formula: TRUNC((chars * MDW + 5) / MDW * 256) / 256 * MDW * 0.75
/// where MDW = max digit width in pixels (7px for Calibri 11pt at 96 DPI, scaled by font size ratio).
fn col_chars_to_pt(chars: f64, mdw_scale: f32) -> f32 {
    let mdw_px = CALIBRI_11_MDW_PX * mdw_scale as f64;
    if mdw_px <= 0.0 { return 0.0; }
    let w_px = ((chars * mdw_px + 5.0) / mdw_px * 256.0).trunc() / 256.0 * mdw_px;
    (w_px * 0.75) as f32
}

// ── cell address parsing ─────────────────────────────────────────────────────

/// "A"→0, "Z"→25, "AA"→26 (0-based).
fn col_letter_to_idx(col: &str) -> usize {
    let mut result = 0usize;
    for b in col.bytes() {
        result = result * 26 + (b.to_ascii_uppercase() - b'A' + 1) as usize;
    }
    result.saturating_sub(1)
}

/// "B3" → (row=2, col=1) 0-based. None on invalid input.
fn parse_cell_ref(r: &str) -> Option<(usize, usize)> {
    let col_end = r.bytes().take_while(|b| b.is_ascii_alphabetic()).count();
    if col_end == 0 || col_end >= r.len() {
        return None;
    }
    let row: usize = r[col_end..].parse().ok()?;
    if row == 0 {
        return None;
    }
    Some((row - 1, col_letter_to_idx(&r[..col_end])))
}

/// "A1:C3" → ((0,0),(2,2)) 0-based.
fn parse_range(s: &str) -> Option<((usize, usize), (usize, usize))> {
    let mut parts = s.splitn(2, ':');
    let start = parse_cell_ref(parts.next()?)?;
    let end = parse_cell_ref(parts.next()?)?;
    Some((start, end))
}

// ── color parsing ────────────────────────────────────────────────────────────

/// "FFRRGGBB" or "RRGGBB" (with optional leading #) → [r,g,b].
fn parse_color_argb(s: &str) -> Option<[u8; 3]> {
    let s = s.trim_start_matches('#');
    if s.len() == 8 {
        let r = u8::from_str_radix(&s[2..4], 16).ok()?;
        let g = u8::from_str_radix(&s[4..6], 16).ok()?;
        let b = u8::from_str_radix(&s[6..8], 16).ok()?;
        Some([r, g, b])
    } else if s.len() == 6 {
        let r = u8::from_str_radix(&s[0..2], 16).ok()?;
        let g = u8::from_str_radix(&s[2..4], 16).ok()?;
        let b = u8::from_str_radix(&s[4..6], 16).ok()?;
        Some([r, g, b])
    } else {
        None
    }
}

// ── theme color resolution ───────────────────────────────────────────────────

/// Parse `xl/theme/theme1.xml` and return the 12 standard theme colors.
/// Index order (per ECMA-376 DrawingML): dk1,lt1,dk2,lt2,accent1..6,hlink,folHlink.
fn parse_theme_colors(bytes: &[u8]) -> [Option<[u8; 3]>; 12] {
    let mut colors: [Option<[u8; 3]>; 12] = [None; 12];
    let xml = match read_zip_entry(bytes, "xl/theme/theme1.xml") {
        Some(s) => s,
        None => return colors,
    };
    let order = [
        "dk1", "lt1", "dk2", "lt2",
        "accent1", "accent2", "accent3", "accent4", "accent5", "accent6",
        "hlink", "folHlink",
    ];
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);
    let mut cur_idx: Option<usize> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let local = e.local_name();
                let name = std::str::from_utf8(local.as_ref()).unwrap_or("");
                if let Some(idx) = order.iter().position(|&n| n == name) {
                    cur_idx = Some(idx);
                } else if let Some(idx) = cur_idx {
                    match local.as_ref() {
                        b"srgbClr" => {
                            if let Some(v) = attr_str(e, b"val") {
                                colors[idx] = parse_color_argb(&v);
                            }
                        }
                        b"sysClr" => {
                            if let Some(v) = attr_str(e, b"lastClr") {
                                colors[idx] = parse_color_argb(&v);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let local_end = e.local_name();
                let name_end = std::str::from_utf8(local_end.as_ref()).unwrap_or("");
                if order.iter().any(|&n| n == name_end) {
                    cur_idx = None;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    colors
}

/// Apply Excel tint to a base color.
/// Positive tint blends toward white, negative toward black.
fn apply_tint(color: [u8; 3], tint: f64) -> [u8; 3] {
    if tint == 0.0 { return color; }
    let apply = |c: u8| -> u8 {
        if tint > 0.0 {
            (c as f64 + (255.0 - c as f64) * tint).round().clamp(0.0, 255.0) as u8
        } else {
            (c as f64 * (1.0 + tint)).round().clamp(0.0, 255.0) as u8
        }
    };
    [apply(color[0]), apply(color[1]), apply(color[2])]
}

/// Resolve color from rgb/theme/tint attributes into [r,g,b].
fn resolve_color(
    rgb: Option<String>,
    theme: Option<String>,
    tint: Option<String>,
    theme_colors: &[Option<[u8; 3]>; 12],
) -> Option<[u8; 3]> {
    if let Some(rgb_str) = rgb {
        return parse_color_argb(&rgb_str);
    }
    if let Some(theme_str) = theme {
        if let Ok(idx) = theme_str.parse::<usize>() {
            if let Some(Some(base)) = theme_colors.get(idx) {
                let t = tint.and_then(|t| t.parse::<f64>().ok()).unwrap_or(0.0);
                return Some(apply_tint(*base, t));
            }
        }
    }
    None
}

// ── number formatting ────────────────────────────────────────────────────────

/// Returns (display_text, is_numeric).
/// is_numeric drives default right-alignment when cell style is "general".
fn format_number(val: f64, num_fmt_id: u32, custom: Option<&str>) -> (String, bool) {
    match num_fmt_id {
        0 => {
            // General
            if val == val.floor() && val.abs() < 1e15 {
                (format!("{}", val as i64), true)
            } else {
                (format_float_clean(val), true)
            }
        }
        1 => (format!("{}", val.round() as i64), true),
        2 => (format!("{:.2}", val), true),
        3 => (format_with_thousands(val.round() as i64, 0), true),
        4 => (format_with_thousands_dec(val, 2), true),
        9 => (format!("{:.0}%", val * 100.0), true),
        10 => (format!("{:.2}%", val * 100.0), true),
        11 => (format!("{:.2E}", val), true),
        14..=22 => (excel_serial_to_date(val), false),
        37 | 38 => {
            if val < 0.0 {
                (format!("({})", format_with_thousands(val.abs().round() as i64, 0)), true)
            } else {
                (format_with_thousands(val.round() as i64, 0), true)
            }
        }
        39 | 40 => {
            if val < 0.0 {
                (format!("({:.2})", val.abs()), true)
            } else {
                (format!("{:.2}", val), true)
            }
        }
        49 => (format!("{}", val), false),
        _ => {
            if let Some(fmt) = custom {
                format_with_pattern(val, fmt)
            } else if val == val.floor() && val.abs() < 1e15 {
                (format!("{}", val as i64), true)
            } else {
                (format_float_clean(val), true)
            }
        }
    }
}

fn format_float_clean(val: f64) -> String {
    let s = format!("{}", val);
    // Remove unnecessary trailing zeros after decimal
    if s.contains('.') {
        let s = s.trim_end_matches('0');
        let s = s.trim_end_matches('.');
        s.to_string()
    } else {
        s
    }
}

fn format_with_thousands(n: i64, _dec: usize) -> String {
    let abs = n.unsigned_abs();
    let s = format!("{}", abs);
    let with_sep: String = s
        .chars()
        .rev()
        .enumerate()
        .flat_map(|(i, c)| {
            if i > 0 && i % 3 == 0 {
                vec![',', c]
            } else {
                vec![c]
            }
        })
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    if n < 0 {
        format!("-{}", with_sep)
    } else {
        with_sep
    }
}

fn format_with_thousands_dec(val: f64, decimals: usize) -> String {
    let int_part = format_with_thousands(val.trunc() as i64, 0);
    let frac = format!("{:.prec$}", val.fract().abs(), prec = decimals);
    format!("{}{}", int_part, &frac[1..]) // skip leading "0"
}

fn excel_serial_to_date(serial: f64) -> String {
    // Excel serial: 1 = 1900-01-01 (with leap year bug: 60 = fictitious 1900-02-29)
    let s = serial as i64;
    if s <= 0 {
        return format!("{}", serial);
    }
    // Adjust for leap year bug (serial 60 is invalid, treat 61+ as 1 day offset)
    let adjusted = if s >= 61 { s - 1 } else { s };
    // Days since 1899-12-31
    // 25568 days from 1900-01-01 to 1970-01-01 (accounting for leap year bug)
    let days_from_epoch = adjusted - 25569;
    let mut year = 1970i32;
    let mut days = days_from_epoch as i32;
    while days < 0 {
        year -= 1;
        days += if is_leap(year) { 366 } else { 365 };
    }
    loop {
        let diy = if is_leap(year) { 366 } else { 365 };
        if days < diy {
            break;
        }
        days -= diy;
        year += 1;
    }
    let months = [
        31i32,
        if is_leap(year) { 29 } else { 28 },
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
    ];
    let mut month = 1u32;
    for &m in &months {
        if days < m {
            break;
        }
        days -= m;
        month += 1;
    }
    format!("{:02}/{:02}/{}", days + 1, month, year)
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn format_with_pattern(val: f64, pattern: &str) -> (String, bool) {
    let p = pattern.to_ascii_lowercase();
    if p.contains('%') {
        let multiplied = val * 100.0;
        if p.contains(".00") || p.contains("0.0") {
            return (format!("{:.2}%", multiplied), true);
        }
        return (format!("{:.0}%", multiplied), true);
    }
    if p.contains("yyyy") || p.contains("yy") || (p.contains("mm") && p.contains("dd")) {
        return (excel_serial_to_date(val), false);
    }
    // Count decimal places from format pattern
    let decimals = if let Some(pos) = pattern.find('.') {
        pattern[pos + 1..]
            .chars()
            .take_while(|c| *c == '0' || *c == '#')
            .count()
    } else {
        0
    };
    if decimals > 0 {
        (format!("{:.prec$}", val, prec = decimals), true)
    } else if val == val.floor() && val.abs() < 1e15 {
        (format!("{}", val as i64), true)
    } else {
        (format_float_clean(val), true)
    }
}

// ── ZIP helpers ──────────────────────────────────────────────────────────────

fn read_zip_entry(bytes: &[u8], name: &str) -> Option<String> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).ok()?;
    let mut entry = archive.by_name(name).ok()?;
    let mut content = String::new();
    entry.read_to_string(&mut content).ok()?;
    Some(content)
}

// ── format detection ─────────────────────────────────────────────────────────

pub fn is_xlsx(bytes: &[u8]) -> bool {
    if bytes.len() < 4 || &bytes[0..2] != b"PK" {
        return false;
    }
    // Fast scan: check if the ZIP central directory contains "xl/workbook.xml"
    // Fallback when zip crate can't open the archive for any reason.
    let needle = b"xl/workbook.xml";
    if bytes.windows(needle.len()).any(|w| w == needle) {
        return true;
    }
    read_zip_entry(bytes, "xl/workbook.xml").is_some()
}

// ── font declarations for prefetch ───────────────────────────────────────────

pub fn extract_font_declarations(bytes: &[u8]) -> Vec<String> {
    let xml = match read_zip_entry(bytes, "xl/styles.xml") {
        Some(s) => s,
        None => return Vec::new(),
    };
    let mut names: Vec<String> = Vec::new();
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                if e.local_name().as_ref() == b"name" {
                    if let Some(v) = attr_str(e, b"val") {
                        if !v.is_empty() && !names.contains(&v) {
                            names.push(v);
                        }
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    names
}

// ── internal style types ─────────────────────────────────────────────────────

#[derive(Clone)]
struct XlFont {
    bold: bool,
    italic: bool,
    underline: bool,
    size_pt: f32,
    color: [u8; 3],
    name: String,
}

impl Default for XlFont {
    fn default() -> Self {
        XlFont {
            bold: false,
            italic: false,
            underline: false,
            size_pt: 11.0,
            color: [0, 0, 0],
            name: String::new(),
        }
    }
}

#[derive(Clone, Default)]
struct XlFill {
    bg_color: Option<[u8; 3]>,
}

#[derive(Clone, Default)]
struct XlBorderEdge {
    style: String, // "thin", "medium", "thick", "hair", "dashed", ...
    color: [u8; 3],
}

#[derive(Clone, Default)]
struct XlBorder {
    left: XlBorderEdge,
    right: XlBorderEdge,
    top: XlBorderEdge,
    bottom: XlBorderEdge,
}

#[derive(Clone)]
struct XlCellXf {
    font_id: usize,
    fill_id: usize,
    border_id: usize,
    num_fmt_id: u32,
    h_align: Align,
    h_align_general: bool, // "general" = auto-detect right for numbers, left for text
    wrap_text: bool,
    v_align: VAlign,
    // ECMA-376 inheritance fields
    xf_id: usize,          // index into cell_style_xfs (base named style)
    apply_font: bool,
    apply_fill: bool,
    apply_border: bool,
    apply_alignment: bool,
    apply_num_fmt: bool,
}

impl Default for XlCellXf {
    fn default() -> Self {
        XlCellXf {
            font_id: 0,
            fill_id: 0,
            border_id: 0,
            num_fmt_id: 0,
            h_align: Align::Left,
            h_align_general: true,
            wrap_text: false,
            v_align: VAlign::Bottom,
            xf_id: 0,
            apply_font: true,
            apply_fill: true,
            apply_border: true,
            apply_alignment: true,
            apply_num_fmt: true,
        }
    }
}

struct XlStyles {
    fonts: Vec<XlFont>,
    fills: Vec<XlFill>,
    borders: Vec<XlBorder>,
    cell_xfs: Vec<XlCellXf>,
    cell_style_xfs: Vec<XlCellXf>, // base named styles (cellStyleXfs)
    num_fmts: HashMap<u32, String>,
    theme_colors: [Option<[u8; 3]>; 12],
    /// Scale factor for column-width conversion: workbook_default_font_size / 11.0.
    /// Calibri/Arial 11pt (Excel default) = 1.0. Arial 16pt = 16/11 ≈ 1.455.
    col_mdw_scale: f32,
}

impl XlStyles {
    fn get_font(&self, idx: usize) -> &XlFont {
        self.fonts.get(idx).unwrap_or_else(|| &self.fonts[0])
    }
    fn get_fill(&self, idx: usize) -> &XlFill {
        self.fills.get(idx).unwrap_or_else(|| &self.fills[0])
    }
    fn get_border(&self, idx: usize) -> &XlBorder {
        self.borders.get(idx).unwrap_or_else(|| &self.borders[0])
    }
    /// Resolve cell format with ECMA-376 inheritance from cellStyleXfs.
    fn resolve_xf(&self, idx: usize) -> XlCellXf {
        let xf = self.cell_xfs.get(idx).cloned().unwrap_or_default();
        let base = self.cell_style_xfs.get(xf.xf_id).cloned().unwrap_or_default();
        XlCellXf {
            font_id: if xf.apply_font { xf.font_id } else { base.font_id },
            fill_id: if xf.apply_fill { xf.fill_id } else { base.fill_id },
            border_id: if xf.apply_border { xf.border_id } else { base.border_id },
            num_fmt_id: if xf.apply_num_fmt { xf.num_fmt_id } else { base.num_fmt_id },
            h_align: if xf.apply_alignment { xf.h_align } else { base.h_align },
            h_align_general: if xf.apply_alignment { xf.h_align_general } else { base.h_align_general },
            wrap_text: if xf.apply_alignment { xf.wrap_text } else { base.wrap_text },
            v_align: if xf.apply_alignment { xf.v_align } else { base.v_align },
            ..xf
        }
    }
}

// ── cell data ────────────────────────────────────────────────────────────────

struct XlCell {
    text: String,
    style_idx: usize,
    is_numeric: bool,
    /// Non-empty for inlineStr cells with per-run <rPr> formatting.
    rich_runs: Vec<crate::model::Run>,
}

// ── workbook.xml → sheet list ────────────────────────────────────────────────

fn parse_workbook(bytes: &[u8]) -> Vec<(String, String)> {
    let xml = match read_zip_entry(bytes, "xl/workbook.xml") {
        Some(s) => s,
        None => return Vec::new(),
    };
    let mut sheets = Vec::new();
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                if e.local_name().as_ref() == b"sheet" {
                    let name = attr_str(e, b"name").unwrap_or_else(|| "Sheet".to_string());
                    // r:id attribute — local name is "id"
                    let rid = attr_str(e, b"id").unwrap_or_default();
                    sheets.push((name, rid));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    sheets
}

// ── workbook.xml.rels → r:id → file path ────────────────────────────────────

fn parse_workbook_rels(bytes: &[u8]) -> HashMap<String, String> {
    let xml = match read_zip_entry(bytes, "xl/_rels/workbook.xml.rels") {
        Some(s) => s,
        None => return HashMap::new(),
    };
    let mut map = HashMap::new();
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                if e.local_name().as_ref() == b"Relationship" {
                    let id = attr_str(e, b"Id").unwrap_or_default();
                    let target = attr_str(e, b"Target").unwrap_or_default();
                    if id.is_empty() || target.is_empty() {
                        continue;
                    }
                    let path = if target.starts_with('/') {
                        target.trim_start_matches('/').to_string()
                    } else if target.starts_with("xl/") || target.starts_with("..") {
                        target
                    } else {
                        format!("xl/{}", target)
                    };
                    map.insert(id, path);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    map
}

// ── sharedStrings.xml → string table ────────────────────────────────────────

/// Returns (plain_text, rich_runs) per shared string.
/// rich_runs is empty when the string has no per-run formatting.
fn parse_shared_strings(bytes: &[u8]) -> Vec<(String, Vec<crate::model::Run>)> {
    let xml = match read_zip_entry(bytes, "xl/sharedStrings.xml") {
        Some(s) => s,
        None => return Vec::new(),
    };
    let mut strings = Vec::new();
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(false);

    let mut in_si = false;
    let mut in_r = false;
    let mut in_t = false;
    let mut in_rpr = false;
    let mut plain_buf = String::new();
    let mut runs: Vec<crate::model::Run> = Vec::new();
    let mut run_text = String::new();
    let mut run_bold = false;
    let mut run_italic = false;
    let mut run_underline = false;
    let mut run_size: Option<f32> = None;
    let mut run_color: [u8; 3] = [0, 0, 0];
    let mut run_font: Option<String> = None;
    let mut is_rich = false; // true if si contains <r> elements

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => match e.local_name().as_ref() {
                b"si" => {
                    in_si = true;
                    plain_buf.clear();
                    runs.clear();
                    is_rich = false;
                }
                b"r" if in_si => {
                    in_r = true;
                    is_rich = true;
                    run_text.clear();
                    run_bold = false;
                    run_italic = false;
                    run_underline = false;
                    run_size = None;
                    run_color = [0, 0, 0];
                    run_font = None;
                }
                b"rPr" if in_r => { in_rpr = true; }
                b"b" if in_rpr => { run_bold = true; }
                b"i" if in_rpr => { run_italic = true; }
                b"u" if in_rpr => { run_underline = true; }
                b"sz" if in_rpr => {
                    if let Some(v) = attr_str(e, b"val") {
                        run_size = v.parse().ok();
                    }
                }
                b"color" if in_rpr => {
                    // simple rgb only (no theme in shared strings in practice)
                    if let Some(v) = attr_str(e, b"rgb") {
                        if let Some(c) = parse_color_argb(&v) {
                            run_color = c;
                        }
                    }
                }
                b"rFont" if in_rpr => {
                    run_font = attr_str(e, b"val");
                }
                b"t" if in_si => { in_t = true; }
                _ => {}
            },
            Ok(Event::Empty(ref e)) => match e.local_name().as_ref() {
                b"b" if in_rpr => { run_bold = true; }
                b"i" if in_rpr => { run_italic = true; }
                b"u" if in_rpr => { run_underline = true; }
                b"sz" if in_rpr => {
                    if let Some(v) = attr_str(e, b"val") {
                        run_size = v.parse().ok();
                    }
                }
                b"color" if in_rpr => {
                    if let Some(v) = attr_str(e, b"rgb") {
                        if let Some(c) = parse_color_argb(&v) {
                            run_color = c;
                        }
                    }
                }
                b"rFont" if in_rpr => {
                    run_font = attr_str(e, b"val");
                }
                _ => {}
            },
            Ok(Event::End(ref e)) => match e.local_name().as_ref() {
                b"si" => {
                    let plain = plain_buf.trim_end_matches('\n').to_string();
                    strings.push((plain, if is_rich { runs.clone() } else { vec![] }));
                    in_si = false;
                }
                b"r" if in_si => {
                    plain_buf.push_str(&run_text);
                    runs.push(crate::model::Run {
                        text: run_text.clone(),
                        style: crate::model::RunStyle {
                            bold: run_bold,
                            italic: run_italic,
                            underline: run_underline,
                            strike: false,
                            size_pt: run_size.unwrap_or(11.0),
                            color: run_color,
                            font_name: run_font.clone(),
                            font_name_east_asia: None,
                            size_cs_pt: None,
                        },
                        inline_image: None,
                        is_page_number: false,
                    });
                    in_r = false;
                    in_rpr = false;
                    in_t = false;
                }
                b"rPr" => { in_rpr = false; }
                b"t" => { in_t = false; }
                _ => {}
            },
            Ok(Event::Text(ref e)) => {
                if in_t {
                    if let Ok(s) = e.unescape() {
                        let s = unescape_double(s);
                        if in_r {
                            run_text.push_str(&s);
                        } else {
                            plain_buf.push_str(&s);
                        }
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    strings
}

// ── styles.xml → XlStyles ───────────────────────────────────────────────────

fn border_edge_to_line(edge: &XlBorderEdge) -> Option<BorderLine> {
    let (size, style) = match edge.style.as_str() {
        "thin" => (2u32, BorderStyle::Single),
        "medium" => (4, BorderStyle::Single),
        "thick" => (8, BorderStyle::Single),
        "hair" => (1, BorderStyle::Single),
        "dashed" | "mediumDashed" | "dashDot" | "mediumDashDot" => (2, BorderStyle::Dashed),
        "dotted" => (1, BorderStyle::Dotted),
        "double" => (4, BorderStyle::Double),
        _ => return None,
    };
    let c = edge.color;
    Some(BorderLine {
        size_eighth_pt: size,
        color: c,
        style,
        explicit: true,
    })
}

fn parse_styles(bytes: &[u8]) -> XlStyles {
    let theme_colors = parse_theme_colors(bytes);

    let xml = match read_zip_entry(bytes, "xl/styles.xml") {
        Some(s) => s,
        None => {
            return XlStyles {
                fonts: vec![XlFont::default()],
                fills: vec![XlFill::default()],
                borders: vec![XlBorder::default()],
                cell_xfs: vec![XlCellXf::default()],
                cell_style_xfs: vec![XlCellXf::default()],
                num_fmts: HashMap::new(),
                theme_colors,
                col_mdw_scale: 1.0,
            };
        }
    };

    let mut fonts: Vec<XlFont> = Vec::new();
    let mut fills: Vec<XlFill> = Vec::new();
    let mut borders: Vec<XlBorder> = Vec::new();
    let mut cell_xfs: Vec<XlCellXf> = Vec::new();
    let mut cell_style_xfs: Vec<XlCellXf> = Vec::new();
    let mut num_fmts: HashMap<u32, String> = HashMap::new();

    // 0=none 1=fonts 2=fills 3=borders 4=cellXfs 5=numFmts 6=cellStyleXfs
    let mut section: u8 = 0;
    // whether we're inside a nested element of the section (font/fill/border/xf)
    let mut in_item = false;

    let mut cur_font = XlFont::default();
    let mut cur_fill = XlFill::default();
    let mut cur_fill_pattern = String::new();
    let mut cur_border = XlBorder::default();
    let mut cur_border_which: u8 = 0; // 1=left 2=right 3=top 4=bottom
    let mut cur_xf = XlCellXf::default();

    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);

    macro_rules! handle_start_end_tag {
        ($e:expr, $push:expr) => {
            let local = $e.local_name();
            let local_ref = local.as_ref();
            match section {
                1 => {
                    // fonts
                    match local_ref {
                        b"font" => {
                            cur_font = XlFont::default();
                            in_item = true;
                        }
                        b"b" if in_item => {
                            cur_font.bold = true;
                        }
                        b"i" if in_item => {
                            cur_font.italic = true;
                        }
                        b"u" if in_item => {
                            cur_font.underline = true;
                        }
                        b"sz" if in_item => {
                            if let Some(v) = attr_str($e, b"val") {
                                cur_font.size_pt = v.parse().unwrap_or(11.0);
                            }
                        }
                        b"name" if in_item => {
                            if let Some(v) = attr_str($e, b"val") {
                                cur_font.name = v;
                            }
                        }
                        b"color" if in_item => {
                            if let Some(c) = resolve_color(
                                attr_str($e, b"rgb"),
                                attr_str($e, b"theme"),
                                attr_str($e, b"tint"),
                                &theme_colors,
                            ) {
                                cur_font.color = c;
                            }
                        }
                        _ => {}
                    }
                    if $push && local_ref == b"font" {
                        fonts.push(cur_font.clone());
                        in_item = false;
                    }
                }
                2 => {
                    // fills
                    match local_ref {
                        b"fill" => {
                            cur_fill = XlFill::default();
                            cur_fill_pattern = String::new();
                            in_item = true;
                        }
                        b"patternFill" if in_item => {
                            cur_fill_pattern = attr_str($e, b"patternType").unwrap_or_default();
                        }
                        b"fgColor" if in_item => {
                            if cur_fill_pattern != "none" && cur_fill_pattern != "gray125" {
                                cur_fill.bg_color = resolve_color(
                                    attr_str($e, b"rgb"),
                                    attr_str($e, b"theme"),
                                    attr_str($e, b"tint"),
                                    &theme_colors,
                                );
                            }
                        }
                        b"bgColor" if in_item => {
                            // bgColor is the pattern background (for patterns like gray125)
                            // for solid fills, fgColor is the actual background
                        }
                        _ => {}
                    }
                    if $push && local_ref == b"fill" {
                        fills.push(cur_fill.clone());
                        in_item = false;
                    }
                }
                3 => {
                    // borders
                    match local_ref {
                        b"border" => {
                            cur_border = XlBorder::default();
                            cur_border_which = 0;
                            in_item = true;
                        }
                        b"left" if in_item => {
                            cur_border_which = 1;
                            if let Some(s) = attr_str($e, b"style") {
                                cur_border.left.style = s;
                            }
                        }
                        b"right" if in_item => {
                            cur_border_which = 2;
                            if let Some(s) = attr_str($e, b"style") {
                                cur_border.right.style = s;
                            }
                        }
                        b"top" if in_item => {
                            cur_border_which = 3;
                            if let Some(s) = attr_str($e, b"style") {
                                cur_border.top.style = s;
                            }
                        }
                        b"bottom" if in_item => {
                            cur_border_which = 4;
                            if let Some(s) = attr_str($e, b"style") {
                                cur_border.bottom.style = s;
                            }
                        }
                        b"color" if in_item => {
                            if let Some(c) = resolve_color(
                                attr_str($e, b"rgb"),
                                attr_str($e, b"theme"),
                                attr_str($e, b"tint"),
                                &theme_colors,
                            ) {
                                match cur_border_which {
                                    1 => cur_border.left.color = c,
                                    2 => cur_border.right.color = c,
                                    3 => cur_border.top.color = c,
                                    4 => cur_border.bottom.color = c,
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                    if $push && local_ref == b"border" {
                        borders.push(cur_border.clone());
                        in_item = false;
                    }
                }
                4 | 6 => {
                    // cellXfs (4) or cellStyleXfs (6)
                    match local_ref {
                        b"xf" => {
                            cur_xf = XlCellXf::default();
                            if let Some(v) = attr_str($e, b"fontId") {
                                cur_xf.font_id = v.parse().unwrap_or(0);
                            }
                            if let Some(v) = attr_str($e, b"fillId") {
                                cur_xf.fill_id = v.parse().unwrap_or(0);
                            }
                            if let Some(v) = attr_str($e, b"borderId") {
                                cur_xf.border_id = v.parse().unwrap_or(0);
                            }
                            if let Some(v) = attr_str($e, b"numFmtId") {
                                cur_xf.num_fmt_id = v.parse().unwrap_or(0);
                            }
                            if let Some(v) = attr_str($e, b"xfId") {
                                cur_xf.xf_id = v.parse().unwrap_or(0);
                            }
                            // apply_* default=true; explicit "0" means inherit from base
                            cur_xf.apply_font = attr_str($e, b"applyFont").as_deref() != Some("0");
                            cur_xf.apply_fill = attr_str($e, b"applyFill").as_deref() != Some("0");
                            cur_xf.apply_border = attr_str($e, b"applyBorder").as_deref() != Some("0");
                            cur_xf.apply_alignment = attr_str($e, b"applyAlignment").as_deref() != Some("0");
                            cur_xf.apply_num_fmt = attr_str($e, b"applyNumberFormat").as_deref() != Some("0");
                            in_item = true;
                            if $push {
                                if section == 6 {
                                    cell_style_xfs.push(cur_xf.clone());
                                } else {
                                    cell_xfs.push(cur_xf.clone());
                                }
                                in_item = false;
                            }
                        }
                        b"alignment" if in_item => {
                            if let Some(v) = attr_str($e, b"horizontal") {
                                match v.as_str() {
                                    "center" => {
                                        cur_xf.h_align = Align::Center;
                                        cur_xf.h_align_general = false;
                                    }
                                    "right" => {
                                        cur_xf.h_align = Align::Right;
                                        cur_xf.h_align_general = false;
                                    }
                                    "left" => {
                                        cur_xf.h_align = Align::Left;
                                        cur_xf.h_align_general = false;
                                    }
                                    "justify" | "distributed" => {
                                        cur_xf.h_align = Align::Justify;
                                        cur_xf.h_align_general = false;
                                    }
                                    _ => {} // "general" = keep h_align_general=true
                                }
                            }
                            if attr_str($e, b"wrapText").as_deref() == Some("1") {
                                cur_xf.wrap_text = true;
                            }
                            if let Some(v) = attr_str($e, b"vertical") {
                                cur_xf.v_align = match v.as_str() {
                                    "top" => VAlign::Top,
                                    "center" => VAlign::Center,
                                    _ => VAlign::Bottom,
                                };
                            }
                        }
                        _ => {}
                    }
                }
                5 => {
                    // numFmts
                    if local_ref == b"numFmt" {
                        if let (Some(id_str), Some(code)) =
                            (attr_str($e, b"numFmtId"), attr_str($e, b"formatCode"))
                        {
                            if let Ok(id) = id_str.parse::<u32>() {
                                num_fmts.insert(id, code);
                            }
                        }
                    }
                }
                _ => {}
            }
        };
    }

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let local = e.local_name();
                let local_ref = local.as_ref();
                match local_ref {
                    b"fonts" => {
                        section = 1;
                        in_item = false;
                    }
                    b"fills" => {
                        section = 2;
                        in_item = false;
                    }
                    b"borders" => {
                        section = 3;
                        in_item = false;
                    }
                    b"cellXfs" => {
                        section = 4;
                        in_item = false;
                    }
                    b"numFmts" => {
                        section = 5;
                        in_item = false;
                    }
                    b"cellStyleXfs" => {
                        section = 6;
                        in_item = false;
                    }
                    _ => {
                        handle_start_end_tag!(e, false);
                    }
                }
            }
            Ok(Event::Empty(ref e)) => {
                let local = e.local_name();
                let local_ref = local.as_ref();
                match local_ref {
                    b"fonts" | b"fills" | b"borders" | b"cellXfs" | b"cellStyleXfs" | b"numFmts" => {}
                    _ => {
                        handle_start_end_tag!(e, true);
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let local = e.local_name();
                let local_ref = local.as_ref();
                match local_ref {
                    b"fonts" | b"fills" | b"borders" | b"cellXfs" | b"cellStyleXfs" | b"numFmts" => {
                        section = 0;
                        in_item = false;
                    }
                    b"font" if section == 1 => {
                        fonts.push(cur_font.clone());
                        in_item = false;
                    }
                    b"fill" if section == 2 => {
                        fills.push(cur_fill.clone());
                        in_item = false;
                    }
                    b"border" if section == 3 => {
                        borders.push(cur_border.clone());
                        in_item = false;
                    }
                    b"xf" if section == 4 => {
                        cell_xfs.push(cur_xf.clone());
                        in_item = false;
                    }
                    b"xf" if section == 6 => {
                        cell_style_xfs.push(cur_xf.clone());
                        in_item = false;
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    if fonts.is_empty() {
        fonts.push(XlFont::default());
    }
    if fills.is_empty() {
        fills.push(XlFill::default());
    }
    if borders.is_empty() {
        borders.push(XlBorder::default());
    }
    if cell_xfs.is_empty() {
        cell_xfs.push(XlCellXf::default());
    }
    if cell_style_xfs.is_empty() {
        cell_style_xfs.push(XlCellXf::default());
    }

    let col_mdw_scale = {
        let f = fonts.first().cloned().unwrap_or_default();
        (font_mdw_11pt(&f.name) / CALIBRI_11_MDW_PX * f.size_pt as f64 / 11.0) as f32
    };

    XlStyles {
        fonts,
        fills,
        borders,
        cell_xfs,
        cell_style_xfs,
        num_fmts,
        theme_colors,
        col_mdw_scale,
    }
}

// ── worksheet parser ─────────────────────────────────────────────────────────

struct ParsedSheet {
    max_row: usize,
    max_col: usize,
    col_widths: Vec<f32>,
    row_heights: Vec<f32>,
    cells: HashMap<(usize, usize), XlCell>,
    merges: Vec<((usize, usize), (usize, usize))>,
    default_col_chars: f64,
}

fn parse_sheet(
    bytes: &[u8],
    path: &str,
    shared_strings: &[(String, Vec<crate::model::Run>)],
    styles: &XlStyles,
) -> ParsedSheet {
    let xml = match read_zip_entry(bytes, path) {
        Some(s) => s,
        None => {
            return ParsedSheet {
                max_row: 0,
                max_col: 0,
                col_widths: Vec::new(),
                row_heights: Vec::new(),
                cells: HashMap::new(),
                merges: Vec::new(),
                default_col_chars: DEFAULT_COL_CHARS,
            };
        }
    };

    let mut max_row = 0usize;
    let mut max_col = 0usize;
    // per-column width overrides (index → pt)
    let mut col_width_map: HashMap<usize, f32> = HashMap::new();
    // per-row height overrides (index → pt)
    let mut row_height_map: HashMap<usize, f32> = HashMap::new();
    let mut cells: HashMap<(usize, usize), XlCell> = HashMap::new();
    let mut merges: Vec<((usize, usize), (usize, usize))> = Vec::new();

    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);

    // Cell parsing state
    let mut cur_row: usize = 0;
    let mut cur_col: usize = 0;
    let mut cur_cell_type: String = String::new();
    let mut cur_cell_style: usize = 0;
    let mut cur_cell_value: String = String::new();
    let mut cur_cell_formula: bool = false;
    let mut in_cell = false;
    let mut in_value = false;
    let mut in_formula = false;
    // inlineStr rich text run state
    let mut in_is_r = false;
    let mut in_is_rpr = false;
    let mut in_is_t = false;
    let mut cur_run_text = String::new();
    let mut cur_run_bold = false;
    let mut cur_run_italic = false;
    let mut cur_run_underline = false;
    let mut cur_run_color: [u8; 3] = [0, 0, 0];
    let mut cur_run_has_color = false;
    let mut cur_run_size: Option<f32> = None;
    let mut cur_run_font: Option<String> = None;
    let mut is_runs: Vec<crate::model::Run> = Vec::new();

    // Parse dimension to get initial max_row/max_col
    // Also extract defaultRowHeight and defaultColWidth from sheetFormatPr
    let mut default_row_pt = DEFAULT_ROW_PT;
    let mut default_col_chars = DEFAULT_COL_CHARS;
    {
        let xml2 = xml.as_str();
        if let Some(dim_start) = xml2.find("dimension ref=\"") {
            let rest = &xml2[dim_start + 15..];
            if let Some(dim_end) = rest.find('"') {
                let dim = &rest[..dim_end];
                if let Some((_, end)) = parse_range(dim) {
                    max_row = end.0 + 1;
                    max_col = end.1 + 1;
                }
            }
        }
        if let Some(fmt_start) = xml2.find("sheetFormatPr") {
            let rest = &xml2[fmt_start..];
            if let Some(tag_end) = rest.find('>') {
                let tag = &rest[..tag_end];
                if let Some(rh_start) = tag.find("defaultRowHeight=\"") {
                    let after = &tag[rh_start + 18..];
                    if let Some(rh_end) = after.find('"') {
                        if let Ok(rh) = after[..rh_end].parse::<f32>() {
                            if rh > 0.0 {
                                default_row_pt = rh;
                            }
                        }
                    }
                }
                if let Some(cw_start) = tag.find("defaultColWidth=\"") {
                    let after = &tag[cw_start + 17..];
                    if let Some(cw_end) = after.find('"') {
                        if let Ok(cw) = after[..cw_end].parse::<f64>() {
                            if cw > 0.0 {
                                default_col_chars = cw;
                            }
                        }
                    }
                }
            }
        }
    }

    // Helper: parse <c> element attributes and update cell position tracking
    macro_rules! parse_cell_start {
        ($e:expr) => {{
            cur_cell_type = attr_str($e, b"t").unwrap_or_default();
            cur_cell_style = attr_str($e, b"s")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            cur_cell_value.clear();
            cur_cell_formula = false;
            in_value = false;
            in_formula = false;
            if let Some(r) = attr_str($e, b"r") {
                if let Some((row, col)) = parse_cell_ref(&r) {
                    cur_row = row;
                    cur_col = col;
                    if row + 1 > max_row { max_row = row + 1; }
                    if col + 1 > max_col { max_col = col + 1; }
                }
            }
        }};
    }

    loop {
        match reader.read_event() {
            Ok(Event::Empty(ref e)) => {
                let local = e.local_name();
                let local_ref = local.as_ref();
                match local_ref {
                    b"col" => {
                        let min: usize = attr_str(e, b"min").and_then(|v| v.parse().ok()).unwrap_or(1);
                        let max: usize = attr_str(e, b"max").and_then(|v| v.parse().ok()).unwrap_or(min);
                        let width: f64 = attr_str(e, b"width").and_then(|v| v.parse().ok()).unwrap_or(default_col_chars);
                        let pt = col_chars_to_pt(width, styles.col_mdw_scale);
                        for col_idx in (min - 1)..max { col_width_map.insert(col_idx, pt); }
                    }
                    b"c" => {
                        // Self-closing <c/> — parse attrs then immediately finalize.
                        // These are typically empty cells with a style (fill color for gantt bars).
                        parse_cell_start!(e);
                        // Store the cell so fill colors and overflow checks work correctly.
                        // build_cell returns None for empty values, so we insert directly.
                        cells.insert((cur_row, cur_col), XlCell {
                            text: String::new(),
                            style_idx: cur_cell_style,
                            is_numeric: false,
                            rich_runs: vec![],
                        });
                        in_cell = false;
                    }
                    // rPr child elements (self-closing like <b val="false"/>)
                    b"rFont" if in_is_rpr => {
                        cur_run_font = attr_str(e, b"val");
                    }
                    b"b" if in_is_rpr => {
                        let v = attr_str(e, b"val").unwrap_or_else(|| "1".to_string());
                        cur_run_bold = v != "0" && v != "false";
                    }
                    b"i" if in_is_rpr => {
                        let v = attr_str(e, b"val").unwrap_or_else(|| "1".to_string());
                        cur_run_italic = v != "0" && v != "false";
                    }
                    b"u" if in_is_rpr => {
                        let v = attr_str(e, b"val").unwrap_or_else(|| "single".to_string());
                        cur_run_underline = v != "none";
                    }
                    b"sz" if in_is_rpr => {
                        cur_run_size = attr_str(e, b"val").and_then(|v| v.parse().ok());
                    }
                    b"color" if in_is_rpr => {
                        if let Some(rgb) = attr_str(e, b"rgb").and_then(|v| parse_color_argb(&v)) {
                            cur_run_color = rgb;
                            cur_run_has_color = true;
                        }
                    }
                    b"mergeCell" => {
                        if let Some(r) = attr_str(e, b"ref") {
                            if let Some(range) = parse_range(&r) { merges.push(range); }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Start(ref e)) => {
                let local = e.local_name();
                let local_ref = local.as_ref();

                match local_ref {
                    b"col" => {
                        let min: usize =
                            attr_str(e, b"min").and_then(|v| v.parse().ok()).unwrap_or(1);
                        let max: usize =
                            attr_str(e, b"max").and_then(|v| v.parse().ok()).unwrap_or(min);
                        let width: f64 =
                            attr_str(e, b"width").and_then(|v| v.parse().ok()).unwrap_or(default_col_chars);
                        let pt = col_chars_to_pt(width, styles.col_mdw_scale);
                        for col_idx in (min - 1)..(max) {
                            col_width_map.insert(col_idx, pt);
                        }
                    }
                    b"row" => {
                        let r: usize =
                            attr_str(e, b"r").and_then(|v| v.parse().ok()).unwrap_or(cur_row + 1);
                        cur_row = r - 1;
                        if cur_row + 1 > max_row {
                            max_row = cur_row + 1;
                        }
                        let custom_h = attr_str(e, b"customHeight")
                            .map(|v| v == "1" || v == "true")
                            .unwrap_or(false);
                        if let Some(ht) = attr_str(e, b"ht").and_then(|v| v.parse::<f32>().ok()) {
                            row_height_map.insert(cur_row, ht);
                        }
                        let _ = custom_h;
                    }
                    b"c" => {
                        parse_cell_start!(e);
                        is_runs.clear();
                        in_cell = true;
                    }
                    b"v" if in_cell => {
                        in_value = true;
                    }
                    b"f" if in_cell => {
                        in_formula = true;
                    }
                    b"r" if in_cell && cur_cell_type == "inlineStr" => {
                        in_is_r = true;
                        cur_run_text.clear();
                        cur_run_bold = false;
                        cur_run_italic = false;
                        cur_run_underline = false;
                        cur_run_has_color = false;
                        cur_run_color = [0, 0, 0];
                        cur_run_size = None;
                        cur_run_font = None;
                    }
                    b"rPr" if in_is_r => { in_is_rpr = true; }
                    b"rFont" if in_is_rpr => {
                        cur_run_font = attr_str(e, b"val");
                    }
                    b"b" if in_is_rpr => {
                        let v = attr_str(e, b"val").unwrap_or_else(|| "1".to_string());
                        cur_run_bold = v != "0" && v != "false";
                    }
                    b"i" if in_is_rpr => {
                        let v = attr_str(e, b"val").unwrap_or_else(|| "1".to_string());
                        cur_run_italic = v != "0" && v != "false";
                    }
                    b"u" if in_is_rpr => {
                        let v = attr_str(e, b"val").unwrap_or_else(|| "single".to_string());
                        cur_run_underline = v != "none";
                    }
                    b"sz" if in_is_rpr => {
                        cur_run_size = attr_str(e, b"val").and_then(|v| v.parse().ok());
                    }
                    b"color" if in_is_rpr => {
                        if let Some(rgb) = attr_str(e, b"rgb").and_then(|v| parse_color_argb(&v)) {
                            cur_run_color = rgb;
                            cur_run_has_color = true;
                        }
                    }
                    b"t" if in_cell => {
                        if in_is_r {
                            in_is_t = true;
                        } else {
                            // plain <t> inside <is> (no <r> wrapper) or other text
                            in_value = true;
                        }
                    }
                    b"mergeCell" => {
                        if let Some(r) = attr_str(e, b"ref") {
                            if let Some(range) = parse_range(&r) {
                                merges.push(range);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                let local = e.local_name();
                let local_ref = local.as_ref();
                match local_ref {
                    b"c" => {
                        if in_cell {
                            let cell = if !is_runs.is_empty() {
                                // Rich inlineStr: store runs; text = concatenation for overflow check
                                let full_text: String = is_runs.iter().map(|r| r.text.as_str()).collect();
                                Some(XlCell {
                                    text: full_text,
                                    style_idx: cur_cell_style,
                                    is_numeric: false,
                                    rich_runs: is_runs.drain(..).collect(),
                                })
                            } else {
                                build_cell(
                                    &cur_cell_type,
                                    &cur_cell_value,
                                    cur_cell_style,
                                    shared_strings,
                                    styles,
                                )
                            };
                            if let Some(cell) = cell {
                                cells.insert((cur_row, cur_col), cell);
                            }
                            is_runs.clear();
                            in_cell = false;
                            in_value = false;
                            in_formula = false;
                            in_is_r = false;
                            in_is_rpr = false;
                            in_is_t = false;
                        }
                    }
                    b"rPr" if in_is_rpr => {
                        in_is_rpr = false;
                    }
                    b"r" if in_is_r => {
                        // Finalize the current inlineStr run
                        let base_font = styles.get_font(styles.resolve_xf(cur_cell_style).font_id);
                        is_runs.push(crate::model::Run {
                            text: cur_run_text.clone(),
                            style: crate::model::RunStyle {
                                bold: cur_run_bold,
                                italic: cur_run_italic,
                                underline: cur_run_underline,
                                strike: false,
                                size_pt: cur_run_size.unwrap_or(base_font.size_pt),
                                color: if cur_run_has_color { cur_run_color } else { base_font.color },
                                font_name: cur_run_font.clone().or_else(|| {
                                    if base_font.name.is_empty() { None } else { Some(base_font.name.clone()) }
                                }),
                                font_name_east_asia: None,
                                size_cs_pt: None,
                            },
                            inline_image: None,
                            is_page_number: false,
                        });
                        in_is_r = false;
                        cur_run_text.clear();
                    }
                    b"t" => {
                        in_value = false;
                        in_is_t = false;
                    }
                    b"v" => {
                        in_value = false;
                    }
                    b"f" => {
                        in_formula = false;
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(ref e)) => {
                if in_is_t {
                    if let Ok(s) = e.unescape() {
                        cur_run_text.push_str(&unescape_double(s));
                    }
                } else if in_value && in_cell {
                    if let Ok(s) = e.unescape() {
                        cur_cell_value.push_str(&unescape_double(s));
                    }
                }
                // We ignore formula text (<f>) since we use cached value (<v>)
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    // Trim max_col/max_row to actual data extent.
    // max_col: cell counts as content if it has text OR visible fill (bg_color.is_some()).
    //   This keeps RBAC fill-only cells while dropping border/font-only table artifacts.
    //   NOT using from_merges: wide header merges (A1:Z1) inflate far beyond real data.
    // max_row: text-only cells as baseline. Fill cells inflate max_row on Gantt sheets
    //   (colored date column backgrounds in all rows, including empty ones below tasks).
    //   from_merges kept so vertical section-label merges (e.g. A5:A10) are not cut.
    let cell_has_fill = |c: &XlCell| -> bool {
        if c.style_idx == 0 { return false; }
        let xf = styles.resolve_xf(c.style_idx);
        styles.get_fill(xf.fill_id).bg_color.is_some()
    };
    {
        let actual_max_col = cells.iter()
            .filter(|(_, c)| !c.text.is_empty() || cell_has_fill(c))
            .map(|(&(_, col), _)| col + 1)
            .max()
            .unwrap_or(0);
        let actual_max_row = {
            let from_text = cells.iter()
                .filter(|(_, c)| !c.text.is_empty())
                .map(|(&(row, _), _)| row + 1)
                .max()
                .unwrap_or(0);
            let from_styled = if from_text == 0 {
                // Fallback: sheet with no text (e.g. pure color matrix)
                cells.iter()
                    .filter(|(_, c)| cell_has_fill(c))
                    .map(|(&(row, _), _)| row + 1)
                    .max()
                    .unwrap_or(0)
            } else { 0 };
            let from_merges = merges.iter().map(|&(_, (r2, _))| r2 + 1).max().unwrap_or(0);
            from_text.max(from_styled).max(from_merges)
        };
        if actual_max_col > 0 { max_col = max_col.min(actual_max_col); }
        if actual_max_row > 0 { max_row = max_row.min(actual_max_row); }
    }

    // Build final column widths and row heights vectors
    let col_widths: Vec<f32> = (0..max_col)
        .map(|i| {
            col_width_map
                .get(&i)
                .copied()
                .unwrap_or_else(|| col_chars_to_pt(default_col_chars, styles.col_mdw_scale))
        })
        .collect();
    let row_heights: Vec<f32> = (0..max_row)
        .map(|i| row_height_map.get(&i).copied().unwrap_or(default_row_pt))
        .collect();

    ParsedSheet {
        max_row,
        max_col,
        col_widths,
        row_heights,
        cells,
        merges,
        default_col_chars,
    }
}

fn build_cell(
    cell_type: &str,
    raw_value: &str,
    style_idx: usize,
    shared_strings: &[(String, Vec<crate::model::Run>)],
    styles: &XlStyles,
) -> Option<XlCell> {
    if raw_value.is_empty() {
        // Still create the cell so formatting shows (border, bg)
        return Some(XlCell {
            text: String::new(),
            style_idx,
            is_numeric: false,
            rich_runs: vec![],
        });
    }

    let xf = styles.resolve_xf(style_idx);
    let num_fmts = &styles.num_fmts;

    let mut rich_runs_out: Vec<crate::model::Run> = vec![];
    let (text, is_numeric) = match cell_type {
        "s" => {
            let idx: usize = raw_value.parse().ok()?;
            if let Some((s, runs)) = shared_strings.get(idx) {
                if !runs.is_empty() {
                    rich_runs_out = runs.clone();
                }
                (s.clone(), false)
            } else {
                (String::new(), false)
            }
        }
        "b" => {
            let b = raw_value == "1" || raw_value.eq_ignore_ascii_case("true");
            (if b { "TRUE".to_string() } else { "FALSE".to_string() }, false)
        }
        "e" => {
            // Error value
            (raw_value.to_string(), false)
        }
        "str" | "inlineStr" => {
            (raw_value.to_string(), false)
        }
        _ => {
            // Numeric (default type, no t attribute, or t="n")
            if let Ok(val) = raw_value.parse::<f64>() {
                let custom = num_fmts.get(&xf.num_fmt_id).map(|s| s.as_str());
                format_number(val, xf.num_fmt_id, custom)
            } else {
                (raw_value.to_string(), false)
            }
        }
    };

    Some(XlCell {
        text,
        style_idx,
        is_numeric,
        rich_runs: rich_runs_out,
    })
}

// ── sheet → Table ────────────────────────────────────────────────────────────

fn default_grid_line() -> BorderLine {
    BorderLine {
        size_eighth_pt: 2,
        color: GRID_COLOR,
        style: BorderStyle::Single,
        explicit: true,
    }
}

fn sheet_to_table(sheet: &ParsedSheet, styles: &XlStyles) -> (Table, f32, f32) {
    // Build merge lookup
    // merge_topleft: (row,col) → (colspan, rowspan)
    let mut merge_topleft: HashMap<(usize, usize), (usize, usize)> = HashMap::new();
    // merge_covered: cells that are NOT the top-left of a merge range
    let mut merge_covered: HashSet<(usize, usize)> = HashSet::new();

    for &((r1, c1), (r2, c2)) in &sheet.merges {
        let colspan = (c2 - c1) + 1;
        let rowspan = (r2 - r1) + 1;
        merge_topleft.insert((r1, c1), (colspan, rowspan));
        for r in r1..=r2 {
            for c in c1..=c2 {
                if r != r1 || c != c1 {
                    merge_covered.insert((r, c));
                }
            }
        }
    }

    let default_col_w_dxa =
        (col_chars_to_pt(sheet.default_col_chars, styles.col_mdw_scale) * TWIPS_PER_PT) as u32;

    let grid_col_widths: Vec<u32> = sheet
        .col_widths
        .iter()
        .map(|&pt| (pt * TWIPS_PER_PT).round() as u32)
        .collect();

    let total_w_pt: f32 = sheet.col_widths.iter().sum();
    let total_h_pt: f32 = sheet.row_heights.iter().sum();

    let mut rows: Vec<TableRow> = Vec::new();

    for row_idx in 0..sheet.max_row {
        let row_h_pt = sheet.row_heights.get(row_idx).copied().unwrap_or(DEFAULT_ROW_PT);
        let row_h_dxa = (row_h_pt * TWIPS_PER_PT).round() as u32;

        let mut cells: Vec<TableCell> = Vec::new();
        let mut col_idx = 0;

        while col_idx < sheet.max_col {
            // Skip cells covered by a column-merge in this row
            // (covered by colspan of a cell to the left)
            if merge_covered.contains(&(row_idx, col_idx)) {
                // Check if this is covered by column-merge only (same row, different col)
                // or by row-merge (different row). For column-covered: skip.
                // For row-covered: add empty placeholder cell.
                let is_col_covered = sheet.merges.iter().any(|&((r1, c1), (r2, c2))| {
                    r1 == row_idx && c1 < col_idx && c1 <= col_idx && col_idx <= c2
                });
                if is_col_covered {
                    col_idx += 1;
                    continue;
                }
                // Row-covered cell: add empty cell
                let col_w_dxa = grid_col_widths
                    .get(col_idx)
                    .copied()
                    .unwrap_or(default_col_w_dxa);
                cells.push(TableCell {
                    blocks: vec![Block::Paragraph(Paragraph::default())],
                    width_dxa: col_w_dxa,
                    grid_span: 1,
                    borders: default_cell_borders(None, styles),
                    bg_color: None,
                    margins: Some(CellMargins { top: 0, left: 0, bottom: 0, right: 0 }),
                    no_wrap: true,
                    v_align: VAlign::Bottom,
                });
                col_idx += 1;
                continue;
            }

            let col_w_pt = sheet.col_widths.get(col_idx).copied()
                .unwrap_or_else(|| col_chars_to_pt(sheet.default_col_chars, styles.col_mdw_scale));
            let col_w_dxa = (col_w_pt * TWIPS_PER_PT).round() as u32;

            let (grid_span, _rowspan) = merge_topleft
                .get(&(row_idx, col_idx))
                .copied()
                .unwrap_or((1, 1));

            // Sum widths for colspan
            let mut cell_w_dxa: u32 = if grid_span > 1 {
                (col_idx..col_idx + grid_span)
                    .map(|c| grid_col_widths.get(c).copied().unwrap_or(default_col_w_dxa))
                    .sum()
            } else {
                col_w_dxa
            };

            let xl_cell = sheet.cells.get(&(row_idx, col_idx));
            let xf_idx = xl_cell.map(|c| c.style_idx).unwrap_or(0);
            let xf = styles.resolve_xf(xf_idx);
            let font = styles.get_font(xf.font_id);
            let fill = styles.get_fill(xf.fill_id);
            let border = styles.get_border(xf.border_id);

            let text = xl_cell.map(|c| c.text.as_str()).unwrap_or("");
            let is_numeric = xl_cell.map(|c| c.is_numeric).unwrap_or(false);

            // Determine alignment early (needed for overflow check)
            let align = if xf.h_align_general {
                if is_numeric { Align::Right } else { Align::Left }
            } else {
                xf.h_align
            };

            // Text overflow: only left-aligned non-wrapping cells spill into adjacent empty cells.
            // Center/right do NOT overflow in Excel.
            let mut effective_span = grid_span;
            if !text.is_empty() && !xf.wrap_text && align == Align::Left {
                let mut look = col_idx + grid_span;
                while look < sheet.max_col {
                    let look_key = (row_idx, look);
                    if merge_covered.contains(&look_key) { break; }
                    if merge_topleft.contains_key(&look_key) { break; }
                    // Only overflow into cells with no text and no background
                    let is_empty_cell = match sheet.cells.get(&look_key) {
                        None => true,
                        Some(nc) => {
                            if !nc.text.is_empty() { break; }
                            let nc_xf = styles.resolve_xf(nc.style_idx);
                            styles.get_fill(nc_xf.fill_id).bg_color.is_none()
                        }
                    };
                    if !is_empty_cell { break; }
                    cell_w_dxa += grid_col_widths.get(look).copied().unwrap_or(default_col_w_dxa);
                    effective_span += 1;
                    look += 1;
                }
            }

            let run_style = RunStyle {
                bold: font.bold,
                italic: font.italic,
                underline: font.underline,
                strike: false,
                size_pt: font.size_pt,
                color: auto_color(font.color, fill.bg_color),
                font_name: if font.name.is_empty() {
                    None
                } else {
                    Some(font.name.clone())
                },
                font_name_east_asia: None,
                size_cs_pt: None,
            };

            let bg = fill.bg_color;
            let para_runs = if let Some(c) = xl_cell {
                if !c.rich_runs.is_empty() {
                    c.rich_runs.iter().map(|r| {
                        let mut r2 = r.clone();
                        r2.style.color = auto_color(r.style.color, bg);
                        r2
                    }).collect()
                } else if !text.is_empty() {
                    vec![Run { text: text.to_string(), style: run_style, inline_image: None, is_page_number: false }]
                } else {
                    Vec::new()
                }
            } else if !text.is_empty() {
                vec![Run { text: text.to_string(), style: run_style, inline_image: None, is_page_number: false }]
            } else {
                Vec::new()
            };

            let para = Paragraph {
                runs: para_runs,
                align,
                space_before_pt: 0.0,
                space_after_pt: 0.0,
                // Excel default single-line spacing: ~1.31x font size (14.4pt for Calibri 11pt).
                // Must exceed LINE_FACTOR (1.15) to avoid descender/ascender overlap.
                line_pct: 1.3,
                ..Default::default()
            };

            let bg_color = fill.bg_color;
            let cell_borders = default_cell_borders(Some(border), styles);

            cells.push(TableCell {
                blocks: vec![Block::Paragraph(para)],
                width_dxa: cell_w_dxa,
                grid_span: effective_span as u32,
                borders: cell_borders,
                bg_color,
                margins: Some(CellMargins { top: 0, left: 0, bottom: 0, right: 0 }),
                no_wrap: !xf.wrap_text,
                v_align: xf.v_align.clone(),
            });

            col_idx += effective_span;
        }

        rows.push(TableRow {
            cells,
            height_dxa: row_h_dxa,
            // XLSX rows always auto-size to content. customHeight="1" in XML is just Excel's
            // stored hint for the last-computed height, not a hard clip like DOCX hRule="exact".
            height_exact: false,
        });
    }

    let table = Table {
        rows,
        width_dxa: (total_w_pt * TWIPS_PER_PT).round() as u32,
        width_is_pct: false,
        indent_dxa: 0,
        borders: crate::model::Borders::default(),
        cell_margins: CellMargins { top: 0, left: 0, bottom: 0, right: 0 },
        grid_col_widths,
    };

    (table, total_w_pt, total_h_pt)
}

fn default_cell_borders(
    border: Option<&XlBorder>,
    _styles: &XlStyles,
) -> crate::model::Borders {
    let grid = default_grid_line();

    if let Some(b) = border {
        let left = border_edge_to_line(&b.left).unwrap_or_else(|| grid.clone());
        let right = border_edge_to_line(&b.right).unwrap_or_else(|| grid.clone());
        let top = border_edge_to_line(&b.top).unwrap_or_else(|| grid.clone());
        let bottom = border_edge_to_line(&b.bottom).unwrap_or_else(|| grid.clone());
        crate::model::Borders {
            top,
            bottom,
            left,
            right,
            inside_h: grid.clone(),
            inside_v: grid,
        }
    } else {
        crate::model::Borders {
            top: grid.clone(),
            bottom: grid.clone(),
            left: grid.clone(),
            right: grid.clone(),
            inside_h: grid.clone(),
            inside_v: grid,
        }
    }
}

// ── main parser ──────────────────────────────────────────────────────────────

pub fn parse(bytes: &[u8]) -> Result<Document, String> {
    let sheet_list = parse_workbook(bytes);
    if sheet_list.is_empty() {
        return Err("XLSX: no sheets found in workbook".to_string());
    }

    let rels = parse_workbook_rels(bytes);
    let shared_strings = parse_shared_strings(bytes);
    let styles = parse_styles(bytes);

    let mut blocks: Vec<Block> = Vec::new();
    let mut page_dims: Vec<(f32, f32)> = Vec::new();
    let mut page_groups: Vec<PageGroup> = Vec::new();
    let mut page_idx = 0usize;

    for (sheet_name, rid) in &sheet_list {
        let path = match rels.get(rid.as_str()) {
            Some(p) => p.clone(),
            None => {
                // Fallback: try common naming patterns
                let sheet_num = page_idx + 1;
                format!("xl/worksheets/sheet{}.xml", sheet_num)
            }
        };

        let sheet = parse_sheet(bytes, &path, &shared_strings, &styles);

        if sheet.max_row == 0 || sheet.max_col == 0 {
            // Empty sheet: add a minimal placeholder page
            let placeholder = Table {
                rows: vec![TableRow {
                    cells: vec![TableCell {
                        blocks: vec![Block::Paragraph(Paragraph::default())],
                        width_dxa: 9360, // A4-ish width
                        grid_span: 1,
                        borders: crate::model::Borders::default(),
                        bg_color: None,
                        margins: None,
                        no_wrap: false,
                        v_align: VAlign::Top,
                    }],
                    height_dxa: 300,
                    height_exact: true,
                }],
                width_dxa: 9360,
                width_is_pct: false,
                indent_dxa: 0,
                borders: crate::model::Borders::default(),
                cell_margins: CellMargins::default(),
                grid_col_widths: vec![9360],
            };
            if page_idx > 0 {
                blocks.push(Block::PageBreak);
            }
            blocks.push(Block::Table(placeholder));
            page_dims.push((468.0, 200.0)); // fallback dims
            page_groups.push(PageGroup {
                name: sheet_name.clone(),
                start_page: page_idx,
                page_count: 1,
            });
            page_idx += 1;
            continue;
        }

        let (table, total_w_pt, total_h_pt) = sheet_to_table(&sheet, &styles);

        if page_idx > 0 {
            blocks.push(Block::PageBreak);
        }
        blocks.push(Block::Table(table));
        page_dims.push((total_w_pt.max(1.0), total_h_pt.max(1.0)));
        page_groups.push(PageGroup {
            name: sheet_name.clone(),
            start_page: page_idx,
            page_count: 1,
        });
        page_idx += 1;
    }

    // page_count and block_pages: set manually (no measure() needed for XLSX)
    // Each page maps to one table block (plus pagebreak before it)
    let page_count = page_idx;
    let mut block_pages: Vec<usize> = Vec::with_capacity(blocks.len());
    let mut current_page = 0usize;
    for block in &blocks {
        block_pages.push(current_page);
        if matches!(block, Block::PageBreak) {
            current_page += 1;
        }
    }

    // Use first sheet's dimensions as the document "default" (for fallback)
    let (first_w, first_h) = page_dims.first().copied().unwrap_or((595.276, 841.89));

    Ok(Document {
        blocks,
        page_w_pt: first_w,
        page_h_pt: first_h,
        margin_l_pt: 0.0,
        margin_r_pt: 0.0,
        margin_t_pt: 0.0,
        margin_b_pt: 0.0,
        page_count,
        block_pages,
        bytes: bytes.to_vec(),
        embedded_fonts: Vec::new(),
        header: None,
        footer: None,
        header_first: None,
        footer_first: None,
        header_margin_pt: 0.0,
        footer_margin_pt: 0.0,
        page_num_start: 1,
        page_dims,
        page_groups,
        doc_format: "xlsx".to_string(),
    })
}
