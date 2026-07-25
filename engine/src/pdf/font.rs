//! PDF fonts (ISO 32000-1 clause 9.5-9.8).
//!
//! Turns a font dictionary into three things the text renderer needs:
//! how to split a string into character codes, the advance for each code, and
//! the glyph outline in text space (1 unit = 1 em).
//!
//! Glyph programs come from the embedded font file when present
//! (`/FontFile2` TrueType, `/FontFile3` CFF or OpenType, `/FontFile` Type 1)
//! and otherwise from a bundled substitute chosen for metric compatibility.
//! Advances always come from the PDF's own `/Widths` or `/W` array, so glyph
//! positions stay correct even when the substitute's shapes differ.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::raster::{Path, Transform};

use super::cmap::{CMap, ToUnicode};
use super::encoding::{self, BaseEncoding};
use super::object::{Dict, Obj};
use super::type1::Type1Font;
use super::xref::PdfFile;

/// Where a font's glyph outlines come from.
enum Program {
    /// A complete sfnt (TrueType `glyf` or OpenType CFF), parsed with ttf-parser.
    Sfnt(Rc<Vec<u8>>),
    /// A bare CFF table (`/FontFile3` with subtype Type1C or CIDFontType0C).
    Cff(Rc<Vec<u8>>),
    Type1(Rc<Type1Font>),
    /// Glyphs are content streams (`/Subtype /Type3`).
    Type3,
    None,
}

/// One Type 3 glyph procedure.
pub struct Type3Data {
    pub char_procs: Dict,
    pub resources: Obj,
    pub matrix: Transform,
}

/// Advance widths, in 1/1000 em.
enum Widths {
    Simple {
        first: u32,
        widths: Vec<f64>,
        missing: f64,
    },
    Cid {
        default: f64,
        singles: HashMap<u32, f64>,
        ranges: Vec<(u32, u32, f64)>,
    },
    /// No width information in the PDF: fall back to the font program.
    FromProgram,
}

pub struct Font {
    /// `/BaseFont`, with any subset tag stripped.
    pub base_name: String,
    program: Program,
    /// Substitute used when the embedded program has no glyph, or when there is
    /// no embedded program at all.
    fallback: Option<Rc<Vec<u8>>>,
    /// True when the outlines come from a substitute rather than the document.
    pub substituted: bool,

    pub composite: bool,
    /// Code to CID map for composite fonts.
    pub cmap: CMap,
    pub to_unicode: Option<ToUnicode>,
    /// Simple fonts: code to glyph name.
    encoding: Vec<Option<String>>,
    /// True when the descriptor flags the font as symbolic (its built-in
    /// encoding wins over any standard one).
    symbolic: bool,
    widths: Widths,
    /// CID to GID map for `CIDFontType2`.
    cid_to_gid: Option<Rc<Vec<u8>>>,
    pub type3: Option<Type3Data>,
    /// Units per em of the glyph source, used to normalize outlines.
    units_per_em: f64,
    pub vertical: bool,

    /// Outlines in text space, keyed by CID (composite) or code (simple).
    outlines: RefCell<HashMap<u32, Option<Rc<Path>>>>,
    /// Advances read from the font program, keyed the same way.
    program_widths: RefCell<HashMap<u32, f64>>,
    /// Glyph id to character, derived from the embedded font's Unicode cmap.
    /// Built on demand as a last resort for text extraction.
    reverse_cmap: RefCell<Option<Rc<HashMap<u16, char>>>>,
}

impl Font {
    // ── loading ──────────────────────────────────────────────────────────────

    pub fn load(file: &PdfFile, dict: &Dict) -> Font {
        let subtype = dict.get("Subtype").and_then(|o| o.as_name()).unwrap_or("");
        if subtype == "Type0" {
            return Font::load_type0(file, dict);
        }
        if subtype == "Type3" {
            return Font::load_type3(file, dict);
        }
        Font::load_simple(file, dict, subtype)
    }

    fn load_simple(file: &PdfFile, dict: &Dict, subtype: &str) -> Font {
        let base_name = base_font_name(file, dict);
        let descriptor = file.dget(dict, "FontDescriptor");
        let flags = descriptor
            .as_ref()
            .and_then(|d| file.oget(d, "Flags"))
            .and_then(|o| o.as_i64())
            .unwrap_or(0);
        // Bit 3 (value 4) is Symbolic, bit 6 (value 32) is Nonsymbolic.
        let symbolic = flags & 4 != 0 && flags & 32 == 0;

        let (program, units_per_em) = load_program(file, descriptor.as_ref());
        let substituted = matches!(program, Program::None);
        let fallback = Some(substitute_for(&base_name, flags));

        // Encoding: base table, then /Differences.
        let mut encoding = default_encoding(&base_name, &program, symbolic, subtype);
        apply_encoding_dict(file, dict, &mut encoding);

        let widths = load_simple_widths(file, dict, descriptor.as_ref());
        let to_unicode = load_to_unicode(file, dict);

        let units_per_em = if units_per_em > 0.0 { units_per_em } else { 1000.0 };

        Font {
            base_name,
            program,
            fallback,
            substituted,
            composite: false,
            cmap: CMap::default(),
            to_unicode,
            encoding,
            symbolic,
            widths,
            cid_to_gid: None,
            type3: None,
            units_per_em,
            vertical: false,
            outlines: RefCell::new(HashMap::new()),
            program_widths: RefCell::new(HashMap::new()),
            reverse_cmap: RefCell::new(None),
        }
    }

