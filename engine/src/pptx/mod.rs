//! PPTX (PowerPoint) support: container parsing, slide model, rendering.
//! Copyright (c) 2026 Anicca Code Studio. MIT licensed.
//!
//! 100% original engine. No third-party document library is used: the OOXML
//! container is read with `zip`, XML with `quick-xml`, and every shape is
//! rasterized with the in-house `crate::raster` vector engine and cosmic-text.

pub mod drawingml;
pub mod geometry;
pub mod inherit;
pub mod layout;
pub mod model;
pub mod render;
pub mod slide;
pub mod theme;
pub mod xmltree;

use std::collections::HashMap;
use std::io::Read;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use model::{EMU_PER_PT, Slide};
use theme::Theme;

pub type Zip<'a> = zip::ZipArchive<std::io::Cursor<&'a [u8]>>;

// ── shared helpers ─────────────────────────────────────────────────────────────

/// Attribute value by exact (possibly namespaced) key, e.g. `b"r:id"`.
pub(crate) fn attr(e: &BytesStart, key: &[u8]) -> Option<String> {
    for a in e.attributes().flatten() {
        if a.key.as_ref() == key {
            return Some(String::from_utf8_lossy(&a.value).into_owned());
        }
    }
    None
}

/// Attribute value by local name only (ignores namespace prefix).
pub(crate) fn attr_local(e: &BytesStart, local: &[u8]) -> Option<String> {
    for a in e.attributes().flatten() {
        let k = a.key;
        let name = k.local_name();
        if name.as_ref() == local {
            return Some(String::from_utf8_lossy(&a.value).into_owned());
        }
    }
    None
}

pub(crate) fn attr_i64(e: &BytesStart, key: &[u8]) -> Option<i64> {
    attr(e, key).and_then(|v| v.trim().parse::<i64>().ok())
}

pub(crate) fn local(name: &[u8]) -> &[u8] {
    match name.iter().position(|&b| b == b':') {
        Some(i) => &name[i + 1..],
        None => name,
    }
}

pub(crate) fn read_zip_bytes(zip: &mut Zip, name: &str) -> Option<Vec<u8>> {
    let mut f = zip.by_name(name).ok()?;
    let mut out = Vec::new();
    f.read_to_end(&mut out).ok()?;
    Some(out)
}

pub(crate) fn read_zip_text(zip: &mut Zip, name: &str) -> Option<String> {
    read_zip_bytes(zip, name).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// True if the entry exists in the archive.
pub(crate) fn zip_has(zip: &mut Zip, name: &str) -> bool {
    zip.by_name(name).is_ok()
}

/// Directory portion of a part path, e.g. "ppt/slides/slide1.xml" → "ppt/slides/".
pub(crate) fn dir_of(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[..=i].to_string(),
        None => String::new(),
    }
}

