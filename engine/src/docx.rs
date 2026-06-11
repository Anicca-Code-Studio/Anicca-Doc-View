//! Minimal DOCX (OOXML WordprocessingML) parser with styles.xml resolution.
//!
//! Reads `word/styles.xml` (docDefaults + named styles + basedOn chains) and
//! `word/document.xml`, producing a flat paragraph list with resolved run and
//! paragraph styling, heading levels, and spacing. Tables, images and numbering
//! are future milestones.

use std::collections::HashMap;
use std::io::Read;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::model::{Align, Document, Paragraph, Run, RunStyle};

const TWIPS_PER_PT: f32 = 20.0;
const DEFAULT_PAGE_W_PT: f32 = 595.276; // A4 portrait
const DEFAULT_PAGE_H_PT: f32 = 841.89;
const DEFAULT_MARGIN_PT: f32 = 72.0;
const DEFAULT_SIZE_PT: f32 = 11.0;

pub fn is_zip(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && &bytes[0..2] == b"PK"
}

/// Optional style properties (run + paragraph), merged along basedOn chains.
#[derive(Clone, Debug, Default)]
struct StyleProps {
    bold: Option<bool>,
    italic: Option<bool>,
    size_pt: Option<f32>,
    color: Option<[u8; 3]>,
    align: Option<Align>,
    space_before_pt: Option<f32>,
    space_after_pt: Option<f32>,
    line_pct: Option<f32>,
    outline_level: Option<u8>,
}

impl StyleProps {
    fn merge(&mut self, other: &StyleProps) {
        if other.bold.is_some() {
            self.bold = other.bold;
        }
        if other.italic.is_some() {
            self.italic = other.italic;
        }
        if other.size_pt.is_some() {
            self.size_pt = other.size_pt;
        }
        if other.color.is_some() {
            self.color = other.color;
        }
        if other.align.is_some() {
            self.align = other.align;
        }
        if other.space_before_pt.is_some() {
            self.space_before_pt = other.space_before_pt;
        }
        if other.space_after_pt.is_some() {
            self.space_after_pt = other.space_after_pt;
        }
        if other.line_pct.is_some() {
            self.line_pct = other.line_pct;
        }
        if other.outline_level.is_some() {
            self.outline_level = other.outline_level;
        }
    }
}

#[derive(Clone, Debug, Default)]
struct RawStyle {
    based_on: Option<String>,
    name: Option<String>,
    props: StyleProps,
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
}

fn heading_level(name: &str) -> Option<u8> {
    let n = name.trim().to_lowercase();
    let rest = n.strip_prefix("heading ")?;
    rest.trim().parse::<u8>().ok().map(|v| v.saturating_sub(1))
}

fn attr_val(e: &BytesStart, key: &[u8]) -> Option<String> {
    for a in e.attributes().flatten() {
        if a.key.as_ref() == key {
            return Some(String::from_utf8_lossy(&a.value).into_owned());
        }
    }
    None
}