    fn load_type0(file: &PdfFile, dict: &Dict) -> Font {
        let base_name = base_font_name(file, dict);

        // Encoding: a predefined CMap name or an embedded CMap stream.
        let cmap = match file.dget(dict, "Encoding") {
            Some(Obj::Name(n)) => CMap::predefined(&n),
            Some(Obj::Stream(s)) => match file.stream_data_of(&s) {
                Some(d) => CMap::parse(&d),
                None => CMap::identity(false),
            },
            _ => CMap::identity(false),
        };
        let vertical = cmap.vertical;

        let descendant = file
            .dget(dict, "DescendantFonts")
            .and_then(|o| o.as_array().and_then(|a| a.first().cloned()))
            .map(|o| file.resolve(&o));
        let desc_dict = descendant.as_ref().and_then(|o| o.as_dict().cloned()).unwrap_or_default();

        let descriptor = file.dget(&desc_dict, "FontDescriptor");
        let flags = descriptor
            .as_ref()
            .and_then(|d| file.oget(d, "Flags"))
            .and_then(|o| o.as_i64())
            .unwrap_or(0);
        let (program, units_per_em) = load_program(file, descriptor.as_ref());
        let substituted = matches!(program, Program::None);

        let cid_to_gid = match file.dget(&desc_dict, "CIDToGIDMap") {
            Some(Obj::Stream(s)) => file.stream_data_of(&s).map(Rc::new),
            _ => None,
        };

        let widths = load_cid_widths(file, &desc_dict);
        let to_unicode = load_to_unicode(file, dict);
        let units_per_em = if units_per_em > 0.0 { units_per_em } else { 1000.0 };

        Font {
            base_name: base_name.clone(),
            program,
            fallback: Some(substitute_for(&base_name, flags)),
            substituted,
            composite: true,
            cmap,
            to_unicode,
            encoding: vec![None; 256],
            symbolic: flags & 4 != 0,
            widths,
            cid_to_gid,
            type3: None,
            units_per_em,
            vertical,
            outlines: RefCell::new(HashMap::new()),
            program_widths: RefCell::new(HashMap::new()),
            reverse_cmap: RefCell::new(None),
        }
    }

    fn load_type3(file: &PdfFile, dict: &Dict) -> Font {
        let matrix = file
            .dget(dict, "FontMatrix")
            .and_then(|o| o.as_array().map(|a| a.to_vec()))
            .filter(|a| a.len() >= 6)
            .map(|a| {
                let v: Vec<f64> = a.iter().map(|o| o.as_f64().unwrap_or(0.0)).collect();
                Transform::new(v[0], v[1], v[2], v[3], v[4], v[5])
            })
            .unwrap_or(Transform::new(0.001, 0.0, 0.0, 0.001, 0.0, 0.0));

        let char_procs = file
            .dget(dict, "CharProcs")
            .and_then(|o| o.as_dict().cloned())
            .unwrap_or_default();
        let resources = file.dget(dict, "Resources").unwrap_or(Obj::Null);

        let mut encoding = vec![None; 256];
        apply_encoding_dict(file, dict, &mut encoding);

        Font {
            base_name: "Type3".to_string(),
            program: Program::Type3,
            fallback: None,
            substituted: false,
            composite: false,
            cmap: CMap::default(),
            to_unicode: load_to_unicode(file, dict),
            encoding,
            symbolic: true,
            widths: load_simple_widths(file, dict, None),
            cid_to_gid: None,
            type3: Some(Type3Data { char_procs, resources, matrix }),
            // Type 3 widths are in glyph space; the font matrix converts them.
            units_per_em: 1.0,
            vertical: false,
            outlines: RefCell::new(HashMap::new()),
            program_widths: RefCell::new(HashMap::new()),
            reverse_cmap: RefCell::new(None),
        }
    }

    // ── code iteration ───────────────────────────────────────────────────────

    /// Splits a PDF string into (code, cid, byte length) triples.
    pub fn decode(&self, bytes: &[u8]) -> Vec<(u32, u32, usize)> {
        let mut out = Vec::with_capacity(bytes.len());
        if !self.composite {
            for &b in bytes {
                out.push((b as u32, b as u32, 1));
            }
            return out;
        }
        let mut i = 0usize;
        while i < bytes.len() {
            let (code, len) = self.cmap.next_code(bytes, i);
            out.push((code, self.cmap.cid(code), len));
            i += len.max(1);
        }
        out
    }

