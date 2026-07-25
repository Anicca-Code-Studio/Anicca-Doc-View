//! Anicca Doc View rendering engine (WASM).
//! Copyright (c) 2026 Anicca Code Studio. MIT licensed.
//!
//! Exposes a `Wasm` class plus `parseFontInfo`, matching the interface the
//! viewer's worker expects.

pub mod docx;
pub mod fonts_bundled;
pub mod model;
pub mod pdf;
pub mod pptx;
pub mod raster;
pub mod render;
pub mod xlsx;

use std::collections::HashMap;

use cosmic_text::{FontSystem, SwashCache};
use serde::Serialize;
use wasm_bindgen::prelude::*;

use model::{Block, Document};

fn to_js<T: Serialize>(v: &T) -> Result<JsValue, JsValue> {
    let s = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
    v.serialize(&s).map_err(|e| JsValue::from_str(&e.to_string()))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LicenseResultJs {
    valid: bool,
    tier: String,
    features: Vec<String>,
    limits: HashMap<String, f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl LicenseResultJs {
    fn licensed() -> Self {
        LicenseResultJs {
            valid: true,
            tier: "licensed".to_string(),
            features: vec!["no_attribution".to_string(), "no_telemetry".to_string()],
            limits: HashMap::new(),
            error: None,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PageInfoJs {
    width: f32,
    height: f32,
    rotation: i32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PageGroupLayoutJs {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PageGroupJs {
    name: String,
    start_page_index: usize,
    page_count: usize,
    layout: PageGroupLayoutJs,
}


#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FontInfoJs {
    typeface: String,
    bold: bool,
    italic: bool,
}

#[derive(Serialize)]
struct DisplayJs {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DestinationJs {
    page_index: usize,
    display: DisplayJs,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OutlineItemJs {
    title: String,
    destination: Option<DestinationJs>,
    children: Vec<OutlineItemJs>,
    initially_collapsed: bool,
}

fn build_outline(entries: &[(u8, String, usize)], i: &mut usize, level: u8) -> Vec<OutlineItemJs> {
    let mut nodes = Vec::new();
    while *i < entries.len() {
        let (lvl, title, page) = &entries[*i];
        if *lvl < level {
            break;
        }
        let title = title.clone();
        let page = *page;
        *i += 1;
        let children = build_outline(entries, i, level + 1);
        nodes.push(OutlineItemJs {
            title,
            destination: Some(DestinationJs {
                page_index: page,
                display: DisplayJs { kind: "fit" },
            }),
            children,
            initially_collapsed: false,
        });
    }
    nodes
}

/// A loaded document: the OOXML block model, a PDF, or a PPTX slide deck.
enum Loaded {
    Ooxml(Document),
    Pdf(pdf::PdfDocument),
    Pptx(pptx::PptxDocument),
}

#[wasm_bindgen]
pub struct Wasm {
    fonts: FontSystem,
    swash: SwashCache,
    docs: HashMap<String, Loaded>,
    next_id: u64,
}

#[wasm_bindgen]
impl Wasm {
    #[wasm_bindgen(constructor)]
    pub fn new(_domain: String, _viewer_version: String) -> Wasm {
        Wasm {
            fonts: render::new_font_system(),
            swash: SwashCache::new(),
            docs: HashMap::new(),
            next_id: 1,
        }
    }

    pub fn init_gpu(&mut self) -> bool {
        false
    }

    pub fn setup_telemetry(&mut self, _distinct_id: String) {}

    pub fn disable_telemetry(&mut self) -> bool {
        true
    }

    pub fn set_license(&mut self, _license: String) -> Result<JsValue, JsValue> {
        to_js(&LicenseResultJs::licensed())
    }

    pub fn license_status(&self) -> Result<JsValue, JsValue> {
        to_js(&LicenseResultJs::licensed())
    }

    pub fn load(&mut self, bytes: Vec<u8>) -> Result<String, JsValue> {
        if pdf::is_pdf(&bytes) {
            let doc = pdf::parse(&bytes).map_err(|e| JsValue::from_str(&e))?;
            return Ok(self.insert(Loaded::Pdf(doc)));
        }

        if xlsx::is_xlsx(&bytes) {
            let mut doc = xlsx::parse(&bytes).map_err(|e| JsValue::from_str(&e))?;
            // Two-pass: measure actual auto-height row expansion for correct canvas sizing
            render::measure_xlsx_page_heights(&mut self.fonts, &mut doc);
            return Ok(self.insert(Loaded::Ooxml(doc)));
        }

        if pptx::is_pptx(&bytes) {
            let doc = pptx::parse(&bytes).map_err(|e| JsValue::from_str(&e))?;
            return Ok(self.insert(Loaded::Pptx(doc)));
        }

        if !docx::is_zip(&bytes) {
            return Err(JsValue::from_str(
                "anicca-engine: format not supported (PDF, DOCX, XLSX and PPTX only)",
            ));
        }
        let mut doc = docx::parse(&bytes).map_err(|e| JsValue::from_str(&e))?;

        // Load embedded fonts from the DOCX into our FontSystem
        render::load_embedded_fonts(&mut self.fonts, &doc.embedded_fonts);

        // Compute layout metrics
        let (page_count, block_pages) = render::measure(&mut self.fonts, &doc);
        doc.page_count = page_count;
        doc.block_pages = block_pages;

        Ok(self.insert(Loaded::Ooxml(doc)))
    }

    pub fn document_format(&self, document_id: String) -> String {
        match self.docs.get(&document_id) {
            Some(Loaded::Pdf(_)) => "pdf".to_string(),
            Some(Loaded::Pptx(_)) => "pptx".to_string(),
            Some(Loaded::Ooxml(d)) => d.doc_format.clone(),
            None => "docx".to_string(),
        }
    }

    pub fn has_document(&self, document_id: String) -> bool {
        self.docs.contains_key(&document_id)
    }

    pub fn remove_document(&mut self, document_id: String) -> bool {
        self.docs.remove(&document_id).is_some()
    }

    pub fn needs_password(&self, _document_id: String) -> bool {
        false
    }

    pub fn authenticate(&mut self, _document_id: String, _password: String) -> bool {
        true
    }

    pub fn page_count(&self, document_id: String) -> usize {
        match self.docs.get(&document_id) {
            Some(Loaded::Pdf(p)) => p.page_count(),
            Some(Loaded::Pptx(p)) => p.page_count(),
            Some(Loaded::Ooxml(d)) => d.page_count,
            None => 0,
        }
    }

    pub fn page_info(&self, document_id: String, page_index: usize) -> Result<JsValue, JsValue> {
        if let Some(Loaded::Pdf(p)) = self.docs.get(&document_id) {
            return to_js(&pdf_page_info(p, page_index));
        }
        if let Some(Loaded::Pptx(p)) = self.docs.get(&document_id) {
            let (width, height) = p.slide_size_pt();
            return to_js(&PageInfoJs { width, height, rotation: 0 });
        }
        let doc = self.ooxml(&document_id)?;
        let (width, height) = doc
            .page_dims
            .get(page_index)
            .copied()
            .unwrap_or((doc.page_w_pt, doc.page_h_pt));
        to_js(&PageInfoJs {
            width,
            height,
            rotation: 0,
        })
    }

    pub fn all_page_info(&self, document_id: String) -> Result<JsValue, JsValue> {
        if let Some(Loaded::Pdf(p)) = self.docs.get(&document_id) {
            let pages: Vec<PageInfoJs> = (0..p.page_count()).map(|i| pdf_page_info(p, i)).collect();
            return to_js(&pages);
        }
        if let Some(Loaded::Pptx(p)) = self.docs.get(&document_id) {
            let (width, height) = p.slide_size_pt();
            let pages: Vec<PageInfoJs> = (0..p.page_count())
                .map(|_| PageInfoJs { width, height, rotation: 0 })
                .collect();
            return to_js(&pages);
        }
        let doc = self.ooxml(&document_id)?;
        let pages: Vec<PageInfoJs> = (0..doc.page_count)
            .map(|i| {
                let (width, height) = doc
                    .page_dims
                    .get(i)
                    .copied()
                    .unwrap_or((doc.page_w_pt, doc.page_h_pt));
                PageInfoJs {
                    width,
                    height,
                    rotation: 0,
                }
            })
            .collect();
        to_js(&pages)
    }

    pub fn page_groups(&self, document_id: String) -> Result<JsValue, JsValue> {
        if let Some(Loaded::Pdf(p)) = self.docs.get(&document_id) {
            return to_js(&vec![PageGroupJs {
                name: String::new(),
                start_page_index: 0,
                page_count: p.page_count(),
                layout: PageGroupLayoutJs { kind: "linear" },
            }]);
        }
        if let Some(Loaded::Pptx(p)) = self.docs.get(&document_id) {
            return to_js(&vec![PageGroupJs {
                name: String::new(),
                start_page_index: 0,
                page_count: p.page_count(),
                layout: PageGroupLayoutJs { kind: "linear" },
            }]);
        }
        let doc = self.ooxml(&document_id)?;
        if !doc.page_groups.is_empty() {
            let groups: Vec<PageGroupJs> = doc
                .page_groups
                .iter()
                .map(|g| PageGroupJs {
                    name: g.name.clone(),
                    start_page_index: g.start_page,
                    page_count: g.page_count,
                    layout: PageGroupLayoutJs { kind: "linear" },
                })
                .collect();
            return to_js(&groups);
        }
        to_js(&vec![PageGroupJs {
            name: String::new(),
            start_page_index: 0,
            page_count: doc.page_count,
            layout: PageGroupLayoutJs { kind: "linear" },
        }])
    }

    pub fn render_page_to_rgba(
        &mut self,
        document_id: String,
        page_index: usize,
        width: usize,
        height: usize,
    ) -> Result<Vec<u8>, JsValue> {
        let doc = self
            .docs
            .remove(&document_id)
            .ok_or_else(|| JsValue::from_str("document not found"))?;
        let rgba = match &doc {
            Loaded::Pdf(p) => match p.pages.get(page_index) {
                // Rotation is reported to the viewer separately, so the canvas
                // itself is rendered unrotated.
                Some(page) => pdf::content::render_page_rgba(&p.file, page, width, height, false),
                None => vec![255u8; width * height * 4],
            },
            Loaded::Ooxml(d) => {
                render::render_page(&mut self.fonts, &mut self.swash, d, page_index, width, height)
            }
            Loaded::Pptx(p) => pptx::render::render_slide_rgba(
                &mut self.fonts,
                &mut self.swash,
                p,
                page_index,
                width,
                height,
            ),
        };
        self.docs.insert(document_id, doc);
        Ok(rgba)
    }

    pub fn render_page_gpu(
        &mut self,
        document_id: String,
        page_index: usize,
        width: usize,
        height: usize,
    ) -> Result<Vec<u8>, JsValue> {
        self.render_page_to_rgba(document_id, page_index, width, height)
    }

    pub fn get_outline(&self, document_id: String) -> Result<JsValue, JsValue> {
        if let Some(Loaded::Pdf(p)) = self.docs.get(&document_id) {
            return to_js(&pdf_outline(p));
        }
        if let Some(Loaded::Pptx(p)) = self.docs.get(&document_id) {
            let items: Vec<OutlineItemJs> = p
                .slide_titles()
                .into_iter()
                .enumerate()
                .map(|(i, title)| OutlineItemJs {
                    title: if title.is_empty() { format!("Slide {}", i + 1) } else { title },
                    destination: Some(DestinationJs {
                        page_index: i,
                        display: DisplayJs { kind: "fit" },
                    }),
                    children: Vec::new(),
                    initially_collapsed: false,
                })
                .collect();
            return to_js(&items);
        }
        let doc = self.ooxml(&document_id)?;
        let mut entries: Vec<(u8, String, usize)> = Vec::new();

        for (i, block) in doc.blocks.iter().enumerate() {
            if let Block::Paragraph(p) = block {
                if let Some(lvl) = p.outline_level {
                    let title = p.text();
                    if title.trim().is_empty() {
                        continue;
                    }
                    let page = doc.block_pages.get(i).copied().unwrap_or(0);
                    entries.push((lvl, title, page));
                }
            }
        }

        if entries.is_empty() {
            return to_js(&Vec::<OutlineItemJs>::new());
        }
        let min = entries.iter().map(|e| e.0).min().unwrap_or(0);
        let mut idx = 0usize;
        let tree = build_outline(&entries, &mut idx, min);
        to_js(&tree)
    }

    pub fn get_page_annotations(
        &self,
        _document_id: String,
        _page_index: usize,
    ) -> Result<JsValue, JsValue> {
        to_js(&Vec::<()>::new())
    }

    pub fn get_all_annotations(&self, _document_id: String) -> Result<JsValue, JsValue> {
        to_js(&HashMap::<String, Vec<()>>::new())
    }

    pub fn get_layout_page(
        &mut self,
        document_id: String,
        page_index: usize,
    ) -> Result<JsValue, JsValue> {
        if let Some(Loaded::Pdf(p)) = self.docs.get(&document_id) {
            let page = match p.pages.get(page_index) {
                Some(page) => pdf::content::layout_page(&p.file, page),
                None => return to_js(&render::LpPage { width: 0.0, height: 0.0, frames: Vec::new() }),
            };
            return to_js(&page);
        }
        if let Some(Loaded::Pptx(p)) = self.docs.get(&document_id) {
            let page = pptx::layout::layout_slide(&mut self.fonts, p, page_index);
            return to_js(&page);
        }
        let doc = self.ooxml(&document_id)?.clone();
        let page = render::layout_page(&mut self.fonts, &doc, page_index);
        to_js(&page)
    }

    pub fn get_visibility_groups(&self, _document_id: String) -> Result<JsValue, JsValue> {
        to_js(&Vec::<()>::new())
    }

    pub fn set_visibility_group_visible(
        &mut self,
        _document_id: String,
        _group_id: String,
        _visible: bool,
    ) -> bool {
        false
    }

    pub fn get_font_usage(&self, _document_id: String) -> Result<JsValue, JsValue> {
        to_js(&Vec::<()>::new())
    }

    pub fn get_bytes(&self, document_id: String) -> Result<Vec<u8>, JsValue> {
        match self.docs.get(&document_id) {
            Some(Loaded::Pdf(p)) => Ok(p.bytes().to_vec()),
            Some(Loaded::Pptx(p)) => Ok(p.bytes().to_vec()),
            Some(Loaded::Ooxml(d)) => Ok(d.bytes.clone()),
            None => Err(JsValue::from_str("document not found")),
        }
    }

    #[wasm_bindgen(js_name = registerFonts)]
    pub fn register_fonts(&mut self, _fonts: JsValue) {}

    #[wasm_bindgen(js_name = enableGoogleFonts)]
    pub fn enable_google_fonts(&mut self) {}

    /// Register a font from raw bytes. Call before load() for best results.
    /// Accepts TTF/OTF/WOFF2 bytes fetched from any source (Google Fonts, custom URL, etc).
    #[wasm_bindgen(js_name = registerFontData)]
    pub fn register_font_data(&mut self, bytes: Vec<u8>) {
        if bytes.len() >= 4 {
            self.fonts.db_mut().load_font_data(bytes);
        }
    }

    /// Return the list of font family names declared in a document.
    /// JS can use this to prefetch fonts before calling load().
    #[wasm_bindgen(js_name = getDeclaredFonts)]
    pub fn get_declared_fonts(&self, bytes: Vec<u8>) -> Result<JsValue, JsValue> {
        let names = if pdf::is_pdf(&bytes) {
            // Only fonts the PDF does not embed are worth prefetching.
            pdf::parse(&bytes).map(|d| d.declared_fonts()).unwrap_or_default()
        } else if xlsx::is_xlsx(&bytes) {
            xlsx::extract_font_declarations(&bytes)
        } else if pptx::is_pptx(&bytes) {
            pptx::parse(&bytes).map(|d| d.declared_fonts()).unwrap_or_default()
        } else {
            docx::extract_font_declarations(&bytes)
        };
        to_js(&names)
    }

    pub fn pdf_compose(&mut self, _compositions: JsValue, _doc_ids: JsValue) -> Result<JsValue, JsValue> {
        Err(JsValue::from_str("pdf operations not supported"))
    }

    pub fn pdf_split_by_outline(
        &mut self,
        _document_id: String,
        _max_level: i32,
        _split_mid_page: bool,
    ) -> Result<JsValue, JsValue> {
        Err(JsValue::from_str("pdf operations not supported"))
    }

    pub fn pdf_extract_images(
        &mut self,
        _document_id: String,
        _convert: bool,
    ) -> Result<JsValue, JsValue> {
        Err(JsValue::from_str("pdf operations not supported"))
    }

    pub fn pdf_extract_fonts(&mut self, _document_id: String) -> Result<JsValue, JsValue> {
        Err(JsValue::from_str("pdf operations not supported"))
    }

    pub fn pdf_compress(&mut self, _document_id: String) -> Result<Vec<u8>, JsValue> {
        Err(JsValue::from_str("pdf operations not supported"))
    }

    pub fn pdf_decompress(&mut self, _document_id: String) -> Result<Vec<u8>, JsValue> {
        Err(JsValue::from_str("pdf operations not supported"))
    }

    pub fn pdf_save_annotations(
        &mut self,
        _document_id: String,
        _annotations_by_page: JsValue,
    ) -> Result<Vec<u8>, JsValue> {
        Err(JsValue::from_str("pdf operations not supported"))
    }
}

impl Wasm {
    fn insert(&mut self, doc: Loaded) -> String {
        let id = format!("doc-{}", self.next_id);
        self.next_id += 1;
        self.docs.insert(id.clone(), doc);
        id
    }

    fn ooxml(&self, id: &str) -> Result<&Document, JsValue> {
        match self.docs.get(id) {
            Some(Loaded::Ooxml(d)) => Ok(d),
            Some(Loaded::Pdf(_)) => Err(JsValue::from_str("operation not valid for a PDF document")),
            Some(Loaded::Pptx(_)) => Err(JsValue::from_str("operation not valid for a PPTX document")),
            None => Err(JsValue::from_str("document not found")),
        }
    }
}

fn pdf_page_info(doc: &pdf::PdfDocument, index: usize) -> PageInfoJs {
    // The viewer rotates the page itself, so the size stays unrotated and the
    // rotation is reported alongside it.
    let (width, height) = match doc.pages.get(index) {
        Some(p) => p.size_unrotated(),
        None => (612.0, 792.0),
    };
    PageInfoJs {
        width,
        height,
        rotation: doc.page_rotation(index),
    }
}

/// Converts the document outline (`/Outlines`) into the viewer's tree.
fn pdf_outline(doc: &pdf::PdfDocument) -> Vec<OutlineItemJs> {
    let catalog = match doc.file.catalog_ref() {
        Some(c) => c,
        None => return Vec::new(),
    };
    let root = match doc.file.oget(&catalog, "Outlines") {
        Some(o) => o,
        None => return Vec::new(),
    };
    let first = match root.get("First") {
        Some(f) => f.clone(),
        None => return Vec::new(),
    };
    let mut seen = std::collections::HashSet::new();
    outline_siblings(doc, &first, 0, &mut seen)
}

fn outline_siblings(
    doc: &pdf::PdfDocument,
    first: &pdf::object::Obj,
    depth: usize,
    seen: &mut std::collections::HashSet<(u32, u16)>,
) -> Vec<OutlineItemJs> {
    let mut out = Vec::new();
    if depth > 32 {
        return out;
    }
    let mut cur = first.clone();
    // Bounded so a corrupt sibling loop cannot hang the worker.
    for _ in 0..4096 {
        if let Some(id) = cur.as_ref_id() {
            if !seen.insert(id) {
                break;
            }
        }
        let node = doc.file.resolve(&cur);
        let dict = match node.as_dict() {
            Some(d) => d,
            None => break,
        };
        let title = doc
            .file
            .dget(dict, "Title")
            .and_then(|t| t.as_str_bytes().map(pdf_text_string))
            .unwrap_or_default();

        let children = match dict.get("First") {
            Some(f) => outline_siblings(doc, &f.clone(), depth + 1, seen),
            None => Vec::new(),
        };
        // A negative /Count means the node is stored collapsed.
        let collapsed = doc
            .file
            .dget(dict, "Count")
            .and_then(|c| c.as_i64())
            .map(|c| c < 0)
            .unwrap_or(false);

        if !title.trim().is_empty() {
            out.push(OutlineItemJs {
                title,
                destination: pdf_destination(doc, dict),
                children,
                initially_collapsed: collapsed,
            });
        } else {
            out.extend(children);
        }

        match dict.get("Next") {
            Some(n) => cur = n.clone(),
            None => break,
        }
    }
    out
}

/// Resolves an outline entry's target page, through `/Dest` or a GoTo action.
fn pdf_destination(doc: &pdf::PdfDocument, dict: &pdf::object::Dict) -> Option<DestinationJs> {
    let dest = match doc.file.dget(dict, "Dest") {
        Some(d) => Some(d),
        None => doc
            .file
            .dget(dict, "A")
            .filter(|a| a.get("S").and_then(|s| s.as_name()) == Some("GoTo"))
            .and_then(|a| doc.file.oget(&a, "D")),
    }?;
    let page_index = doc.page_index_of_dest(&dest)?;
    Some(DestinationJs {
        page_index,
        display: DisplayJs { kind: "fit" },
    })
}

/// Decodes a PDF text string: UTF-16BE with a byte-order mark, else PDFDocEncoding
/// (close enough to Latin-1 for titles).
fn pdf_text_string(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return char::decode_utf16(units)
            .map(|r| r.unwrap_or('\u{FFFD}'))
            .collect();
    }
    bytes.iter().map(|b| *b as char).collect()
}

#[wasm_bindgen(js_name = parseFontInfo)]
pub fn parse_font_info(data: Vec<u8>) -> Result<JsValue, JsValue> {
    let (typeface, bold, italic) = match ttf_parser::Face::parse(&data, 0) {
        Ok(face) => {
            let mut name = String::new();
            for n in face.names() {
                if n.name_id == ttf_parser::name_id::FULL_NAME {
                    if let Some(s) = n.to_string() {
                        name = s;
                        break;
                    }
                }
            }
            if name.is_empty() {
                for n in face.names() {
                    if n.name_id == ttf_parser::name_id::FAMILY {
                        if let Some(s) = n.to_string() {
                            name = s;
                            break;
                        }
                    }
                }
            }
            (name, face.is_bold(), face.is_italic())
        }
        Err(_) => (String::new(), false, false),
    };

    let s = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
    FontInfoJs { typeface, bold, italic }
        .serialize(&s)
        .map_err(|e| JsValue::from_str(&e.to_string()))
}
