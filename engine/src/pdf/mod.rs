//! PDF support for anicca-engine.
//!
//! Written from scratch against ISO 32000-1. Module layout:
//!
//! ```text
//! lexer.rs       syntax tokenizer + direct-object parser
//! object.rs      the object model (Obj/Dict/Stream)
//! xref.rs        PdfFile: xref chain, object streams, indirect-object access
//! filter.rs      stream filters and predictors
//! page.rs        page tree traversal and inherited attributes
//! colorspace.rs  colour spaces, reduced to device RGB
//! content.rs     content-stream interpreter, painting into raster::Canvas
//! ```

pub mod cmap;
pub mod colorspace;
pub mod content;
pub mod encoding;
pub mod filter;
pub mod font;
pub mod image;
pub mod lexer;
pub mod object;
pub mod page;
pub mod text;
pub mod type1;
pub mod xref;

use page::Page;
use xref::PdfFile;

/// Detects a PDF header. Producers sometimes prepend junk, so the header is
/// searched for in the first kilobyte rather than required at offset 0.
pub fn is_pdf(bytes: &[u8]) -> bool {
    let window = &bytes[..bytes.len().min(1024)];
    xref::find(window, b"%PDF-").is_some()
}

pub struct PdfDocument {
    pub file: PdfFile,
    pub pages: Vec<Page>,
}

impl PdfDocument {
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Displayed page size in points, after `/Rotate`.
    pub fn page_size(&self, index: usize) -> (f32, f32) {
        match self.pages.get(index) {
            Some(p) => p.size(),
            None => (612.0, 792.0),
        }
    }

    pub fn page_rotation(&self, index: usize) -> i32 {
        self.pages.get(index).map(|p| p.rotate).unwrap_or(0)
    }

    /// Original file bytes, as handed to `parse`.
    pub fn bytes(&self) -> &[u8] {
        &self.file.bytes
    }

    /// Resolves a destination to a page index.
    ///
    /// Accepts the explicit array form `[pageRef /Fit …]`, a page number (used
    /// by remote destinations), and named destinations resolved through the
    /// catalog's `/Dests` dictionary or `/Names /Dests` name tree.
    pub fn page_index_of_dest(&self, dest: &object::Obj) -> Option<usize> {
        self.page_index_of_dest_depth(dest, 0)
    }

    fn page_index_of_dest_depth(&self, dest: &object::Obj, depth: usize) -> Option<usize> {
        use object::Obj;
        if depth > 8 {
            return None;
        }
        let dest = self.file.resolve(dest);
        match &dest {
            Obj::Array(a) => {
                let first = a.first()?;
                if let Some((num, _)) = first.as_ref_id() {
                    return self.pages.iter().position(|p| p.obj_num == Some(num));
                }
                // A bare integer is a zero-based page number.
                first.as_usize().filter(|i| *i < self.pages.len())
            }
            // A dictionary destination wraps the array in /D.
            Obj::Dict(_) => {
                let inner = self.file.oget(&dest, "D")?;
                self.page_index_of_dest_depth(&inner, depth + 1)
            }
            Obj::Name(n) => self.lookup_named_dest(n.as_bytes(), depth),
            Obj::Str(s) => self.lookup_named_dest(s, depth),
            _ => None,
        }
    }

    fn lookup_named_dest(&self, name: &[u8], depth: usize) -> Option<usize> {
        let catalog = self.file.catalog_ref()?;
        // PDF 1.1 style: a flat /Dests dictionary keyed by name.
        if let Some(dests) = self.file.oget(&catalog, "Dests") {
            if let Some(d) = dests.as_dict() {
                let key = String::from_utf8_lossy(name).into_owned();
                if let Some(v) = self.file.dget(d, &key) {
                    return self.page_index_of_dest_depth(&v, depth + 1);
                }
            }
        }
        // PDF 1.2 style: a /Names /Dests name tree.
        let names = self.file.oget(&catalog, "Names")?;
        let tree = self.file.oget(&names, "Dests")?;
        let found = self.search_name_tree(&tree, name, 0)?;
        self.page_index_of_dest_depth(&found, depth + 1)
    }