    /// Advance for a code, in text-space units (1.0 = one em).
    pub fn advance(&self, code: u32, cid: u32) -> f64 {
        // Type 3 widths are in glyph space; the font matrix maps them to text
        // space, so they are not the usual 1/1000 em.
        if let Some(t3) = &self.type3 {
            let raw = match &self.widths {
                Widths::Simple { first, widths, .. } => code
                    .checked_sub(*first)
                    .and_then(|i| widths.get(i as usize))
                    .copied()
                    .unwrap_or(0.0),
                _ => 0.0,
            };
            return raw * t3.matrix.a;
        }
        match &self.widths {
            Widths::Simple { first, widths, missing } => {
                let idx = code.checked_sub(*first).map(|i| i as usize);
                match idx.and_then(|i| widths.get(i)) {
                    Some(w) => {
                        // A zero entry inside the array is meaningful for
                        // combining marks, but many producers pad with zeros
                        // past the real glyphs; only trust it if the glyph
                        // itself is blank.
                        if *w != 0.0 {
                            return w / 1000.0;
                        }
                        if self.glyph_is_blank(code, cid) {
                            return 0.0;
                        }
                        self.program_advance(code, cid).unwrap_or(*missing / 1000.0)
                    }
                    None => self
                        .program_advance(code, cid)
                        .filter(|_| *missing == 0.0)
                        .unwrap_or(*missing / 1000.0),
                }
            }
            Widths::Cid { default, singles, ranges } => {
                if let Some(w) = singles.get(&cid) {
                    return w / 1000.0;
                }
                for (lo, hi, w) in ranges {
                    if cid >= *lo && cid <= *hi {
                        return w / 1000.0;
                    }
                }
                default / 1000.0
            }
            Widths::FromProgram => self.program_advance(code, cid).unwrap_or(0.5),
        }
    }

