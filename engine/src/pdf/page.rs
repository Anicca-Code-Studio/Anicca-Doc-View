//! Page tree traversal (ISO 32000-1 clause 7.7.3).
//!
//! Collects the leaf `/Page` nodes in document order and folds in the four
//! inheritable attributes (`Resources`, `MediaBox`, `CropBox`, `Rotate`).

use std::collections::HashSet;

use super::object::{rect_from, Dict, Obj};
use super::xref::PdfFile;

/// US Letter, used when a page declares no usable MediaBox.
pub const DEFAULT_MEDIA_BOX: [f64; 4] = [0.0, 0.0, 612.0, 792.0];

/// Guards against pathological or malicious page trees.
const MAX_DEPTH: usize = 64;
const MAX_PAGES: usize = 100_000;

#[derive(Clone, Debug)]
pub struct Page {
    /// Object number of the page, used to resolve outline and link
    /// destinations that reference it.
    pub obj_num: Option<u32>,
    pub dict: Dict,
    /// Inherited `/Resources`, already resolved. `Obj::Null` when absent.
    pub resources: Obj,
    /// Effective crop box: `/CropBox` intersected with `/MediaBox`, falling
    /// back to the media box. This is the area a viewer displays.
    pub crop: [f64; 4],
    pub media: [f64; 4],
    /// `/Rotate`, normalized to 0/90/180/270.
    pub rotate: i32,
}

impl Page {
    /// Crop-box size in points, ignoring `/Rotate`. This is what the viewer
    /// wants: it reports the rotation separately and turns the canvas itself.
    pub fn size_unrotated(&self) -> (f32, f32) {
        (
            (self.crop[2] - self.crop[0]) as f32,
            (self.crop[3] - self.crop[1]) as f32,
        )
    }

    /// Displayed size in points, after applying `/Rotate`.
    pub fn size(&self) -> (f32, f32) {
        let (w, h) = self.size_unrotated();
        if self.rotate == 90 || self.rotate == 270 {
            (h, w)
        } else {
            (w, h)
        }
    }
}

/// Inheritable attributes carried down the tree.
#[derive(Clone, Default)]
struct Inherited {
    resources: Option<Obj>,
    media: Option<[f64; 4]>,
    crop: Option<[f64; 4]>,
    rotate: Option<i32>,
}

impl Inherited {
    fn merged_with(&self, file: &PdfFile, node: &Dict) -> Inherited {
        let mut out = self.clone();
        if let Some(r) = file.dget(node, "Resources") {
            if r.as_dict().is_some() {
                out.resources = Some(r);
            }
        }
        if let Some(m) = file.dget(node, "MediaBox").as_ref().and_then(resolve_rect(file)) {
            out.media = Some(m);
        }
        if let Some(c) = file.dget(node, "CropBox").as_ref().and_then(resolve_rect(file)) {
            out.crop = Some(c);
        }
        if let Some(r) = file.dget(node, "Rotate").and_then(|o| o.as_i64()) {
            out.rotate = Some(normalize_rotate(r));
        }
        out
    }
}

/// Rect entries are sometimes arrays of indirect references.
fn resolve_rect(file: &PdfFile) -> impl Fn(&Obj) -> Option<[f64; 4]> + '_ {
    move |obj: &Obj| {
        if let Some(r) = rect_from(obj) {
            return Some(r);
        }
        let a = obj.as_array()?;
        if a.len() < 4 {
            return None;
        }
        let v: Vec<Obj> = a.iter().map(|o| file.resolve(o)).collect();
        rect_from(&Obj::Array(std::rc::Rc::new(v)))
    }
}

pub fn normalize_rotate(r: i64) -> i32 {
    let mut r = (r % 360) as i32;
    if r < 0 {
        r += 360;
    }
    // Round to the nearest quarter turn; non-multiples of 90 are invalid.
    ((r + 45) / 90 * 90) % 360
}

/// Collects every page, in order.
pub fn collect_pages(file: &PdfFile) -> Vec<Page> {
    let mut out = Vec::new();
    let catalog = match file.catalog_ref() {
        Some(c) => c,
        None => return out,
    };
    if let Some(pages) = file.oget(&catalog, "Pages") {
        let mut visited: HashSet<u32> = HashSet::new();
        let root_ref = catalog.get("Pages").and_then(|o| o.as_ref_id()).map(|(n, _)| n);
        if let Some(n) = root_ref {
            visited.insert(n);
        }
        walk(file, &pages, root_ref, &Inherited::default(), 0, &mut visited, &mut out);
    }
    if out.is_empty() {
        out = scan_for_pages(file);
    }
    out
}

