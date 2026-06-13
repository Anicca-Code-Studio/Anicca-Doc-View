//! DOCX (OOXML WordprocessingML) parser — full milestone:
//! fonts, tables, images, lists, indentation, embedded font extraction.

use std::collections::HashMap;
use std::io::Read;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::model::{
    Align, AnchorImage, Block, BorderLine, BorderStyle, Document, ImageFormat, InlineImage,
    Paragraph, Run, RunStyle, Table, TableCell, TableRow,
};

const TWIPS_PER_PT: f32 = 20.0;
const DEFAULT_PAGE_W_PT: f32 = 595.276; // A4 portrait
const DEFAULT_PAGE_H_PT: f32 = 841.89;
const DEFAULT_MARGIN_PT: f32 = 72.0;
const DEFAULT_SIZE_PT: f32 = 11.0;

pub fn is_zip(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && &bytes[0..2] == b"PK"
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn attr_val(e: &BytesStart, key: &[u8]) -> Option<String> {
    for a in e.attributes().flatten() {
        if a.key.as_ref() == key {
            return Some(String::from_utf8_lossy(&a.value).into_owned());
        }
    }
    None
}

fn toggle_on(e: &BytesStart, key: &[u8]) -> bool {
    match attr_val(e, key) {
        None => true,
        Some(v) => !matches!(v.as_str(), "0" | "false" | "off"),
    }
}

fn parse_color(v: &str) -> Option<[u8; 3]> {
    if v.eq_ignore_ascii_case("auto") || v.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&v[0..2], 16).ok()?;
    let g = u8::from_str_radix(&v[2..4], 16).ok()?;
    let b = u8::from_str_radix(&v[4..6], 16).ok()?;
    Some([r, g, b])
}

fn parse_align(v: &str) -> Align {
    match v {
        "center" => Align::Center,
        "right" | "end" => Align::Right,
        "both" | "distribute" | "justify" => Align::Justify,
        _ => Align::Left,
    }
}

fn parse_border_style(v: &str) -> BorderStyle {
    match v {
        "single" | "thick" | "thinThickSmallGap" | "thickThinSmallGap" => BorderStyle::Single,
        "double" | "triple" => BorderStyle::Double,
        "dashed" | "dashSmallGap" | "dashDot" | "dashDotDot" => BorderStyle::Dashed,
        "dotted" | "dotDash" | "dotDotDash" => BorderStyle::Dotted,
        _ => BorderStyle::None,
    }
}

fn parse_border_line(e: &BytesStart) -> BorderLine {
    let style = attr_val(e, b"w:val")
        .map(|v| parse_border_style(&v))
        .unwrap_or(BorderStyle::None);
    let size = attr_val(e, b"w:sz")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let color = attr_val(e, b"w:color")
        .and_then(|v| parse_color(&v))
        .unwrap_or([0, 0, 0]);
    BorderLine { size_eighth_pt: size, color, style, explicit: true }
}

fn read_zip_bytes(archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, name: &str) -> Option<Vec<u8>> {
    let mut file = archive.by_name(name).ok()?;
    let mut out = Vec::new();
    file.read_to_end(&mut out).ok()?;
    Some(out)
}

fn read_zip_text(archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, name: &str) -> Option<String> {
    read_zip_bytes(archive, name).and_then(|b| String::from_utf8(b).ok())
}

fn heading_level(name: &str) -> Option<u8> {
    let n = name.trim().to_lowercase();
    let rest = n.strip_prefix("heading ")?;
    rest.trim().parse::<u8>().ok().map(|v| v.saturating_sub(1))
}

/// Field char state machine: after the "separate" marker of a PAGE field, the
/// next w:t run is the cached page number and gets flagged for substitution.
fn handle_fld_char(e: &BytesStart, instr: &mut String, pending_page: &mut bool) {
    match attr_val(e, b"w:fldCharType").as_deref() {
        Some("begin") => {
            instr.clear();
            *pending_page = false;
        }
        Some("separate") => {
            *pending_page = instr
                .split_whitespace()
                .any(|t| t.eq_ignore_ascii_case("PAGE"));
        }
        Some("end") => {
            *pending_page = false;
            instr.clear();
        }
        _ => {}
    }
}

// ── relationships ─────────────────────────────────────────────────────────────

