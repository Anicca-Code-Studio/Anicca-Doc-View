//! Anicca Doc View rendering engine (WASM).
//! Copyright (c) 2026 Anicca Code Studio. MIT licensed.
//!
//! Exposes a `Wasm` class plus `parseFontInfo`, matching the interface the
//! viewer's worker expects.

pub mod docx;
pub mod model;
pub mod render;

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
struct PageGroupJs {
    start_page_index: usize,
    page_count: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LayoutPageJs {
    width: f32,
    height: f32,
    frames: Vec<()>,
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

#[wasm_bindgen]
pub struct Wasm {
    fonts: FontSystem,
    swash: SwashCache,
    docs: HashMap<String, Document>,
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
        if !docx::is_zip(&bytes) {
            return Err(JsValue::from_str(
                "anicca-engine: only DOCX is supported in this build",
            ));
        }
        let mut doc = docx::parse(&bytes).map_err(|e| JsValue::from_str(&e))?;

        // Load embedded fonts from the DOCX into our FontSystem
        render::load_embedded_fonts(&mut self.fonts, &doc.embedded_fonts);

        // Compute layout metrics
        let (page_count, block_pages) = render::measure(&mut self.fonts, &doc);
        doc.page_count = page_count;
        doc.block_pages = block_pages;

        let id = format!("doc-{}", self.next_id);
        self.next_id += 1;
        self.docs.insert(id.clone(), doc);
        Ok(id)
    }

    pub fn document_format(&self, _document_id: String) -> String {
        "docx".to_string()
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
        self.docs.get(&document_id).map(|d| d.page_count).unwrap_or(0)
    }

    pub fn page_info(&self, document_id: String, _page_index: usize) -> Result<JsValue, JsValue> {
        let doc = self.doc(&document_id)?;
        to_js(&PageInfoJs {
            width: doc.page_w_pt,
            height: doc.page_h_pt,
            rotation: 0,
        })
    }

    pub fn all_page_info(&self, document_id: String) -> Result<JsValue, JsValue> {
        let doc = self.doc(&document_id)?;
        let pages: Vec<PageInfoJs> = (0..doc.page_count)
            .map(|_| PageInfoJs {
                width: doc.page_w_pt,
                height: doc.page_h_pt,
                rotation: 0,
            })
            .collect();
        to_js(&pages)
    }

    pub fn page_groups(&self, document_id: String) -> Result<JsValue, JsValue> {
        let doc = self.doc(&document_id)?;
        to_js(&vec![PageGroupJs {
            start_page_index: 0,
            page_count: doc.page_count,
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
        let rgba = render::render_page(&mut self.fonts, &mut self.swash, &doc, page_index, width, height);
        self.docs.insert(document_id, doc);
        Ok(rgba)
    }

    pub fn render_page_gpu(
        &mut self,
        _document_id: String,
        _page_index: usize,
        _width: usize,
        _height: usize,
    ) -> Result<Vec<u8>, JsValue> {
        Err(JsValue::from_str("gpu rendering not supported"))
    }

    pub fn get_outline(&self, document_id: String) -> Result<JsValue, JsValue> {
        let doc = self.doc(&document_id)?;
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
        &self,
        document_id: String,
        _page_index: usize,
    ) -> Result<JsValue, JsValue> {
        let doc = self.doc(&document_id)?;
        to_js(&LayoutPageJs {
            width: doc.page_w_pt,
            height: doc.page_h_pt,
            frames: Vec::new(),
        })
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
        let doc = self.doc(&document_id)?;
        Ok(doc.bytes.clone())
    }

    #[wasm_bindgen(js_name = registerFonts)]
    pub fn register_fonts(&mut self, _fonts: JsValue) {}

    #[wasm_bindgen(js_name = enableGoogleFonts)]
    pub fn enable_google_fonts(&mut self) {}

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
    fn doc(&self, id: &str) -> Result<&Document, JsValue> {
        self.docs
            .get(id)
            .ok_or_else(|| JsValue::from_str("document not found"))
    }
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