    /// Unicode text for a code, for the selectable text layer.
    pub fn to_text(&self, code: u32, cid: u32) -> Option<String> {
        if let Some(tu) = &self.to_unicode {
            if let Some(s) = tu.get(code) {
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
        if !self.composite {
            let (from_program, name_matched) = self.char_from_program(code, cid);
            // The encoding name is only trustworthy when the glyph was actually
            // found through it. Subset fonts routinely reuse arbitrary codes
            // for ligatures and alternates, and then the name lies: the font's
            // own Unicode cmap is the authority.
            if name_matched {
                if let Some(name) = self.encoding.get(code as usize).and_then(|n| n.as_deref()) {
                    if let Some(c) = encoding::glyph_name_to_unicode(name) {
                        return Some(c.to_string());
                    }
                }
            }
            if let Some(c) = from_program {
                return Some(c.to_string());
            }
            if let Some(name) = self.encoding.get(code as usize).and_then(|n| n.as_deref()) {
                if let Some(c) = encoding::glyph_name_to_unicode(name) {
                    return Some(c.to_string());
                }
            }
            // Codes in a font with no usable encoding are usually Latin-1.
            if (32..=255).contains(&code) {
                return char::from_u32(code).map(|c| c.to_string());
            }
            return None;
        }
        // Composite font without /ToUnicode: an Identity CMap over a
        // Unicode-ordered font makes the CID the code point often enough to be
        // worth trying, but never for the low control range.
        if cid >= 32 {
            return char::from_u32(cid).map(|c| c.to_string());
        }
        None
    }

    /// Glyph outline in text space (y up, 1.0 = one em), cached per code.
    pub fn outline(&self, code: u32, cid: u32) -> Option<Rc<Path>> {
        let key = if self.composite { cid } else { code };
        if let Some(hit) = self.outlines.borrow().get(&key) {
            return hit.clone();
        }
        let built = self.build_outline(code, cid).map(Rc::new);
        self.outlines.borrow_mut().insert(key, built.clone());
        built
    }

    fn glyph_is_blank(&self, code: u32, cid: u32) -> bool {
        match self.outline(code, cid) {
            Some(p) => p.is_empty(),
            None => true,
        }
    }

    fn build_outline(&self, code: u32, cid: u32) -> Option<Path> {
        // Type 1 programs are addressed by glyph name.
        if let Program::Type1(t1) = &self.program {
            let name = self.glyph_name(code, t1);
            if let Some(n) = name {
                if let Some((path, _)) = t1.outline(&n) {
                    return Some(path.transformed(&t1.font_matrix));
                }
            }
        }

        if let Program::Sfnt(data) = &self.program {
            if let Some(face) = ttf_parser::Face::parse(data, 0).ok() {
                if let Some(gid) = self.gid_in_face(&face, code, cid) {
                    if let Some(p) = outline_from_face(&face, gid) {
                        return Some(p);
                    }
                }
            }
        }

        if let Program::Cff(data) = &self.program {
            if let Some(table) = ttf_parser::cff::Table::parse(data) {
                if let Some(gid) = self.gid_in_cff(&table, code, cid) {
                    if let Some(p) = outline_from_cff(&table, gid) {
                        return Some(p);
                    }
                }
            }
        }

        // Substitute: go through Unicode.
        let data = self.fallback.as_ref()?;
        let face = ttf_parser::Face::parse(data, 0).ok()?;
        let text = self.to_text(code, cid)?;
        let ch = text.chars().next()?;
        let gid = face.glyph_index(ch)?;
        outline_from_face(&face, gid)
    }

    /// Glyph name for a simple font code, honouring a Type 1 program's built-in
    /// encoding when the PDF gives none.
    fn glyph_name(&self, code: u32, t1: &Type1Font) -> Option<String> {
        if let Some(n) = self.encoding.get(code as usize).and_then(|n| n.clone()) {
            if t1.has_glyph(&n) {
                return Some(n);
            }
            // The PDF names a glyph the program does not have: try its own
            // encoding before giving up.
            if let Some(b) = t1.encoding.get(code as usize).and_then(|n| n.clone()) {
                if t1.has_glyph(&b) {
                    return Some(b);
                }
            }
            return Some(n);
        }
        t1.encoding.get(code as usize).and_then(|n| n.clone())
    }

    /// TrueType/OpenType glyph selection.
    ///
    /// Symbolic fonts address their `(3,0)` or `(1,0)` cmap with the raw code;
    /// nonsymbolic fonts go code to glyph name to Unicode to `(3,1)`. Both fall
    /// back to the `post` table and finally to treating the code as a glyph
    /// index, which is what viewers do for broken subset fonts.
    fn gid_in_face(
        &self,
        face: &ttf_parser::Face,
        code: u32,
        cid: u32,
    ) -> Option<ttf_parser::GlyphId> {
        self.resolve_gid(face, code, cid).map(|(g, _)| g)
    }

    /// Like `gid_in_face`, but also reports whether the glyph was found through
    /// the encoding's glyph name. When it was not, the name is not describing
    /// this glyph and must not be trusted for text extraction.
    fn resolve_gid(
        &self,
        face: &ttf_parser::Face,
        code: u32,
        cid: u32,
    ) -> Option<(ttf_parser::GlyphId, bool)> {
        use ttf_parser::PlatformId;

        if self.composite {
            let gid = match &self.cid_to_gid {
                Some(map) => {
                    let i = cid as usize * 2;
                    let hi = map.get(i).copied().unwrap_or(0) as u16;
                    let lo = map.get(i + 1).copied().unwrap_or(0) as u16;
                    (hi << 8) | lo
                }
                None => cid as u16,
            };
            return Some((ttf_parser::GlyphId(gid), false));
        }

        let cmap = face.tables().cmap;
        let name = self.encoding.get(code as usize).and_then(|n| n.as_deref());

        if self.symbolic {
            if let Some(cmap) = cmap {
                for sub in cmap.subtables {
                    let symbol = sub.platform_id == PlatformId::Windows && sub.encoding_id == 0;
                    let mac_roman = sub.platform_id == PlatformId::Macintosh && sub.encoding_id == 0;
                    if !symbol && !mac_roman {
                        continue;
                    }
                    // (3,0) subtables usually live in the F000 private area.
                    if let Some(g) = sub.glyph_index(0xF000 + code).or_else(|| sub.glyph_index(code)) {
                        return Some((g, false));
                    }
                }
            }
        }

        if let Some(n) = name {
            // A name that encodes an index refers to the glyph directly.
            if let Some(gid) = encoding::glyph_name_index(n) {
                if gid < face.number_of_glyphs() {
                    return Some((ttf_parser::GlyphId(gid), false));
                }
            }
            if let Some(ch) = encoding::glyph_name_to_unicode(n) {
                if let Some(cmap) = cmap {
                    for sub in cmap.subtables {
                        if sub.is_unicode() {
                            if let Some(g) = sub.glyph_index(ch as u32) {
                                return Some((g, true));
                            }
                        }
                    }
                }
            }
            if let Some(g) = face.glyph_index_by_name(n) {
                return Some((g, true));
            }
        }

        // Non-symbolic path exhausted: try the raw code through any subtable.
        if let Some(cmap) = cmap {
            for sub in cmap.subtables {
                if let Some(g) = sub.glyph_index(code).or_else(|| sub.glyph_index(0xF000 + code)) {
                    return Some((g, false));
                }
            }
            None
        } else {
            // No cmap at all: subset fonts index glyphs by code.
            Some((ttf_parser::GlyphId(code as u16), false))
        }
    }

    /// Bare-CFF glyph selection. CID-keyed CFF needs the charset to map CID to
    /// GID, which ttf-parser does not expose; identity holds for the subset
    /// fonts producers actually emit.
    fn gid_in_cff(
        &self,
        table: &ttf_parser::cff::Table,
        code: u32,
        _cid: u32,
    ) -> Option<ttf_parser::GlyphId> {
        if self.composite {
            return Some(ttf_parser::GlyphId(_cid as u16));
        }
        if let Some(n) = self.encoding.get(code as usize).and_then(|n| n.as_deref()) {
            if let Some(g) = table.glyph_index_by_name(n) {
                return Some(g);
            }
            if let Some(gid) = encoding::glyph_name_index(n) {
                return Some(ttf_parser::GlyphId(gid));
            }
        }
        // The CFF's own encoding, then the code as an index.
        if code <= 0xFF {
            if let Some(g) = table.glyph_index(code as u8) {
                return Some(g);
            }
        }
        Some(ttf_parser::GlyphId(code as u16))
    }

    /// Advance from the font program, in text-space units.
    fn program_advance(&self, code: u32, cid: u32) -> Option<f64> {
        let key = if self.composite { cid } else { code };
        if let Some(w) = self.program_widths.borrow().get(&key) {
            return Some(*w);
        }
        let w = self.compute_program_advance(code, cid)?;
        self.program_widths.borrow_mut().insert(key, w);
        Some(w)
    }

    fn compute_program_advance(&self, code: u32, cid: u32) -> Option<f64> {
        match &self.program {
            Program::Type1(t1) => {
                let name = self.glyph_name(code, t1)?;
                let (_, adv) = t1.outline(&name)?;
                // The font matrix maps glyph space to text space.
                Some(adv * t1.font_matrix.a)
            }
            Program::Sfnt(data) => {
                let face = ttf_parser::Face::parse(data, 0).ok()?;
                let gid = self.gid_in_face(&face, code, cid)?;
                let adv = face.glyph_hor_advance(gid)? as f64;
                Some(adv / face.units_per_em().max(1) as f64)
            }
            Program::Cff(data) => {
                let table = ttf_parser::cff::Table::parse(data)?;
                let gid = self.gid_in_cff(&table, code, cid)?;
                let adv = table.glyph_width(gid)? as f64;
                Some(adv / 1000.0)
            }
            // A substitute's own advances: correct for the metric-compatible
            // faces we bundle (Liberation Sans for Arial/Helvetica, and so on).
            Program::None => {
                let data = self.fallback.as_ref()?;
                let face = ttf_parser::Face::parse(data, 0).ok()?;
                let text = self.to_text(code, cid)?;
                let ch = text.chars().next()?;
                let gid = face.glyph_index(ch)?;
                let adv = face.glyph_hor_advance(gid)? as f64;
                Some(adv / face.units_per_em().max(1) as f64)
            }
            Program::Type3 => None,
        }
    }

    /// Reverse lookup: which character does the glyph this code selects stand
    /// for, according to the embedded font's own Unicode cmap?
    ///
    /// Also reports whether the glyph was found via the encoding's glyph name,
    /// which tells the caller how much to trust that name.
    fn char_from_program(&self, code: u32, cid: u32) -> (Option<char>, bool) {
        let data = match &self.program {
            Program::Sfnt(d) => d,
            // Type 1 and CFF programs are addressed by glyph name, so the name
            // is authoritative by construction.
            Program::Type1(_) | Program::Cff(_) => return (None, true),
            _ => return (None, true),
        };
        let face = match ttf_parser::Face::parse(data, 0) {
            Ok(f) => f,
            Err(_) => return (None, true),
        };
        let (gid, via_name) = match self.resolve_gid(&face, code, cid) {
            Some(v) => v,
            None => return (None, true),
        };

        if self.reverse_cmap.borrow().is_none() {
            *self.reverse_cmap.borrow_mut() = Some(Rc::new(build_reverse_cmap(&face)));
        }
        let map = self.reverse_cmap.borrow().clone();
        (map.and_then(|m| m.get(&gid.0).copied()), via_name)
    }

    /// Glyph-procedure name for a Type 3 code.
    pub fn glyph_name_for_type3(&self, code: u32) -> Option<String> {
        self.encoding.get(code as usize).and_then(|n| n.clone())
    }

    /// Font-wide vertical metrics in text space, for the text layer.
    pub fn ascent_descent(&self) -> (f64, f64) {
        let data = match &self.program {
            Program::Sfnt(d) => Some(d),
            _ => self.fallback.as_ref(),
        };
        if let Some(d) = data {
            if let Ok(face) = ttf_parser::Face::parse(d, 0) {
                let upem = face.units_per_em().max(1) as f64;
                return (face.ascender() as f64 / upem, face.descender() as f64 / upem);
            }
        }
        (0.75, -0.25)
    }

    /// Units per em of the active program, exposed for diagnostics.
    pub fn units_per_em(&self) -> f64 {
        self.units_per_em
    }
}

// ── outline extraction ───────────────────────────────────────────────────────

/// Collects an outline into a `raster::Path`, normalizing to a 1.0 em box.
struct OutlineSink {
    path: Path,
    scale: f64,
    open: bool,
}

impl ttf_parser::OutlineBuilder for OutlineSink {
    fn move_to(&mut self, x: f32, y: f32) {
        if self.open {
            self.path.close();
        }
        self.path.move_to(x as f64 * self.scale, y as f64 * self.scale);
        self.open = true;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.path.line_to(x as f64 * self.scale, y as f64 * self.scale);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        // Elevate the quadratic to a cubic: the control points sit one third of
        // the way from each end toward the quadratic control point.
        let (px, py) = match self.path.segs.last() {
            Some(crate::raster::Seg::MoveTo(x, y)) | Some(crate::raster::Seg::LineTo(x, y)) => (*x, *y),
            Some(crate::raster::Seg::CurveTo(_, _, _, _, x, y)) => (*x, *y),
            _ => (0.0, 0.0),
        };
        let (qx, qy) = (x1 as f64 * self.scale, y1 as f64 * self.scale);
        let (ex, ey) = (x as f64 * self.scale, y as f64 * self.scale);
        let c1 = (px + 2.0 / 3.0 * (qx - px), py + 2.0 / 3.0 * (qy - py));
        let c2 = (ex + 2.0 / 3.0 * (qx - ex), ey + 2.0 / 3.0 * (qy - ey));
        self.path.curve_to(c1.0, c1.1, c2.0, c2.1, ex, ey);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.path.curve_to(
            x1 as f64 * self.scale,
            y1 as f64 * self.scale,
            x2 as f64 * self.scale,
            y2 as f64 * self.scale,
            x as f64 * self.scale,
            y as f64 * self.scale,
        );
    }

    fn close(&mut self) {
        self.path.close();
        self.open = false;
    }
}

/// Inverts the font's character map. Unicode subtables win; a symbol subtable
/// in the `F000` private-use block is folded back to its low byte, which is how
/// symbol fonts encode plain ASCII.
fn build_reverse_cmap(face: &ttf_parser::Face) -> HashMap<u16, char> {
    use ttf_parser::PlatformId;
    let mut out: HashMap<u16, char> = HashMap::new();
    let cmap = match face.tables().cmap {
        Some(c) => c,
        None => return out,
    };
    // Two passes so a Unicode subtable always overrides a symbol one.
    for unicode_pass in [true, false] {
        for sub in cmap.subtables {
            let is_symbol = sub.platform_id == PlatformId::Windows && sub.encoding_id == 0;
            if sub.is_unicode() != unicode_pass || (unicode_pass && is_symbol) {
                continue;
            }
            if !unicode_pass && !is_symbol {
                continue;
            }
            sub.codepoints(|cp| {
                if let Some(gid) = sub.glyph_index(cp) {
                    let value = if is_symbol && (0xF000..=0xF0FF).contains(&cp) {
                        cp - 0xF000
                    } else {
                        cp
                    };
                    if let Some(c) = char::from_u32(value) {
                        if !c.is_control() {
                            out.entry(gid.0).or_insert(c);
                        }
                    }
                }
            });
        }
    }
    out
}

fn outline_from_face(face: &ttf_parser::Face, gid: ttf_parser::GlyphId) -> Option<Path> {
    let upem = face.units_per_em().max(1) as f64;
    let mut sink = OutlineSink { path: Path::new(), scale: 1.0 / upem, open: false };
    // A blank glyph (space) legitimately outlines to nothing.
    if face.outline_glyph(gid, &mut sink).is_none() && sink.path.is_empty() {
        // Distinguish "no such glyph" from "empty glyph": if the id is in range
        // treat it as empty, otherwise report a miss.
        if gid.0 >= face.number_of_glyphs() {
            return None;
        }
    }
    if sink.open {
        sink.path.close();
    }
    Some(sink.path)
}

fn outline_from_cff(table: &ttf_parser::cff::Table, gid: ttf_parser::GlyphId) -> Option<Path> {
    // CFF charstrings are in a 1000-unit em unless the font matrix says
    // otherwise; `Table::matrix` carries that.
    let m = table.matrix();
    let mut sink = OutlineSink { path: Path::new(), scale: 1.0, open: false };
    if table.outline(gid, &mut sink).is_err() && sink.path.is_empty() {
        return None;
    }
    if sink.open {
        sink.path.close();
    }
    let t = Transform::new(m.sx as f64, m.ky as f64, m.kx as f64, m.sy as f64, m.tx as f64, m.ty as f64);
    Some(sink.path.transformed(&t))
}

// ── dictionary helpers ───────────────────────────────────────────────────────

fn base_font_name(file: &PdfFile, dict: &Dict) -> String {
    let raw = file
        .dget(dict, "BaseFont")
        .and_then(|o| o.as_name().map(|s| s.to_string()))
        .unwrap_or_default();
    super::strip_subset_tag(&raw)
}

fn load_program(file: &PdfFile, descriptor: Option<&Obj>) -> (Program, f64) {
    let desc = match descriptor {
        Some(d) => d,
        None => return (Program::None, 1000.0),
    };

    // TrueType glyph outlines.
    if let Some(Obj::Stream(s)) = file.oget(desc, "FontFile2") {
        if let Some(data) = file.stream_data_of(&s) {
            if let Ok(face) = ttf_parser::Face::parse(&data, 0) {
                let upem = face.units_per_em().max(1) as f64;
                return (Program::Sfnt(Rc::new(data)), upem);
            }
        }
    }

    // CFF, or a full OpenType file.
    if let Some(Obj::Stream(s)) = file.oget(desc, "FontFile3") {
        let subtype = s.dict.get("Subtype").and_then(|o| o.as_name()).unwrap_or("");
        if let Some(data) = file.stream_data_of(&s) {
            if subtype == "OpenType" || data.starts_with(b"OTTO") || data.starts_with(&[0, 1, 0, 0]) {
                if let Ok(face) = ttf_parser::Face::parse(&data, 0) {
                    let upem = face.units_per_em().max(1) as f64;
                    return (Program::Sfnt(Rc::new(data)), upem);
                }
            }
            if ttf_parser::cff::Table::parse(&data).is_some() {
                return (Program::Cff(Rc::new(data)), 1000.0);
            }
        }
    }

    // Type 1.
    if let Some(Obj::Stream(s)) = file.oget(desc, "FontFile") {
        let len1 = file.dget(&s.dict, "Length1").and_then(|o| o.as_usize());
        if let Some(data) = file.stream_data_of(&s) {
            if let Some(t1) = Type1Font::parse(&data, len1) {
                let upem = if t1.font_matrix.a.abs() > 1e-12 { 1.0 / t1.font_matrix.a } else { 1000.0 };
                return (Program::Type1(Rc::new(t1)), upem);
            }
        }
    }

    (Program::None, 1000.0)
}

/// Chooses a bundled face for a font that is not embedded, or as a per-glyph
/// backstop for one that is.
fn substitute_for(base_name: &str, flags: i64) -> Rc<Vec<u8>> {
    use crate::fonts_bundled as fb;
    let lower = base_name.to_ascii_lowercase();

    let bold = lower.contains("bold")
        || lower.contains("black")
        || lower.contains("heavy")
        || lower.contains("semibold")
        // Bit 19 (value 1 << 18) is ForceBold.
        || flags & (1 << 18) != 0;
    let italic = lower.contains("italic") || lower.contains("oblique") || flags & (1 << 6) != 0;

    // Bit 1 (value 1) is FixedPitch, bit 2 (value 2) is Serif.
    let serif_flag = flags & 2 != 0;
    let serif = lower.contains("times")
        || lower.contains("serif") && !lower.contains("sans")
        || lower.contains("georgia")
        || lower.contains("garamond")
        || lower.contains("cambria")
        || lower.contains("book antiqua")
        || lower.contains("palatino")
        || lower.contains("roman")
        || lower.contains("minion")
        || (serif_flag && !lower.contains("arial") && !lower.contains("helvetica"));

    // Symbol and ZapfDingbats have no metric-compatible substitute; DejaVu Sans
    // at least covers the arrows, bullets and maths symbols they are used for.
    if lower.contains("symbol") || lower.contains("dingbat") || lower.contains("wingding") {
        return Rc::new(fb::DEJAVU_SANS.to_vec());
    }

    let data: &[u8] = if lower.contains("calibri") || lower.contains("carlito") {
        match (bold, italic) {
            (true, true) => fb::CARLITO_BOLD_ITALIC,
            (true, false) => fb::CARLITO_BOLD,
            (false, true) => fb::CARLITO_ITALIC,
            (false, false) => fb::CARLITO,
        }
    } else if serif {
        match (bold, italic) {
            (true, true) => fb::LIBERATION_SERIF_BOLD_ITALIC,
            (true, false) => fb::LIBERATION_SERIF_BOLD,
            (false, true) => fb::LIBERATION_SERIF_ITALIC,
            (false, false) => fb::LIBERATION_SERIF,
        }
    } else {
        match (bold, italic) {
            (true, true) => fb::LIBERATION_SANS_BOLD_ITALIC,
            (true, false) => fb::LIBERATION_SANS_BOLD,
            (false, true) => fb::LIBERATION_SANS_ITALIC,
            (false, false) => fb::LIBERATION_SANS,
        }
    };
    Rc::new(data.to_vec())
}

/// The encoding a simple font starts from, before `/Differences`.
fn default_encoding(
    base_name: &str,
    program: &Program,
    symbolic: bool,
    subtype: &str,
) -> Vec<Option<String>> {
    let lower = base_name.to_ascii_lowercase();

    // A Type 1 program's built-in encoding wins for symbolic fonts.
    if let Program::Type1(t1) = program {
        if symbolic || lower.contains("symbol") || lower.contains("dingbat") {
            if t1.encoding.iter().any(|e| e.is_some()) {
                return t1.encoding.clone();
            }
        }
    }

    // Symbolic TrueType fonts are addressed by raw code, so an encoding table
    // would only get in the way.
    if symbolic && subtype == "TrueType" {
        return vec![None; 256];
    }

    let base = if lower.contains("symbol") || lower.contains("dingbat") {
        // No table for these; codes go straight to the font's own cmap.
        return vec![None; 256];
    } else {
        BaseEncoding::Standard
    };
    encoding::base_table(base)
        .iter()
        .map(|n| n.map(|s| s.to_string()))
        .collect()
}

/// Applies `/Encoding`: either a base-encoding name or a dictionary with
/// `/BaseEncoding` and `/Differences`.
fn apply_encoding_dict(file: &PdfFile, dict: &Dict, enc: &mut Vec<Option<String>>) {
    let e = match file.dget(dict, "Encoding") {
        Some(e) => e,
        None => return,
    };
    if let Some(name) = e.as_name() {
        if let Some(base) = BaseEncoding::from_name(name) {
            *enc = encoding::base_table(base)
                .iter()
                .map(|n| n.map(|s| s.to_string()))
                .collect();
        }
        return;
    }
    let d = match e.as_dict() {
        Some(d) => d,
        None => return,
    };
    if let Some(base) = file
        .dget(d, "BaseEncoding")
        .and_then(|o| o.as_name().and_then(BaseEncoding::from_name))
    {
        *enc = encoding::base_table(base)
            .iter()
            .map(|n| n.map(|s| s.to_string()))
            .collect();
    }
    if let Some(diff) = file.dget(d, "Differences").and_then(|o| o.as_array().map(|a| a.to_vec())) {
        let mut code = 0usize;
        for item in diff {
            match file.resolve(&item) {
                Obj::Int(v) if v >= 0 => code = v as usize,
                Obj::Real(v) if v >= 0.0 => code = v as usize,
                Obj::Name(n) => {
                    if code < 256 {
                        enc[code] = Some(n.as_ref().clone());
                    }
                    code += 1;
                }
                _ => {}
            }
        }
    }
}

fn load_simple_widths(file: &PdfFile, dict: &Dict, descriptor: Option<&Obj>) -> Widths {
    let first = file.dget(dict, "FirstChar").and_then(|o| o.as_i64()).unwrap_or(0).max(0) as u32;
    let widths: Vec<f64> = file
        .dget(dict, "Widths")
        .and_then(|o| o.as_array().map(|a| a.to_vec()))
        .map(|a| a.iter().map(|o| file.resolve(o).as_f64().unwrap_or(0.0)).collect())
        .unwrap_or_default();
    if widths.is_empty() {
        return Widths::FromProgram;
    }
    let missing = descriptor
        .and_then(|d| file.oget(d, "MissingWidth"))
        .and_then(|o| o.as_f64())
        .unwrap_or(0.0);
    Widths::Simple { first, widths, missing }
}

/// Parses `/W`: a sequence of either `c [w …]` or `cFirst cLast w`.
fn load_cid_widths(file: &PdfFile, desc: &Dict) -> Widths {
    let default = file.dget(desc, "DW").and_then(|o| o.as_f64()).unwrap_or(1000.0);
    let mut singles: HashMap<u32, f64> = HashMap::new();
    let mut ranges: Vec<(u32, u32, f64)> = Vec::new();

    if let Some(w) = file.dget(desc, "W").and_then(|o| o.as_array().map(|a| a.to_vec())) {
        let items: Vec<Obj> = w.iter().map(|o| file.resolve(o)).collect();
        let mut i = 0usize;
        while i < items.len() {
            let start = match items[i].as_i64() {
                Some(v) if v >= 0 => v as u32,
                _ => {
                    i += 1;
                    continue;
                }
            };
            match items.get(i + 1) {
                Some(Obj::Array(list)) => {
                    for (k, item) in list.iter().enumerate() {
                        if let Some(v) = file.resolve(item).as_f64() {
                            singles.insert(start + k as u32, v);
                        }
                    }
                    i += 2;
                }
                Some(second) => {
                    let end = second.as_i64().unwrap_or(start as i64).max(start as i64) as u32;
                    let width = items.get(i + 2).and_then(|o| o.as_f64()).unwrap_or(default);
                    // A huge range is legal but must not be expanded.
                    ranges.push((start, end, width));
                    i += 3;
                }
                None => break,
            }
        }
    }
    Widths::Cid { default, singles, ranges }
}

fn load_to_unicode(file: &PdfFile, dict: &Dict) -> Option<ToUnicode> {
    let s = match file.dget(dict, "ToUnicode")? {
        Obj::Stream(s) => s,
        _ => return None,
    };
    let data = file.stream_data_of(&s)?;
    let tu = ToUnicode::parse(&data);
    if tu.is_empty() {
        None
    } else {
        Some(tu)
    }
}

/// Loads and caches the fonts named in a resource dictionary.
pub struct FontCache {
    fonts: HashMap<String, Rc<Font>>,
}

impl FontCache {
    pub fn new() -> FontCache {
        FontCache { fonts: HashMap::new() }
    }

    /// Looks up `/Font <name>` in `resources`. The cache key is the indirect
    /// object id when there is one, so the same font shared by several pages is
    /// parsed once.
    pub fn get(&mut self, file: &PdfFile, resources: &Obj, name: &str) -> Option<Rc<Font>> {
        let table = file.oget(resources, "Font")?;
        let entry = table.as_dict()?.get(name)?.clone();
        let key = match entry.as_ref_id() {
            Some((n, g)) => format!("{n}_{g}"),
            None => format!("inline:{name}:{:p}", table.as_dict()? as *const Dict),
        };
        if let Some(f) = self.fonts.get(&key) {
            return Some(f.clone());
        }
        let dict = file.resolve(&entry).as_dict()?.clone();
        let font = Rc::new(Font::load(file, &dict));
        self.fonts.insert(key, font.clone());
        Some(font)
    }
}

impl Default for FontCache {
    fn default() -> Self {
        FontCache::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_picks_metric_compatible_faces() {
        use crate::fonts_bundled as fb;
        assert_eq!(*substitute_for("Helvetica", 0), fb::LIBERATION_SANS.to_vec());
        assert_eq!(*substitute_for("Helvetica-Bold", 0), fb::LIBERATION_SANS_BOLD.to_vec());
        assert_eq!(*substitute_for("Times-Roman", 0), fb::LIBERATION_SERIF.to_vec());
        assert_eq!(*substitute_for("Times-BoldItalic", 0), fb::LIBERATION_SERIF_BOLD_ITALIC.to_vec());
        assert_eq!(*substitute_for("Calibri", 0), fb::CARLITO.to_vec());
        assert_eq!(*substitute_for("ZapfDingbats", 0), fb::DEJAVU_SANS.to_vec());
        // Serif flag in the descriptor, no hint in the name.
        assert_eq!(*substitute_for("FooBar", 2), fb::LIBERATION_SERIF.to_vec());
    }

    #[test]
    fn bundled_faces_all_parse() {
        for (i, data) in crate::fonts_bundled::ALL.iter().enumerate() {
            let face = ttf_parser::Face::parse(data, 0);
            assert!(face.is_ok(), "bundled face {i} does not parse");
            let face = face.unwrap();
            assert!(face.units_per_em() > 0);
            assert!(face.glyph_index('A').is_some() || face.number_of_glyphs() > 0);
        }
    }

    #[test]
    fn substitute_outline_and_advance_are_sane() {
        // Liberation Sans 'H': roughly 0.72 em tall, advance 0.722 em.
        let data = Rc::new(crate::fonts_bundled::LIBERATION_SANS.to_vec());
        let face = ttf_parser::Face::parse(&data, 0).unwrap();
        let gid = face.glyph_index('H').unwrap();
        let path = outline_from_face(&face, gid).unwrap();
        let (_, y0, _, y1) = path.bounds().unwrap();
        assert!(y0.abs() < 0.02, "baseline sits at 0, got y0={y0}");
        assert!((y1 - 0.72).abs() < 0.05, "cap height ~0.72 em, got {y1}");
        let adv = face.glyph_hor_advance(gid).unwrap() as f64 / face.units_per_em() as f64;
        assert!((adv - 0.722).abs() < 0.02, "advance ~0.722 em, got {adv}");
    }
}