/// Parses a relationships file and returns rId → target path map.
fn parse_rels(xml: &str, base_dir: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                if e.name().as_ref() == b"Relationship" {
                    if let (Some(id), Some(target)) =
                        (attr_val(&e, b"Id"), attr_val(&e, b"Target"))
                    {
                        let path = if target.starts_with('/') {
                            target.trim_start_matches('/').to_string()
                        } else {
                            format!("{base_dir}{target}")
                        };
                        map.insert(id, path);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    map
}

// ── embedded font extraction ──────────────────────────────────────────────────

/// Deobfuscates an ODTTF font according to ECMA-376 OOXML spec.
/// The first 32 bytes are XOR'd with the 16-byte key derived from the GUID.
fn deobfuscate_odttf(font_bytes: &mut Vec<u8>, font_key: &str) {
    let hex: String = font_key.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 32 {
        return;
    }
    let mut key = [0u8; 16];
    for i in 0..16 {
        key[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap_or(0);
    }
    let len = font_bytes.len().min(32);
    for i in 0..len {
        font_bytes[i] ^= key[i % 16];
    }
}

struct EmbedRef {
    rid: String,
    font_key: String,
}

/// Parses fontTable.xml and returns (font_name, rid, font_key) triples for embedded fonts.
fn parse_font_table(xml: &str) -> Vec<(String, EmbedRef)> {
    let mut result: Vec<(String, EmbedRef)> = Vec::new();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut cur_name: Option<String> = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"w:font" => {
                    cur_name = attr_val(&e, b"w:name");
                }
                b"w:embedRegular" | b"w:embedBold" | b"w:embedItalic" | b"w:embedBoldItalic" => {
                    if let Some(name) = &cur_name {
                        if let (Some(rid), Some(key)) =
                            (attr_val(&e, b"r:id"), attr_val(&e, b"w:fontKey"))
                        {
                            result.push((name.clone(), EmbedRef { rid, font_key: key }));
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    result
}

pub fn extract_embedded_fonts(archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>) -> Vec<Vec<u8>> {
    let font_table_xml = match read_zip_text(archive, "word/fontTable.xml") {
        Some(x) => x,
        None => return Vec::new(),
    };
    let font_rels_xml = read_zip_text(archive, "word/_rels/fontTable.xml.rels").unwrap_or_default();
    let rels = parse_rels(&font_rels_xml, "word/");

    let embeds = parse_font_table(&font_table_xml);
    let mut fonts = Vec::new();
    for (_name, embed) in embeds {
        if let Some(path) = rels.get(&embed.rid) {
            if let Some(mut bytes) = read_zip_bytes(archive, path) {
                deobfuscate_odttf(&mut bytes, &embed.font_key);
                // Validate: a valid TTF/OTF starts with 0x00010000 or "OTTO" or "true" or "typ1"
                if bytes.len() >= 4 {
                    fonts.push(bytes);
                }
            }
        }
    }
    fonts
}

// ── styles ────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
struct StyleProps {
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
    size_pt: Option<f32>,
    color: Option<[u8; 3]>,
    font_name: Option<String>,
    align: Option<Align>,
    space_before_pt: Option<f32>,
    space_after_pt: Option<f32>,
    line_pct: Option<f32>,
    outline_level: Option<u8>,
    indent_left_pt: Option<f32>,
    indent_right_pt: Option<f32>,
    indent_first_line_pt: Option<f32>,
}

impl StyleProps {
    fn merge(&mut self, other: &StyleProps) {
        macro_rules! take {
            ($field:ident) => {
                if other.$field.is_some() {
                    self.$field = other.$field.clone();
                }
            };
        }
        take!(bold);
        take!(italic);
        take!(underline);
        take!(strike);
        take!(size_pt);
        take!(color);
        take!(font_name);
        take!(align);
        take!(space_before_pt);
        take!(space_after_pt);
        take!(line_pct);
        take!(outline_level);
        take!(indent_left_pt);
        take!(indent_right_pt);
        take!(indent_first_line_pt);
    }
}

#[derive(Clone, Debug, Default)]
struct RawStyle {
    based_on: Option<String>,
    name: Option<String>,
    props: StyleProps,
    /// Table style cell margins (w:tblCellMar), per side: top/left/bottom/right.
    cell_margins: [Option<u32>; 4],
}

struct Styles {
    doc_default: StyleProps,
    map: HashMap<String, RawStyle>,
}

impl Styles {
    fn resolve(&self, style_id: &str) -> StyleProps {
        let mut chain: Vec<&RawStyle> = Vec::new();
        let mut cur = Some(style_id.to_string());
        let mut guard = 0;
        while let Some(id) = cur {
            if let Some(s) = self.map.get(&id) {
                chain.push(s);
                cur = s.based_on.clone();
            } else {
                break;
            }
            guard += 1;
            if guard > 32 {
                break;
            }
        }
        chain.reverse();
        let mut out = self.doc_default.clone();
        for s in chain {
            out.merge(&s.props);
            if let Some(name) = &s.name {
                if let Some(lvl) = heading_level(name) {
                    out.outline_level = Some(lvl);
                }
            }
        }
        out
    }

    /// Resolve a table style's cell margins (w:tblCellMar) through basedOn.
    fn resolve_cell_margins(&self, style_id: &str) -> crate::model::CellMargins {
        let mut chain: Vec<&RawStyle> = Vec::new();
        let mut cur = Some(style_id.to_string());
        let mut guard = 0;
        while let Some(id) = cur {
            if let Some(s) = self.map.get(&id) {
                chain.push(s);
                cur = s.based_on.clone();
            } else {
                break;
            }
            guard += 1;
            if guard > 32 {
                break;
            }
        }
        chain.reverse();
        let mut out = crate::model::CellMargins::default();
        for s in chain {
            if let Some(v) = s.cell_margins[0] { out.top = v; }
            if let Some(v) = s.cell_margins[1] { out.left = v; }
            if let Some(v) = s.cell_margins[2] { out.bottom = v; }
            if let Some(v) = s.cell_margins[3] { out.right = v; }
        }
        out
    }
}

fn apply_prop(t: &mut StyleProps, e: &BytesStart) {
    match e.name().as_ref() {
        b"w:b" => t.bold = Some(toggle_on(e, b"w:val")),
        b"w:i" => t.italic = Some(toggle_on(e, b"w:val")),
        b"w:u" => {
            let val = attr_val(e, b"w:val").unwrap_or_default();
            t.underline = Some(!matches!(val.as_str(), "none" | "0" | "false"));
        }
        b"w:strike" | b"w:dstrike" => t.strike = Some(toggle_on(e, b"w:val")),
        b"w:sz" => {
            if let Some(v) = attr_val(e, b"w:val").and_then(|s| s.parse::<f32>().ok()) {
                t.size_pt = Some(v / 2.0);
            }
        }
        b"w:color" => {
            if let Some(v) = attr_val(e, b"w:val") {
                if let Some(c) = parse_color(&v) {
                    t.color = Some(c);
                }
            }
        }
        b"w:rFonts" => {
            // w:ascii is the most common; fall back to w:hAnsi
            let name = attr_val(e, b"w:ascii")
                .or_else(|| attr_val(e, b"w:hAnsi"))
                .or_else(|| attr_val(e, b"w:cs"));
            if name.is_some() {
                t.font_name = name;
            }
        }
        b"w:jc" => {
            if let Some(v) = attr_val(e, b"w:val") {
                t.align = Some(parse_align(&v));
            }
        }
        b"w:spacing" => {
            if let Some(v) = attr_val(e, b"w:after").and_then(|s| s.parse::<f32>().ok()) {
                t.space_after_pt = Some(v / TWIPS_PER_PT);
            }
            if let Some(v) = attr_val(e, b"w:before").and_then(|s| s.parse::<f32>().ok()) {
                t.space_before_pt = Some(v / TWIPS_PER_PT);
            }
            let rule = attr_val(e, b"w:lineRule").unwrap_or_default();
            if let Some(v) = attr_val(e, b"w:line").and_then(|s| s.parse::<f32>().ok()) {
                if rule == "auto" || rule.is_empty() {
                    t.line_pct = Some(v / 240.0);
                } else if rule == "exact" || rule == "atLeast" {
                    // line in twips → points; store as pct relative to default line height
                    let pt = v / TWIPS_PER_PT;
                    t.line_pct = Some(pt / 12.0); // approximate
                }
            }
        }
        b"w:ind" => {
            if let Some(v) = attr_val(e, b"w:left").and_then(|s| s.parse::<f32>().ok()) {
                t.indent_left_pt = Some(v / TWIPS_PER_PT);
            }
            if let Some(v) = attr_val(e, b"w:right").and_then(|s| s.parse::<f32>().ok()) {
                t.indent_right_pt = Some(v / TWIPS_PER_PT);
            }
            if let Some(v) = attr_val(e, b"w:firstLine").and_then(|s| s.parse::<f32>().ok()) {
                t.indent_first_line_pt = Some(v / TWIPS_PER_PT);
            } else if let Some(v) = attr_val(e, b"w:hanging").and_then(|s| s.parse::<f32>().ok()) {
                t.indent_first_line_pt = Some(-(v / TWIPS_PER_PT));
            }
        }
        b"w:outlineLvl" => {
            if let Some(v) = attr_val(e, b"w:val").and_then(|s| s.parse::<u8>().ok()) {
                t.outline_level = Some(v);
            }
        }
        _ => {}
    }
}

fn parse_styles_xml(xml: &str) -> Styles {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut doc_default = StyleProps::default();
    let mut map: HashMap<String, RawStyle> = HashMap::new();
    let mut cur_id: Option<String> = None;
    let mut cur: RawStyle = RawStyle::default();
    let mut in_doc_defaults = false;
    let mut in_cell_mar = false;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"w:docDefaults" => in_doc_defaults = true,
                b"w:style" => {
                    cur_id = attr_val(&e, b"w:styleId");
                    cur = RawStyle::default();
                }
                b"w:name" => {
                    if cur_id.is_some() {
                        cur.name = attr_val(&e, b"w:val");
                    }
                }
                b"w:basedOn" => {
                    if cur_id.is_some() {
                        cur.based_on = attr_val(&e, b"w:val");
                    }
                }
                b"w:tblCellMar" => in_cell_mar = true,
                b"w:top" | b"w:left" | b"w:bottom" | b"w:right" if in_cell_mar => {
                    if cur_id.is_some() {
                        if let Some(v) = attr_val(&e, b"w:w").and_then(|s| s.parse::<u32>().ok()) {
                            let idx = match e.name().as_ref() {
                                b"w:top" => 0,
                                b"w:left" => 1,
                                b"w:bottom" => 2,
                                _ => 3,
                            };
                            cur.cell_margins[idx] = Some(v);
                        }
                    }
                }
                _ => {
                    if cur_id.is_some() {
                        apply_prop(&mut cur.props, &e);
                    } else if in_doc_defaults {
                        apply_prop(&mut doc_default, &e);
                    }
                }
            },
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"w:docDefaults" => in_doc_defaults = false,
                b"w:tblCellMar" => in_cell_mar = false,
                b"w:style" => {
                    if let Some(id) = cur_id.take() {
                        map.insert(id, std::mem::take(&mut cur));
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    Styles { doc_default, map }
}

fn realize_run(p: &StyleProps) -> RunStyle {
    RunStyle {
        bold: p.bold.unwrap_or(false),
        italic: p.italic.unwrap_or(false),
        underline: p.underline.unwrap_or(false),
        strike: p.strike.unwrap_or(false),
        size_pt: p.size_pt.unwrap_or(DEFAULT_SIZE_PT),
        color: p.color.unwrap_or([0, 0, 0]),
        font_name: p.font_name.clone(),
    }
}

// ── numbering ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
enum NumFmt {
    Decimal,
    LowerLetter,
    UpperLetter,
    LowerRoman,
    UpperRoman,
    Bullet,
    None,
}

#[derive(Clone, Debug)]
struct LevelDef {
    start: u32,
    fmt: NumFmt,
    level_text: String,
    indent_left_dxa: u32,
    hanging_dxa: u32,
}

struct Numbering {
    /// abstractNumId → levels
    abstract_nums: HashMap<u32, Vec<LevelDef>>,
    /// numId → abstractNumId
    num_map: HashMap<u32, u32>,
    /// Counter: (numId, ilvl) → current count
    counters: HashMap<(u32, u8), u32>,
}

impl Numbering {
    fn empty() -> Self {
        Numbering {
            abstract_nums: HashMap::new(),
            num_map: HashMap::new(),
            counters: HashMap::new(),
        }
    }

    fn next_label(&mut self, num_id: u32, ilvl: u8) -> (String, f32, f32) {
        let abstract_id = match self.num_map.get(&num_id) {
            Some(&id) => id,
            None => return (String::new(), 0.0, 0.0),
        };
        let levels = match self.abstract_nums.get(&abstract_id) {
            Some(l) => l,
            None => return (String::new(), 0.0, 0.0),
        };
        let level = match levels.get(ilvl as usize) {
            Some(l) => l.clone(),
            None => return (String::new(), 0.0, 0.0),
        };

        let counter = self.counters.entry((num_id, ilvl)).or_insert(level.start.saturating_sub(1));
        *counter += 1;
        let count = *counter;

        // Reset child levels
        for child_ilvl in (ilvl + 1)..9 {
            self.counters.remove(&(num_id, child_ilvl));
        }

        let label = match level.fmt {
            NumFmt::Bullet | NumFmt::None => {
                // level_text is the actual bullet char
                let c = level.level_text.trim();
                if c.is_empty() { "•".to_string() } else { c.to_string() }
            }
            NumFmt::Decimal => {
                let mut t = level.level_text.clone();
                t = t.replace(&format!("%{}", ilvl + 1), &count.to_string());
                t
            }
            NumFmt::LowerLetter => {
                let ch = (b'a' + ((count as u8).wrapping_sub(1)) % 26) as char;
                let mut t = level.level_text.clone();
                t = t.replace(&format!("%{}", ilvl + 1), &ch.to_string());
                t
            }
            NumFmt::UpperLetter => {
                let ch = (b'A' + ((count as u8).wrapping_sub(1)) % 26) as char;
                let mut t = level.level_text.clone();
                t = t.replace(&format!("%{}", ilvl + 1), &ch.to_string());
                t
            }
            NumFmt::LowerRoman => {
                let s = to_roman(count, false);
                let mut t = level.level_text.clone();
                t = t.replace(&format!("%{}", ilvl + 1), &s);
                t
            }
            NumFmt::UpperRoman => {
                let s = to_roman(count, true);
                let mut t = level.level_text.clone();
                t = t.replace(&format!("%{}", ilvl + 1), &s);
                t
            }
        };

        let indent_left = level.indent_left_dxa as f32 / TWIPS_PER_PT;
        let hanging = level.hanging_dxa as f32 / TWIPS_PER_PT;
        (label + " ", indent_left, hanging)
    }
}

fn to_roman(mut n: u32, upper: bool) -> String {
    if n == 0 {
        return "0".to_string();
    }
    let vals = [1000, 900, 500, 400, 100, 90, 50, 40, 10, 9, 5, 4, 1];
    let syms = if upper {
        ["M", "CM", "D", "CD", "C", "XC", "L", "XL", "X", "IX", "V", "IV", "I"]
    } else {
        ["m", "cm", "d", "cd", "c", "xc", "l", "xl", "x", "ix", "v", "iv", "i"]
    };
    let mut out = String::new();
    for (&val, &sym) in vals.iter().zip(syms.iter()) {
        while n >= val {
            out.push_str(sym);
            n -= val;
        }
    }
    out
}

fn parse_numbering_xml(xml: &str) -> Numbering {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();

    let mut abstract_nums: HashMap<u32, Vec<LevelDef>> = HashMap::new();
    let mut num_map: HashMap<u32, u32> = HashMap::new();

    let mut cur_abstract_id: Option<u32> = None;
    let mut cur_levels: Vec<LevelDef> = Vec::new();
    let mut cur_level: Option<LevelDef> = None;
    let mut cur_ilvl: Option<u8> = None;

    let mut cur_num_id: Option<u32> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"w:abstractNum" => {
                    if let Some(v) = attr_val(&e, b"w:abstractNumId").and_then(|s| s.parse().ok()) {
                        cur_abstract_id = Some(v);
                        cur_levels = Vec::new();
                    }
                }
                b"w:lvl" => {
                    if let Some(v) = attr_val(&e, b"w:ilvl").and_then(|s| s.parse::<u8>().ok()) {
                        cur_ilvl = Some(v);
                        cur_level = Some(LevelDef {
                            start: 1,
                            fmt: NumFmt::Decimal,
                            level_text: String::new(),
                            indent_left_dxa: 720,
                            hanging_dxa: 360,
                        });
                    }
                }
                b"w:start" => {
                    if let Some(l) = cur_level.as_mut() {
                        if let Some(v) = attr_val(&e, b"w:val").and_then(|s| s.parse().ok()) {
                            l.start = v;
                        }
                    }
                }
                b"w:numFmt" => {
                    if let Some(l) = cur_level.as_mut() {
                        l.fmt = match attr_val(&e, b"w:val").as_deref().unwrap_or("") {
                            "decimal" => NumFmt::Decimal,
                            "lowerLetter" => NumFmt::LowerLetter,
                            "upperLetter" => NumFmt::UpperLetter,
                            "lowerRoman" => NumFmt::LowerRoman,
                            "upperRoman" => NumFmt::UpperRoman,
                            "bullet" => NumFmt::Bullet,
                            "none" => NumFmt::None,
                            _ => NumFmt::Decimal,
                        };
                    }
                }
                b"w:lvlText" => {
                    if let Some(l) = cur_level.as_mut() {
                        l.level_text = attr_val(&e, b"w:val").unwrap_or_default();
                    }
                }
                b"w:ind" => {
                    if let Some(l) = cur_level.as_mut() {
                        if let Some(v) = attr_val(&e, b"w:left").and_then(|s| s.parse::<u32>().ok()) {
                            l.indent_left_dxa = v;
                        }
                        if let Some(v) = attr_val(&e, b"w:hanging").and_then(|s| s.parse::<u32>().ok()) {
                            l.hanging_dxa = v;
                        }
                    }
                }
                b"w:num" => {
                    if let Some(v) = attr_val(&e, b"w:numId").and_then(|s| s.parse().ok()) {
                        cur_num_id = Some(v);
                    }
                }
                b"w:abstractNumId" => {
                    if let (Some(num_id), Some(v)) = (
                        cur_num_id,
                        attr_val(&e, b"w:val").and_then(|s| s.parse::<u32>().ok()),
                    ) {
                        num_map.insert(num_id, v);
                    }
                }
                _ => {}
            },
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"w:lvl" => {
                    if let (Some(ilvl), Some(l)) = (cur_ilvl.take(), cur_level.take()) {
                        while cur_levels.len() <= ilvl as usize {
                            cur_levels.push(LevelDef {
                                start: 1,
                                fmt: NumFmt::Decimal,
                                level_text: String::new(),
                                indent_left_dxa: 720,
                                hanging_dxa: 360,
                            });
                        }
                        cur_levels[ilvl as usize] = l;
                    }
                }
                b"w:abstractNum" => {
                    if let Some(id) = cur_abstract_id.take() {
                        abstract_nums.insert(id, std::mem::take(&mut cur_levels));
                    }
                }
                b"w:num" => {
                    cur_num_id = None;
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    Numbering {
        abstract_nums,
        num_map,
        counters: HashMap::new(),
    }
}

// ── document body parser ──────────────────────────────────────────────────────

struct DocParser<'a> {
    styles: &'a Styles,
    numbering: &'a mut Numbering,
    rels: HashMap<String, String>,
    archive: &'a mut zip::ZipArchive<std::io::Cursor<&'a [u8]>>,
    page_w: f32,
    page_h: f32,
    margin_l: f32,
    margin_r: f32,
    margin_t: f32,
    margin_b: f32,
    // Section header/footer tracking. Refs persist across sections (OOXML
    // inheritance: a section without its own ref inherits the previous one).
    sect_header_default: Option<String>,
    sect_footer_default: Option<String>,
    // Snapshot of refs at the end of the FIRST section (the cover page section).
    first_sect_header: Option<Option<String>>,
    first_sect_footer: Option<Option<String>>,
    sect_count: usize,
    header_margin: f32,
    footer_margin: f32,
    page_num_start: i32,
    page_num_start_seen: bool,
}

impl<'a> DocParser<'a> {
    fn parse_body(&mut self, xml: &str) -> Vec<Block> {
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(false);
        let mut buf = Vec::new();
        let mut blocks: Vec<Block> = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    match e.name().as_ref() {
                        b"w:tbl" => {
                            let tbl = self.parse_table(&mut reader, &mut buf);
                            blocks.push(Block::Table(tbl));
                        }
                        b"w:p" => {
                            let (para, pb) = self.parse_paragraph(&mut reader, &mut buf);
                            blocks.push(Block::Paragraph(para));
                            if pb {
                                blocks.push(Block::PageBreak);
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::Empty(ref e)) => {
                    match e.name().as_ref() {
                        b"w:pgSz" => {
                            if let Some(v) = attr_val(e, b"w:w").and_then(|s| s.parse::<f32>().ok()) {
                                self.page_w = v / TWIPS_PER_PT;
                            }
                            if let Some(v) = attr_val(e, b"w:h").and_then(|s| s.parse::<f32>().ok()) {
                                self.page_h = v / TWIPS_PER_PT;
                            }
                        }
                        b"w:pgMar" => {
                            if let Some(v) = attr_val(e, b"w:left").and_then(|s| s.parse::<f32>().ok()) {
                                self.margin_l = v / TWIPS_PER_PT;
                            }
                            if let Some(v) = attr_val(e, b"w:right").and_then(|s| s.parse::<f32>().ok()) {
                                self.margin_r = v / TWIPS_PER_PT;
                            }
                            if let Some(v) = attr_val(e, b"w:top").and_then(|s| s.parse::<f32>().ok()) {
                                self.margin_t = v / TWIPS_PER_PT;
                            }
                            if let Some(v) = attr_val(e, b"w:bottom").and_then(|s| s.parse::<f32>().ok()) {
                                self.margin_b = v / TWIPS_PER_PT;
                            }
                            self.capture_pgmar_hf(e);
                        }
                        b"w:headerReference" | b"w:footerReference" => {
                            self.capture_hf_ref(e);
                        }
                        b"w:pgNumType" => {
                            self.capture_pg_num_type(e);
                        }
                        _ => {}
                    }
                }
                Ok(Event::End(ref e)) => {
                    if e.name().as_ref() == b"w:sectPr" {
                        self.end_sect();
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        blocks
    }

    /// Parse a table from the shared reader. Called immediately after consuming `<w:tbl>`.
    /// Returns when `</w:tbl>` is consumed.
    fn parse_table(&mut self, reader: &mut Reader<&[u8]>, buf: &mut Vec<u8>) -> Table {
        let mut table = Table::default();
        let mut cur_row: Option<TableRow> = None;
        let mut cur_cell: Option<TableCell> = None;
        let mut cell_blocks: Vec<Block> = Vec::new();
        let mut in_tbl_borders = false;
        let mut in_tc_borders = false;
        let mut in_tbl_grid = false;
        let mut in_tbl_cell_mar = false;
        let mut in_tc_mar = false;

        loop {
            match reader.read_event_into(buf) {
                Ok(Event::Start(ref e)) => {
                    match e.name().as_ref() {
                        b"w:tblBorders" => { in_tbl_borders = true; }
                        b"w:tcBorders" => { in_tc_borders = true; }
                        b"w:tblGrid" => { in_tbl_grid = true; }
                        b"w:tblCellMar" => { in_tbl_cell_mar = true; }
                        b"w:tcMar" => { in_tc_mar = true; }
                        b"w:tr" => {
                            cur_row = Some(TableRow::default());
                        }
                        b"w:tc" => {
                            cur_cell = Some(TableCell::default());
                            cell_blocks.clear();
                        }
                        b"w:p" => {
                            // Parse cell paragraph inline with shared reader
                            let (para, _pb) = self.parse_paragraph(reader, buf);
                            cell_blocks.push(Block::Paragraph(para));
                        }
                        b"w:tbl" => {
                            // Nested table — recurse
                            let nested = self.parse_table(reader, buf);
                            cell_blocks.push(Block::Table(nested));
                        }
                        _ => {}
                    }
                }
                Ok(Event::Empty(ref e)) => {
                    match e.name().as_ref() {
                        // Grid column widths
                        b"w:gridCol" if in_tbl_grid => {
                            if let Some(v) = attr_val(e, b"w:w").and_then(|s| s.parse().ok()) {
                                table.grid_col_widths.push(v);
                            }
                        }
                        // Cell grid span
                        b"w:gridSpan" => {
                            if let Some(cell) = cur_cell.as_mut() {
                                if let Some(v) = attr_val(e, b"w:val").and_then(|s| s.parse::<u32>().ok()) {
                                    cell.grid_span = v.max(1);
                                }
                            }
                        }
                        // Table style: resolve cell margins from styles.xml
                        b"w:tblStyle" => {
                            if let Some(id) = attr_val(e, b"w:val") {
                                table.cell_margins = self.styles.resolve_cell_margins(&id);
                            }
                        }
                        // Table-level cell margins (w:tblCellMar) override the style
                        b"w:top" | b"w:left" | b"w:bottom" | b"w:right" if in_tbl_cell_mar => {
                            if let Some(v) = attr_val(e, b"w:w").and_then(|s| s.parse::<u32>().ok()) {
                                match e.name().as_ref() {
                                    b"w:top" => table.cell_margins.top = v,
                                    b"w:left" => table.cell_margins.left = v,
                                    b"w:bottom" => table.cell_margins.bottom = v,
                                    _ => table.cell_margins.right = v,
                                }
                            }
                        }
                        // Per-cell margins (w:tcMar) override the table
                        b"w:top" | b"w:left" | b"w:bottom" | b"w:right" if in_tc_mar => {
                            if let Some(cell) = cur_cell.as_mut() {
                                if let Some(v) = attr_val(e, b"w:w").and_then(|s| s.parse::<u32>().ok()) {
                                    let m = cell.margins.get_or_insert(table.cell_margins);
                                    match e.name().as_ref() {
                                        b"w:top" => m.top = v,
                                        b"w:left" => m.left = v,
                                        b"w:bottom" => m.bottom = v,
                                        _ => m.right = v,
                                    }
                                }
                            }
                        }
                        // Table-level properties
                        b"w:tblW" => {
                            if let Some(v) = attr_val(e, b"w:w").and_then(|s| s.parse().ok()) {
                                table.width_dxa = v;
                            }
                            table.width_is_pct = attr_val(e, b"w:type").as_deref() == Some("pct");
                        }
                        b"w:tblInd" => {
                            if let Some(v) = attr_val(e, b"w:w").and_then(|s| s.parse::<i32>().ok()) {
                                table.indent_dxa = v;
                            }
                        }
                        // Table borders (inside w:tblBorders)
                        b"w:top" if in_tbl_borders => { table.borders.top = parse_border_line(e); }
                        b"w:bottom" if in_tbl_borders => { table.borders.bottom = parse_border_line(e); }
                        b"w:left" if in_tbl_borders => { table.borders.left = parse_border_line(e); }
                        b"w:right" if in_tbl_borders => { table.borders.right = parse_border_line(e); }
                        b"w:insideH" if in_tbl_borders => { table.borders.inside_h = parse_border_line(e); }
                        b"w:insideV" if in_tbl_borders => { table.borders.inside_v = parse_border_line(e); }
                        // Cell borders (inside w:tcBorders)
                        b"w:top" if in_tc_borders => {
                            if let Some(c) = cur_cell.as_mut() { c.borders.top = parse_border_line(e); }
                        }
                        b"w:bottom" if in_tc_borders => {
                            if let Some(c) = cur_cell.as_mut() { c.borders.bottom = parse_border_line(e); }
                        }
                        b"w:left" if in_tc_borders => {
                            if let Some(c) = cur_cell.as_mut() { c.borders.left = parse_border_line(e); }
                        }
                        b"w:right" if in_tc_borders => {
                            if let Some(c) = cur_cell.as_mut() { c.borders.right = parse_border_line(e); }
                        }
                        // Row height
                        b"w:trHeight" => {
                            if let Some(row) = cur_row.as_mut() {
                                if let Some(v) = attr_val(e, b"w:val").and_then(|s| s.parse().ok()) {
                                    row.height_dxa = v;
                                }
                                row.height_exact = attr_val(e, b"w:hRule").as_deref() == Some("exact");
                            }
                        }
                        // Cell width
                        b"w:tcW" => {
                            if let Some(cell) = cur_cell.as_mut() {
                                if let Some(v) = attr_val(e, b"w:w").and_then(|s| s.parse().ok()) {
                                    cell.width_dxa = v;
                                }
                            }
                        }
                        // Cell background
                        b"w:shd" => {
                            if let Some(cell) = cur_cell.as_mut() {
                                if let Some(v) = attr_val(e, b"w:fill") {
                                    cell.bg_color = parse_color(&v);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::End(ref e)) => {
                    match e.name().as_ref() {
                        b"w:tbl" => return table,
                        b"w:tblBorders" => { in_tbl_borders = false; }
                        b"w:tcBorders" => { in_tc_borders = false; }
                        b"w:tblGrid" => { in_tbl_grid = false; }
                        b"w:tblCellMar" => { in_tbl_cell_mar = false; }
                        b"w:tcMar" => { in_tc_mar = false; }
                        b"w:tr" => {
                            if let Some(row) = cur_row.take() {
                                table.rows.push(row);
                            }
                        }
                        b"w:tc" => {
                            if let Some(mut cell) = cur_cell.take() {
                                cell.blocks = std::mem::take(&mut cell_blocks);
                                if let Some(row) = cur_row.as_mut() {
                                    row.cells.push(cell);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::Eof) | Err(_) => return table,
                _ => {}
            }
            buf.clear();
        }
    }

    fn parse_paragraph(&mut self, reader: &mut Reader<&[u8]>, buf: &mut Vec<u8>) -> (Paragraph, bool) {
        let mut para = Paragraph::default();
        let mut para_props = self.styles.doc_default.clone();
        let mut run_base = self.styles.doc_default.clone();
        let mut cur_style = realize_run(&run_base);
        let mut cur_text = String::new();
        let mut in_text = false;
        let mut num_id: Option<u32> = None;
        let mut page_break_after = false;
        let mut ilvl: u8 = 0;

        // Field (w:fldChar / w:instrText) state for PAGE numbers
        let mut in_instr = false;
        let mut instr = String::new();
        let mut pending_page = false;

        // Accumulate inline drawing info
        let mut in_drawing = false;
        let mut draw_cx: u64 = 0;
        let mut draw_cy: u64 = 0;
        let mut draw_rid: Option<String> = None;
        let mut draw_depth = 0i32;
        let mut is_anchor_drawing = false;
        let mut anchor_pos_x: i64 = 0;
        let mut anchor_pos_y: i64 = 0;
        let mut anchor_ref_h: u8 = 0;
        let mut anchor_ref_v: u8 = 0;
        let mut anchor_behind: bool = false;
        let mut in_pos_h = false;
        let mut in_pos_v = false;

        loop {
            match reader.read_event_into(buf) {
                Ok(Event::Start(ref e)) => {
                    match e.name().as_ref() {
                        b"w:t" => {
                            in_text = true;
                            cur_text.clear();
                        }
                        b"w:instrText" => {
                            in_instr = true;
                        }
                        b"w:fldChar" => {
                            handle_fld_char(e, &mut instr, &mut pending_page);
                        }
                        b"w:drawing" => {
                            in_drawing = true;
                            draw_depth = 1;
                            draw_cx = 0;
                            draw_cy = 0;
                            draw_rid = None;
                            is_anchor_drawing = false;
                            anchor_pos_x = 0;
                            anchor_pos_y = 0;
                            anchor_ref_h = 0;
                            anchor_ref_v = 0;
                            anchor_behind = false;
                        }
                        b"wp:anchor" if in_drawing => {
                            is_anchor_drawing = true;
                            anchor_behind = attr_val(e, b"behindDoc").as_deref() == Some("1");
                            draw_depth += 1;
                        }
                        b"wp:inline" if in_drawing => {
                            is_anchor_drawing = false;
                            draw_depth += 1;
                        }
                        b"wp:positionH" if in_drawing => {
                            in_pos_h = true;
                            anchor_ref_h = match attr_val(e, b"relativeFrom").as_deref().unwrap_or("column") {
                                "page" => 1,
                                "margin" => 2,
                                _ => 0,
                            };
                            draw_depth += 1;
                        }
                        b"wp:positionV" if in_drawing => {
                            in_pos_v = true;
                            anchor_ref_v = match attr_val(e, b"relativeFrom").as_deref().unwrap_or("paragraph") {
                                "page" => 1,
                                "margin" => 2,
                                _ => 0,
                            };
                            draw_depth += 1;
                        }
                        b"wp:extent" if in_drawing => {
                            if let Some(v) = attr_val(e, b"cx").and_then(|s| s.parse().ok()) {
                                draw_cx = v;
                            }
                            if let Some(v) = attr_val(e, b"cy").and_then(|s| s.parse().ok()) {
                                draw_cy = v;
                            }
                            draw_depth += 1;
                        }
                        b"a:blip" if in_drawing => {
                            draw_rid = attr_val(e, b"r:embed");
                            draw_depth += 1;
                        }
                        b"v:imagedata" if in_drawing => {
                            if draw_rid.is_none() {
                                draw_rid = attr_val(e, b"r:id");
                            }
                            draw_depth += 1;
                        }
                        b"w:sectPr" => {
                            page_break_after = true;
                        }
                        _ => {
                            if in_drawing {
                                draw_depth += 1;
                            } else {
                                apply_prop(&mut run_base, e);
                                apply_prop(&mut para_props, e);
                                apply_prop(&mut cur_style.clone().into_props(), e);
                                self.handle_para_el(
                                    e,
                                    &mut para_props,
                                    &mut run_base,
                                    &mut cur_style,
                                    &mut num_id,
                                    &mut ilvl,
                                );
                            }
                        }
                    }
                }
                Ok(Event::Empty(ref e)) => {
                    match e.name().as_ref() {
                        b"wp:extent" if in_drawing => {
                            if let Some(v) = attr_val(e, b"cx").and_then(|s| s.parse().ok()) {
                                draw_cx = v;
                            }
                            if let Some(v) = attr_val(e, b"cy").and_then(|s| s.parse().ok()) {
                                draw_cy = v;
                            }
                        }
                        b"a:blip" if in_drawing => {
                            draw_rid = attr_val(e, b"r:embed");
                        }
                        b"v:imagedata" if in_drawing => {
                            if draw_rid.is_none() {
                                draw_rid = attr_val(e, b"r:id");
                            }
                        }
                        b"wp:anchor" if in_drawing => {
                            is_anchor_drawing = true;
                            anchor_behind = attr_val(e, b"behindDoc").as_deref() == Some("1");
                        }
                        b"wp:inline" if in_drawing => {
                            is_anchor_drawing = false;
                        }
                        b"w:br" => {
                            if attr_val(e, b"w:type").as_deref() == Some("page") {
                                page_break_after = true;
                            }
                        }
                        b"w:fldChar" => {
                            handle_fld_char(e, &mut instr, &mut pending_page);
                        }
                        b"w:headerReference" | b"w:footerReference" => {
                            self.capture_hf_ref(e);
                        }
                        b"w:pgMar" => {
                            self.capture_pgmar_hf(e);
                        }
                        b"w:pgNumType" => {
                            self.capture_pg_num_type(e);
                        }
                        b"w:tab" => {
                            if let Some(pos_str) = attr_val(e, b"w:pos") {
                                // Tab stop definition inside <w:tabs>
                                let pos: u32 = pos_str.parse().unwrap_or(0);
                                let align = match attr_val(e, b"w:val").as_deref() {
                                    Some("right") => crate::model::TabAlign::Right,
                                    Some("center") => crate::model::TabAlign::Center,
                                    Some("decimal") => crate::model::TabAlign::Decimal,
                                    _ => crate::model::TabAlign::Left,
                                };
                                let leader = match attr_val(e, b"w:leader").as_deref() {
                                    Some("dot") => crate::model::TabLeader::Dot,
                                    Some("hyphen") => crate::model::TabLeader::Hyphen,
                                    Some("underscore") => crate::model::TabLeader::Underscore,
                                    _ => crate::model::TabLeader::None,
                                };
                                para.tab_stops.push(crate::model::TabStop { pos_dxa: pos, align, leader });
                            } else {
                                // Run tab character — flush pending text then push tab as its own run
                                if !cur_text.is_empty() {
                                    para.runs.push(crate::model::Run {
                                        text: std::mem::take(&mut cur_text),
                                        style: cur_style.clone(),
                                        inline_image: None,
                                        is_page_number: false,
                                    });
                                }
                                para.runs.push(crate::model::Run {
                                    text: "\t".to_string(),
                                    style: cur_style.clone(),
                                    inline_image: None,
                                    is_page_number: false,
                                });
                            }
                        }
                        _ => {
                            if !in_drawing {
                                self.handle_para_el(
                                    e,
                                    &mut para_props,
                                    &mut run_base,
                                    &mut cur_style,
                                    &mut num_id,
                                    &mut ilvl,
                                );
                            }
                        }
                    }
                }
                Ok(Event::Text(ref t)) => {
                    if in_text {
                        if let Ok(s) = t.unescape() {
                            cur_text.push_str(&s);
                        }
                    } else if in_instr {
                        if let Ok(s) = t.unescape() {
                            instr.push_str(&s);
                        }
                    } else if in_pos_h {
                        if let Ok(s) = t.unescape() {
                            anchor_pos_x = s.trim().parse().unwrap_or(0);
                        }
                    } else if in_pos_v {
                        if let Ok(s) = t.unescape() {
                            anchor_pos_y = s.trim().parse().unwrap_or(0);
                        }
                    }
                }
                Ok(Event::End(ref e)) => match e.name().as_ref() {
                    b"w:t" => {
                        in_text = false;
                        if pending_page {
                            // PAGE field cached value — mark for render-time substitution
                            para.runs.push(Run {
                                text: std::mem::take(&mut cur_text),
                                style: cur_style.clone(),
                                inline_image: None,
                                is_page_number: true,
                            });
                            pending_page = false;
                        } else if !cur_text.is_empty() {
                            para.runs.push(Run {
                                text: std::mem::take(&mut cur_text),
                                style: cur_style.clone(),
                                inline_image: None,
                                is_page_number: false,
                            });
                        }
                    }
                    b"w:r" => {
                        cur_style = realize_run(&run_base);
                    }
                    b"w:instrText" => {
                        in_instr = false;
                    }
                    b"w:sectPr" => {
                        self.end_sect();
                    }
                    b"wp:positionH" => {
                        in_pos_h = false;
                    }
                    b"wp:positionV" => {
                        in_pos_v = false;
                    }
                    b"w:drawing" => {
                        in_drawing = false;
                        in_pos_h = false;
                        in_pos_v = false;
                        if let Some(rid) = draw_rid.take() {
                            if is_anchor_drawing {
                                // Floating/anchored image — store with position, don't advance inline cursor
                                if let Some(img) = self.resolve_image(&rid, draw_cx, draw_cy) {
                                    para.anchor_images.push(AnchorImage {
                                        width_emu: draw_cx,
                                        height_emu: draw_cy,
                                        data: img.data,
                                        format: img.format,
                                        pos_x_emu: anchor_pos_x,
                                        pos_y_emu: anchor_pos_y,
                                        pos_ref_h: anchor_ref_h,
                                        pos_ref_v: anchor_ref_v,
                                        behind_doc: anchor_behind,
                                    });
                                }
                            } else {
                                // Inline image — add to runs (advances cursor)
                                if let Some(img) = self.resolve_image(&rid, draw_cx, draw_cy) {
                                    para.runs.push(Run {
                                        text: String::new(),
                                        style: cur_style.clone(),
                                        inline_image: Some(img),
                                        is_page_number: false,
                                    });
                                }
                            }
                        }
                        draw_depth = 0;
                        is_anchor_drawing = false;
                    }
                    b"w:p" => {
                        // Finalize paragraph. cur_style carries pPr>rPr props
                        // (e.g. the paragraph mark's w:sz) when no runs exist.
                        para.mark_style = cur_style.clone();
                        if let Some(v) = para_props.align {
                            para.align = v;
                        }
                        para.space_before_pt = para_props.space_before_pt.unwrap_or(0.0);
                        para.space_after_pt = para_props.space_after_pt.unwrap_or(0.0);
                        if let Some(v) = para_props.line_pct {
                            para.line_pct = v;
                        }
                        if para.outline_level.is_none() {
                            para.outline_level = para_props.outline_level;
                        }
                        para.indent_left_pt = para_props.indent_left_pt.unwrap_or(0.0);
                        para.indent_right_pt = para_props.indent_right_pt.unwrap_or(0.0);
                        para.indent_first_line_pt = para_props.indent_first_line_pt.unwrap_or(0.0);

                        // Resolve list numbering
                        if let Some(nid) = num_id {
                            let (label, ind_l, hanging) = self.numbering.next_label(nid, ilvl);
                            if !label.is_empty() {
                                para.list_prefix = Some(label);
                                para.list_hanging_pt = hanging;
                                // Override indent from numbering if not explicitly set
                                if para.indent_left_pt == 0.0 && ind_l > 0.0 {
                                    para.indent_left_pt = ind_l;
                                }
                                if para.indent_first_line_pt == 0.0 {
                                    para.indent_first_line_pt = -hanging;
                                }
                            }
                        }
                        return (para, page_break_after);
                    }
                    _ => {
                        if in_drawing {
                            draw_depth -= 1;
                            if draw_depth <= 0 {
                                in_drawing = false;
                            }
                        }
                    }
                },
                Ok(Event::Eof) | Err(_) => return (para, page_break_after),
                _ => {}
            }
            buf.clear();
        }
    }

    fn handle_para_el(
        &self,
        e: &BytesStart,
        para_props: &mut StyleProps,
        run_base: &mut StyleProps,
        cur_style: &mut RunStyle,
        num_id: &mut Option<u32>,
        ilvl: &mut u8,
    ) {
        match e.name().as_ref() {
            b"w:pStyle" => {
                if let Some(id) = attr_val(e, b"w:val") {
                    let resolved = self.styles.resolve(&id);
                    para_props.merge(&resolved);
                    run_base.merge(&resolved);
                    *cur_style = realize_run(run_base);
                    if resolved.outline_level.is_some() {
                        // stored via para_props
                    }
                }
            }
            b"w:rStyle" => {
                if let Some(id) = attr_val(e, b"w:val") {
                    let resolved = self.styles.resolve(&id);
                    let mut merged = run_base.clone();
                    merged.merge(&resolved);
                    *cur_style = realize_run(&merged);
                }
            }
            b"w:numId" => {
                if let Some(v) = attr_val(e, b"w:val").and_then(|s| s.parse::<u32>().ok()) {
                    if v == 0 {
                        *num_id = None;
                    } else {
                        *num_id = Some(v);
                    }
                }
            }
            b"w:ilvl" => {
                if let Some(v) = attr_val(e, b"w:val").and_then(|s| s.parse::<u8>().ok()) {
                    *ilvl = v;
                }
            }
            b"w:b" => cur_style.bold = toggle_on(e, b"w:val"),
            b"w:i" => cur_style.italic = toggle_on(e, b"w:val"),
            b"w:u" => {
                let val = attr_val(e, b"w:val").unwrap_or_default();
                cur_style.underline = !matches!(val.as_str(), "none" | "0" | "false");
            }
            b"w:strike" | b"w:dstrike" => cur_style.strike = toggle_on(e, b"w:val"),
            b"w:sz" => {
                if let Some(v) = attr_val(e, b"w:val").and_then(|s| s.parse::<f32>().ok()) {
                    cur_style.size_pt = v / 2.0;
                }
            }
            b"w:color" => {
                if let Some(v) = attr_val(e, b"w:val") {
                    if let Some(c) = parse_color(&v) {
                        cur_style.color = c;
                    }
                }
            }
            b"w:rFonts" => {
                let name = attr_val(e, b"w:ascii")
                    .or_else(|| attr_val(e, b"w:hAnsi"))
                    .or_else(|| attr_val(e, b"w:cs"));
                if name.is_some() {
                    cur_style.font_name = name.clone();
                    run_base.font_name = name;
                }
            }
            b"w:jc" => {
                if let Some(v) = attr_val(e, b"w:val") {
                    para_props.align = Some(parse_align(&v));
                }
            }
            b"w:spacing" => apply_prop(para_props, e),
            b"w:ind" => apply_prop(para_props, e),
            b"w:outlineLvl" => apply_prop(para_props, e),
            b"w:tab" => {} // handled at run level
            b"w:br" | b"w:cr" => {} // handled inline
            b"w:pgSz" => {} // handled at body level
            b"w:pgMar" => {} // handled at body level
            _ => {
                apply_prop(para_props, e);
            }
        }
    }

    /// Capture a header/footer reference from a sectPr (only the "default" type).
    fn capture_hf_ref(&mut self, e: &BytesStart) {
        if attr_val(e, b"w:type").as_deref() != Some("default") {
            return;
        }
        if let Some(rid) = attr_val(e, b"r:id") {
            if e.name().as_ref() == b"w:headerReference" {
                self.sect_header_default = Some(rid);
            } else {
                self.sect_footer_default = Some(rid);
            }
        }
    }

    /// Capture page numbering start from the first pgNumType seen.
    fn capture_pg_num_type(&mut self, e: &BytesStart) {
        if self.page_num_start_seen {
            return;
        }
        if let Some(v) = attr_val(e, b"w:start").and_then(|s| s.parse::<i32>().ok()) {
            self.page_num_start = v;
            self.page_num_start_seen = true;
        }
    }

    /// Capture header/footer distances from a pgMar element.
    fn capture_pgmar_hf(&mut self, e: &BytesStart) {
        if let Some(v) = attr_val(e, b"w:header").and_then(|s| s.parse::<f32>().ok()) {
            if v > 0.0 {
                self.header_margin = v / TWIPS_PER_PT;
            }
        }
        if let Some(v) = attr_val(e, b"w:footer").and_then(|s| s.parse::<f32>().ok()) {
            if v > 0.0 {
                self.footer_margin = v / TWIPS_PER_PT;
            }
        }
    }

    /// Called at </w:sectPr>; snapshots first-section refs for the cover page.
    fn end_sect(&mut self) {
        self.sect_count += 1;
        if self.sect_count == 1 {
            self.first_sect_header = Some(self.sect_header_default.clone());
            self.first_sect_footer = Some(self.sect_footer_default.clone());
        }
    }

    /// Parse a header/footer part referenced by rId, using that part's own rels
    /// file for image resolution.
    fn parse_hf_by_rid(&mut self, rid: Option<&String>) -> Option<crate::model::HeaderFooter> {
        let rid = rid?;
        let path = self.rels.get(rid)?.clone();
        let xml = read_zip_text(self.archive, &path)?;
        let fname = path.rsplit('/').next().unwrap_or(&path).to_string();
        let hf_rels = read_zip_text(self.archive, &format!("word/_rels/{fname}.rels"))
            .map(|x| parse_rels(&x, "word/"))
            .unwrap_or_default();
        let saved = std::mem::replace(&mut self.rels, hf_rels);
        let blocks = self.parse_body(&xml);
        self.rels = saved;
        Some(crate::model::HeaderFooter { blocks })
    }

    fn resolve_image(&mut self, rid: &str, cx_emu: u64, cy_emu: u64) -> Option<InlineImage> {
        let path = self.rels.get(rid)?.clone();
        let data = read_zip_bytes(self.archive, &path)?;
        let format = detect_image_format(&data);
        Some(InlineImage {
            width_emu: cx_emu,
            height_emu: cy_emu,
            data,
            format,
        })
    }
}

fn detect_image_format(data: &[u8]) -> ImageFormat {
    if data.starts_with(b"\x89PNG") {
        ImageFormat::Png
    } else if data.starts_with(b"\xff\xd8") {
        ImageFormat::Jpeg
    } else if data.starts_with(b"GIF") {
        ImageFormat::Gif
    } else if data.len() >= 2 && (data.starts_with(b"BM") || data.starts_with(b"BA")) {
        ImageFormat::Bmp
    } else {
        ImageFormat::Unknown
    }
}

// ── trait helper for RunStyle ─────────────────────────────────────────────────

trait IntoProps {
    fn into_props(self) -> StyleProps;
}

impl IntoProps for RunStyle {
    fn into_props(self) -> StyleProps {
        StyleProps {
            bold: Some(self.bold),
            italic: Some(self.italic),
            underline: Some(self.underline),
            strike: Some(self.strike),
            size_pt: Some(self.size_pt),
            color: Some(self.color),
            font_name: self.font_name,
            ..Default::default()
        }
    }
}

// ── public entry point ────────────────────────────────────────────────────────

pub fn parse(bytes: &[u8]) -> Result<Document, String> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| format!("zip open: {e}"))?;

    // Extract embedded fonts
    let embedded_fonts = extract_embedded_fonts(&mut archive);

    // Parse styles
    let styles = read_zip_text(&mut archive, "word/styles.xml")
        .map(|x| parse_styles_xml(&x))
        .unwrap_or(Styles {
            doc_default: StyleProps::default(),
            map: HashMap::new(),
        });

    // Parse numbering
    let mut numbering = read_zip_text(&mut archive, "word/numbering.xml")
        .map(|x| parse_numbering_xml(&x))
        .unwrap_or(Numbering::empty());

    // Parse relationships for images
    let rels = read_zip_text(&mut archive, "word/_rels/document.xml.rels")
        .map(|x| parse_rels(&x, "word/"))
        .unwrap_or_default();

    // Parse document
    let xml = read_zip_text(&mut archive, "word/document.xml")
        .ok_or_else(|| "word/document.xml not found".to_string())?;

    let mut parser = DocParser {
        styles: &styles,
        numbering: &mut numbering,
        rels,
        archive: &mut archive,
        page_w: DEFAULT_PAGE_W_PT,
        page_h: DEFAULT_PAGE_H_PT,
        margin_l: DEFAULT_MARGIN_PT,
        margin_r: DEFAULT_MARGIN_PT,
        margin_t: DEFAULT_MARGIN_PT,
        margin_b: DEFAULT_MARGIN_PT,
        sect_header_default: None,
        sect_footer_default: None,
        first_sect_header: None,
        first_sect_footer: None,
        sect_count: 0,
        header_margin: 36.0,
        footer_margin: 36.0,
        page_num_start: 1,
        page_num_start_seen: false,
    };

    let blocks = parser.parse_body(&xml);

    // Parse header/footer parts now that section refs are known.
    let hdr_rid = parser.sect_header_default.clone();
    let ftr_rid = parser.sect_footer_default.clone();
    let hdr_first_rid = parser.first_sect_header.clone().flatten();
    let ftr_first_rid = parser.first_sect_footer.clone().flatten();
    let header = parser.parse_hf_by_rid(hdr_rid.as_ref());
    let footer = parser.parse_hf_by_rid(ftr_rid.as_ref());
    let header_first = parser.parse_hf_by_rid(hdr_first_rid.as_ref());
    let footer_first = parser.parse_hf_by_rid(ftr_first_rid.as_ref());

    let page_w = parser.page_w;
    let page_h = parser.page_h;
    let margin_l = parser.margin_l;
    let margin_r = parser.margin_r;
    let margin_t = parser.margin_t;
    let margin_b = parser.margin_b;
    let header_margin_pt = parser.header_margin;
    let footer_margin_pt = parser.footer_margin;
    let page_num_start = parser.page_num_start;

    if blocks.is_empty() {
        let doc = Document {
            blocks: vec![Block::Paragraph(Paragraph::default())],
            page_w_pt: page_w,
            page_h_pt: page_h,
            margin_l_pt: margin_l,
            margin_r_pt: margin_r,
            margin_t_pt: margin_t,
            margin_b_pt: margin_b,
            page_count: 1,
            block_pages: Vec::new(),
            bytes: bytes.to_vec(),
            embedded_fonts,
            header,
            footer,
            header_first,
            footer_first,
            header_margin_pt,
            footer_margin_pt,
            page_num_start,
        };
        return Ok(doc);
    }

    Ok(Document {
        blocks,
        page_w_pt: page_w,
        page_h_pt: page_h,
        margin_l_pt: margin_l,
        margin_r_pt: margin_r,
        margin_t_pt: margin_t,
        margin_b_pt: margin_b,
        page_count: 1,
        block_pages: Vec::new(),
        bytes: bytes.to_vec(),
        embedded_fonts,
        header,
        footer,
        header_first,
        footer_first,
        header_margin_pt,
        footer_margin_pt,
        page_num_start,
    })
}