fn toggle_on(e: &BytesStart) -> bool {
    match attr_val(e, b"w:val") {
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

/// Applies a single property element onto a StyleProps target.
fn apply_prop(t: &mut StyleProps, e: &BytesStart) {
    match e.name().as_ref() {
        b"w:b" => t.bold = Some(toggle_on(e)),
        b"w:i" => t.italic = Some(toggle_on(e)),
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
                }
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

fn read_zip_text(archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, name: &str) -> Option<String> {
    let mut file = archive.by_name(name).ok()?;
    let mut out = String::new();
    file.read_to_string(&mut out).ok()?;
    Some(out)
}

fn parse_styles_xml(xml: &str) -> Styles {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);

    let mut doc_default = StyleProps::default();
    let mut map: HashMap<String, RawStyle> = HashMap::new();
    let mut cur_id: Option<String> = None;
    let mut cur: RawStyle = RawStyle::default();
    let mut in_doc_defaults = false;

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
                b"w:style" => {
                    if let Some(id) = cur_id.take() {
                        map.insert(id, std::mem::take(&mut cur));
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    Styles { doc_default, map }
}

fn realize(p: &StyleProps) -> RunStyle {
    RunStyle {
        bold: p.bold.unwrap_or(false),
        italic: p.italic.unwrap_or(false),
        size_pt: p.size_pt.unwrap_or(DEFAULT_SIZE_PT),
        color: p.color.unwrap_or([0, 0, 0]),
    }
}

pub fn parse(bytes: &[u8]) -> Result<Document, String> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| format!("zip open: {e}"))?;

    let styles = read_zip_text(&mut archive, "word/styles.xml")
        .map(|x| parse_styles_xml(&x))
        .unwrap_or(Styles {
            doc_default: StyleProps::default(),
            map: HashMap::new(),
        });

    let xml = read_zip_text(&mut archive, "word/document.xml")
        .ok_or_else(|| "word/document.xml not found".to_string())?;

    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(false);

    let mut paragraphs: Vec<Paragraph> = Vec::new();
    let mut cur_para = Paragraph::default();
    // Run base props inherited from the paragraph's style (and doc defaults).
    let mut run_base = styles.doc_default.clone();
    let mut cur_style = realize(&run_base);
    let mut cur_text = String::new();
    let mut in_text = false;
    // Direct paragraph property overrides accumulate in this StyleProps and are
    // applied on top of the paragraph style at </w:pPr>/run time.
    let mut para_props = styles.doc_default.clone();

    let mut page_w = DEFAULT_PAGE_W_PT;
    let mut page_h = DEFAULT_PAGE_H_PT;
    let mut margin = DEFAULT_MARGIN_PT;

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if e.name().as_ref() == b"w:t" {
                    in_text = true;
                    cur_text.clear();
                } else {
                    handle_doc_el(
                        &e,
                        &styles,
                        &mut cur_para,
                        &mut para_props,
                        &mut run_base,
                        &mut cur_style,
                        &mut page_w,
                        &mut page_h,
                        &mut margin,
                    );
                }
            }
            Ok(Event::Empty(e)) => {
                handle_doc_el(
                    &e,
                    &styles,
                    &mut cur_para,
                    &mut para_props,
                    &mut run_base,
                    &mut cur_style,
                    &mut page_w,
                    &mut page_h,
                    &mut margin,
                );
            }
            Ok(Event::Text(t)) => {
                if in_text {
                    if let Ok(s) = t.unescape() {
                        cur_text.push_str(&s);
                    }
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"w:t" => {
                    in_text = false;
                    if !cur_text.is_empty() {
                        cur_para.runs.push(Run {
                            text: std::mem::take(&mut cur_text),
                            style: cur_style.clone(),
                        });
                    }
                }
                b"w:r" => {
                    // Reset run style for the next run back to paragraph base.
                    cur_style = realize(&run_base);
                }
                b"w:p" => {
                    // Apply accumulated paragraph props.
                    if let Some(a) = para_props.align {
                        cur_para.align = a;
                    }
                    if let Some(v) = para_props.space_before_pt {
                        cur_para.space_before_pt = v;
                    }
                    if let Some(v) = para_props.space_after_pt {
                        cur_para.space_after_pt = v;
                    }
                    if let Some(v) = para_props.line_pct {
                        cur_para.line_pct = v;
                    }
                    if cur_para.outline_level.is_none() {
                        cur_para.outline_level = para_props.outline_level;
                    }
                    paragraphs.push(std::mem::take(&mut cur_para));
                    // Reset paragraph-scoped state.
                    para_props = styles.doc_default.clone();
                    run_base = styles.doc_default.clone();
                    cur_style = realize(&run_base);
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("xml parse: {e}")),
            _ => {}
        }
        buf.clear();
    }

    if paragraphs.is_empty() {
        paragraphs.push(Paragraph::default());
    }

    Ok(Document {
        paragraphs,
        page_w_pt: page_w,
        page_h_pt: page_h,
        margin_pt: margin,
        page_count: 1,
        paragraph_pages: Vec::new(),
        bytes: bytes.to_vec(),
    })
}

#[allow(clippy::too_many_arguments)]
fn handle_doc_el(
    e: &BytesStart,
    styles: &Styles,
    cur_para: &mut Paragraph,
    para_props: &mut StyleProps,
    run_base: &mut StyleProps,
    cur_style: &mut RunStyle,
    page_w: &mut f32,
    page_h: &mut f32,
    margin: &mut f32,
) {
    match e.name().as_ref() {
        b"w:p" => {
            *cur_para = Paragraph::default();
            *para_props = styles.doc_default.clone();
            *run_base = styles.doc_default.clone();
            *cur_style = realize(run_base);
        }
        b"w:pStyle" => {
            if let Some(id) = attr_val(e, b"w:val") {
                let resolved = styles.resolve(&id);
                // Paragraph-level: seed para_props and run_base from the style.
                para_props.merge(&resolved);
                run_base.merge(&resolved);
                *cur_style = realize(run_base);
                if resolved.outline_level.is_some() {
                    cur_para.outline_level = resolved.outline_level;
                }
            }
        }
        b"w:rStyle" => {
            if let Some(id) = attr_val(e, b"w:val") {
                let resolved = styles.resolve(&id);
                let mut merged = run_base.clone();
                merged.merge(&resolved);
                *cur_style = realize(&merged);
            }
        }
        b"w:r" => {
            *cur_style = realize(run_base);
        }
        // Direct run properties mutate the live run style.
        b"w:b" => cur_style.bold = toggle_on(e),
        b"w:i" => cur_style.italic = toggle_on(e),
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
        // Direct paragraph properties accumulate.
        b"w:jc" => {
            if let Some(v) = attr_val(e, b"w:val") {
                para_props.align = Some(parse_align(&v));
            }
        }
        b"w:spacing" => apply_prop(para_props, e),
        b"w:outlineLvl" => apply_prop(para_props, e),
        b"w:tab" => cur_para.runs.push(Run {
            text: "\t".to_string(),
            style: cur_style.clone(),
        }),
        b"w:br" | b"w:cr" => cur_para.runs.push(Run {
            text: "\n".to_string(),
            style: cur_style.clone(),
        }),
        b"w:pgSz" => {
            if let Some(v) = attr_val(e, b"w:w").and_then(|s| s.parse::<f32>().ok()) {
                *page_w = v / TWIPS_PER_PT;
            }
            if let Some(v) = attr_val(e, b"w:h").and_then(|s| s.parse::<f32>().ok()) {
                *page_h = v / TWIPS_PER_PT;
            }
        }
        b"w:pgMar" => {
            let l = attr_val(e, b"w:left").and_then(|s| s.parse::<f32>().ok());
            let t = attr_val(e, b"w:top").and_then(|s| s.parse::<f32>().ok());
            if let Some(v) = l.or(t) {
                *margin = v / TWIPS_PER_PT;
            }
        }
        _ => {}
    }
}