/// Resolve a relationship target (possibly "../media/x.png") against a base dir,
/// collapsing "." and ".." segments, producing a normalized archive path.
pub(crate) fn resolve_path(base_dir: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = Vec::new();
    for seg in base_dir.split('/').chain(target.split('/')) {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Parse a `_rels/*.rels` file into rId → resolved archive path.
pub(crate) fn parse_rels(zip: &mut Zip, part_path: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let base = dir_of(part_path);
    let rels_path = format!("{}_rels/{}.rels", base, file_name(part_path));
    let xml = match read_zip_text(zip, &rels_path) {
        Some(x) => x,
        None => return map,
    };
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                if local(e.name().as_ref()) == b"Relationship" {
                    if let (Some(id), Some(target)) = (attr(&e, b"Id"), attr(&e, b"Target")) {
                        // External targets (TargetMode="External") are URLs; keep raw.
                        let mode = attr(&e, b"TargetMode");
                        let path = if mode.as_deref() == Some("External") {
                            target
                        } else {
                            resolve_path(&base, &target)
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

fn file_name(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

// ── detection ──────────────────────────────────────────────────────────────────

/// A PPTX is a ZIP (PK\x03\x04) that contains `ppt/presentation.xml`.
pub fn is_pptx(bytes: &[u8]) -> bool {
    if bytes.len() < 4 || &bytes[0..2] != b"PK" {
        return false;
    }
    let cursor = std::io::Cursor::new(bytes);
    match zip::ZipArchive::new(cursor) {
        Ok(mut z) => z.by_name("ppt/presentation.xml").is_ok(),
        Err(_) => false,
    }
}

// ── document ───────────────────────────────────────────────────────────────────

pub struct PptxDocument {
    pub slides: Vec<Slide>,
    /// Slide dimensions in EMU (`p:sldSz`).
    pub slide_w_emu: i64,
    pub slide_h_emu: i64,
    pub theme: Theme,
    bytes: Vec<u8>,
}

impl PptxDocument {
    pub fn page_count(&self) -> usize {
        self.slides.len().max(1)
    }

    /// Slide size in points (same for every slide).
    pub fn slide_size_pt(&self) -> (f32, f32) {
        (
            (self.slide_w_emu as f64 / EMU_PER_PT) as f32,
            (self.slide_h_emu as f64 / EMU_PER_PT) as f32,
        )
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Title text of each slide (first title-placeholder text), for the outline.
    pub fn slide_titles(&self) -> Vec<String> {
        self.slides.iter().map(slide::slide_title).collect()
    }

    /// Font family names referenced across slides + theme (for JS prefetch).
    pub fn declared_fonts(&self) -> Vec<String> {
        let mut set: Vec<String> = Vec::new();
        let mut push = |name: &str| {
            let n = name.trim();
            if !n.is_empty() && !set.iter().any(|s| s.eq_ignore_ascii_case(n)) {
                set.push(n.to_string());
            }
        };
        for f in [&self.theme.major_latin, &self.theme.minor_latin] {
            if let Some(f) = f {
                push(f);
            }
        }
        for slide in &self.slides {
            slide::collect_fonts(slide, &mut |n| push(n));
        }
        set
    }
}

/// Parse a PPTX from raw bytes.
pub fn parse(bytes: &[u8]) -> Result<PptxDocument, String> {
    let cursor = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(cursor).map_err(|e| format!("pptx zip open: {e}"))?;

    let pres_xml = read_zip_text(&mut zip, "ppt/presentation.xml")
        .ok_or_else(|| "pptx: missing ppt/presentation.xml".to_string())?;
    let (slide_w_emu, slide_h_emu, slide_rids) = parse_presentation(&pres_xml);

    let pres_rels = parse_rels(&mut zip, "ppt/presentation.xml");

    // Resolve slide part paths in presentation order.
    let mut slide_paths: Vec<String> = Vec::new();
    for rid in &slide_rids {
        if let Some(p) = pres_rels.get(rid) {
            slide_paths.push(p.clone());
        }
    }
    // Fallback: if sldIdLst was empty/unresolved, take slides in name order.
    if slide_paths.is_empty() {
        let mut names: Vec<String> = (0..zip.len())
            .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
            .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
            .collect();
        names.sort_by(|a, b| slide_num(a).cmp(&slide_num(b)));
        slide_paths = names;
    }

    // Theme (first theme referenced by the master, else theme1).
    let theme = theme::load_first_theme(&mut zip);

    let mut slides = Vec::with_capacity(slide_paths.len());
    for path in &slide_paths {
        match slide::parse_slide(&mut zip, path, &theme) {
            Ok(s) => slides.push(s),
            Err(_) => slides.push(Slide::default()),
        }
    }

    let (mut w, mut h) = (slide_w_emu, slide_h_emu);
    if w <= 0 || h <= 0 {
        // Default 4:3 10"x7.5" if size missing.
        w = 9144000;
        h = 6858000;
    }

    Ok(PptxDocument {
        slides,
        slide_w_emu: w,
        slide_h_emu: h,
        theme,
        bytes: bytes.to_vec(),
    })
}

fn slide_num(name: &str) -> u32 {
    name.trim_end_matches(".xml")
        .rsplit(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Extract slide size and ordered slide relationship ids from presentation.xml.
fn parse_presentation(xml: &str) -> (i64, i64, Vec<String>) {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut w = 0i64;
    let mut h = 0i64;
    let mut rids = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match local(e.name().as_ref()) {
                b"sldSz" => {
                    w = attr_i64(&e, b"cx").unwrap_or(0);
                    h = attr_i64(&e, b"cy").unwrap_or(0);
                }
                b"sldId" => {
                    if let Some(rid) = attr(&e, b"r:id").or_else(|| attr_local(&e, b"id2")) {
                        rids.push(rid);
                    } else if let Some(rid) = rid_attr(&e) {
                        rids.push(rid);
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    (w, h, rids)
}

/// Find an r:id-style attribute (any prefix) on a sldId element.
fn rid_attr(e: &BytesStart) -> Option<String> {
    for a in e.attributes().flatten() {
        if a.key.local_name().as_ref() == b"id" {
            let v = String::from_utf8_lossy(&a.value);
            if v.starts_with("rId") {
                return Some(v.into_owned());
            }
        }
    }
    None
}