fn walk(
    file: &PdfFile,
    node: &Obj,
    obj_num: Option<u32>,
    inherited: &Inherited,
    depth: usize,
    visited: &mut HashSet<u32>,
    out: &mut Vec<Page>,
) {
    if depth > MAX_DEPTH || out.len() >= MAX_PAGES {
        return;
    }
    let dict = match node.as_dict() {
        Some(d) => d,
        None => return,
    };
    let inh = inherited.merged_with(file, dict);
    let node_type = dict.get("Type").and_then(|o| o.as_name());

    let kids = file.dget(dict, "Kids");
    let is_leaf = match node_type {
        Some("Page") => true,
        Some("Pages") => false,
        // Broken files omit /Type; a node with /Kids is internal.
        None => kids.is_none(),
        _ => kids.is_none(),
    };

    if is_leaf {
        out.push(make_page(dict.clone(), obj_num, &inh));
        return;
    }

    let kids = match kids.as_ref().and_then(|k| k.as_array().map(|a| a.to_vec())) {
        Some(k) => k,
        None => return,
    };
    for kid in kids {
        // Cycle guard: a node must not appear twice in one traversal.
        let kid_num = kid.as_ref_id().map(|(n, _)| n);
        if let Some(n) = kid_num {
            if !visited.insert(n) {
                continue;
            }
        }
        let resolved = file.resolve(&kid);
        walk(file, &resolved, kid_num, &inh, depth + 1, visited, out);
    }
}

fn make_page(dict: Dict, obj_num: Option<u32>, inh: &Inherited) -> Page {
    let media = inh.media.unwrap_or(DEFAULT_MEDIA_BOX);
    // A degenerate box is worse than no box at all.
    let media = if media[2] - media[0] < 1.0 || media[3] - media[1] < 1.0 {
        DEFAULT_MEDIA_BOX
    } else {
        media
    };
    let crop = match inh.crop {
        Some(c) => {
            let x0 = c[0].max(media[0]);
            let y0 = c[1].max(media[1]);
            let x1 = c[2].min(media[2]);
            let y1 = c[3].min(media[3]);
            if x1 - x0 >= 1.0 && y1 - y0 >= 1.0 {
                [x0, y0, x1, y1]
            } else {
                media
            }
        }
        None => media,
    };
    Page {
        obj_num,
        dict,
        resources: inh.resources.clone().unwrap_or(Obj::Null),
        crop,
        media,
        rotate: inh.rotate.unwrap_or(0),
    }
}

/// Fallback for files whose page tree is unusable: take every object that
/// looks like a page, in object-number order.
fn scan_for_pages(file: &PdfFile) -> Vec<Page> {
    let mut nums: Vec<u32> = file.entries.keys().copied().collect();
    nums.sort_unstable();
    let mut out = Vec::new();
    for num in nums {
        if out.len() >= MAX_PAGES {
            break;
        }
        let obj = file.get_object(num);
        let d = match obj.as_dict() {
            Some(d) => d,
            None => continue,
        };
        if d.get("Type").and_then(|o| o.as_name()) != Some("Page") {
            continue;
        }
        // Rebuild the inherited chain by walking /Parent upwards.
        let mut chain: Vec<Dict> = Vec::new();
        let mut cur = obj.clone();
        for _ in 0..MAX_DEPTH {
            let parent = match cur.as_dict().and_then(|d| file.dget(d, "Parent")) {
                Some(p) => p,
                None => break,
            };
            match parent.as_dict() {
                Some(pd) => chain.push(pd.clone()),
                None => break,
            }
            cur = parent;
        }
        let mut inh = Inherited::default();
        for anc in chain.iter().rev() {
            inh = inh.merged_with(file, anc);
        }
        inh = inh.merged_with(file, d);
        out.push(make_page(d.clone(), Some(num), &inh));
    }
    out
}

/// The page's content streams, concatenated with a newline between parts
/// (a `/Contents` array may split a single token stream across objects, but
/// the spec requires treating the parts as one stream with whitespace joins).
pub fn content_bytes(file: &PdfFile, page: &Page) -> Vec<u8> {
    let contents = match file.dget(&page.dict, "Contents") {
        Some(c) => c,
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    match &contents {
        Obj::Stream(s) => {
            if let Some(d) = file.stream_data_of(s) {
                out.extend_from_slice(&d);
            }
        }
        Obj::Array(a) => {
            for item in a.iter() {
                let r = file.resolve(item);
                if let Some(s) = r.as_stream() {
                    if let Some(d) = file.stream_data_of(s) {
                        out.extend_from_slice(&d);
                        out.push(b'\n');
                    }
                }
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotate_normalization() {
        assert_eq!(normalize_rotate(0), 0);
        assert_eq!(normalize_rotate(90), 90);
        assert_eq!(normalize_rotate(-90), 270);
        assert_eq!(normalize_rotate(450), 90);
        assert_eq!(normalize_rotate(360), 0);
    }
}