    /// Walks a name tree, which is a balanced tree of `/Kids` with `/Limits`
    /// and leaf `/Names` arrays of alternating key and value.
    fn search_name_tree(&self, node: &object::Obj, key: &[u8], depth: usize) -> Option<object::Obj> {
        if depth > 32 {
            return None;
        }
        let dict = node.as_dict()?;
        if let Some(names) = self.file.dget(dict, "Names").and_then(|o| o.as_array().map(|a| a.to_vec())) {
            let mut i = 0usize;
            while i + 1 < names.len() {
                let k = self.file.resolve(&names[i]);
                if k.as_str_bytes() == Some(key) {
                    return Some(names[i + 1].clone());
                }
                i += 2;
            }
            return None;
        }
        let kids = self.file.dget(dict, "Kids")?.as_array().map(|a| a.to_vec())?;
        for kid in kids {
            let k = self.file.resolve(&kid);
            // Skip subtrees whose /Limits exclude the key.
            if let Some(limits) = k.as_dict().and_then(|d| self.file.dget(d, "Limits")) {
                if let Some(l) = limits.as_array() {
                    let lo = l.first().and_then(|o| o.as_str_bytes().map(|b| b.to_vec()));
                    let hi = l.get(1).and_then(|o| o.as_str_bytes().map(|b| b.to_vec()));
                    if let (Some(lo), Some(hi)) = (lo, hi) {
                        if key < lo.as_slice() || key > hi.as_slice() {
                            continue;
                        }
                    }
                }
            }
            if let Some(found) = self.search_name_tree(&k, key, depth + 1) {
                return Some(found);
            }
        }
        None
    }

    /// Font family names referenced by the document that are *not* embedded.
    /// The viewer prefetches web fonts for these before rendering.
    pub fn declared_fonts(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for page in &self.pages {
            let fonts = match self
                .file
                .oget(&page.resources, "Font")
                .and_then(|f| f.as_dict().cloned())
            {
                Some(d) => d,
                None => continue,
            };
            for key in fonts.keys() {
                let font = match self.file.dget(&fonts, key) {
                    Some(f) => f,
                    None => continue,
                };
                if font_is_embedded(&self.file, &font) {
                    continue;
                }
                if let Some(base) = self.file.oget(&font, "BaseFont").and_then(|b| b.as_name().map(|s| s.to_string())) {
                    let clean = strip_subset_tag(&base);
                    if seen.insert(clean.clone()) {
                        names.push(clean);
                    }
                }
            }
        }
        names
    }
}

/// A subset prefix looks like `ABCDEF+Arial`; strip it to get the real family.
pub fn strip_subset_tag(base: &str) -> String {
    let b = base.as_bytes();
    if b.len() > 7 && b[6] == b'+' && b[..6].iter().all(|c| c.is_ascii_uppercase()) {
        base[7..].to_string()
    } else {
        base.to_string()
    }
}

fn font_is_embedded(file: &PdfFile, font: &object::Obj) -> bool {
    // Type0 fonts carry the descriptor on the descendant.
    let holder = match file.oget(font, "DescendantFonts") {
        Some(d) => match d.as_array().and_then(|a| a.first().cloned()) {
            Some(first) => file.resolve(&first),
            None => font.clone(),
        },
        None => font.clone(),
    };
    let desc = match file.oget(&holder, "FontDescriptor") {
        Some(d) => d,
        None => return false,
    };
    ["FontFile", "FontFile2", "FontFile3"]
        .iter()
        .any(|k| desc.get(k).is_some())
}

pub fn parse(bytes: &[u8]) -> Result<PdfDocument, String> {
    let file = PdfFile::open(bytes.to_vec())?;
    let mut doc = PdfDocument { file, pages: Vec::new() };
    doc.pages = page::collect_pages(&doc.file);

    if doc.pages.is_empty() && !doc.file.recovered {
        // The catalog resolved but produced no pages: retry from a full scan.
        doc.file.recover();
        doc.pages = page::collect_pages(&doc.file);
    }
    if doc.pages.is_empty() {
        return Err("pdf: document has no pages".to_string());
    }
    Ok(doc)
}
