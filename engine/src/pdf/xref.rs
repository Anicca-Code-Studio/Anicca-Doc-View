//! Cross-reference resolution and indirect-object access.
//!
//! `PdfFile` owns the raw bytes and knows how to turn an object number into a
//! parsed `Obj`. It handles all four ways a real file can be laid out:
//! classic `xref` tables, xref streams (PDF 1.5+), object streams, and
//! incremental-update chains. When the table is unusable it falls back to
//! scanning the whole file for `N G obj` headers.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::filter;
use super::lexer::{is_ws, Lexer};
use super::object::{Dict, Obj, Stream};

/// Where an object lives.
#[derive(Clone, Copy, Debug)]
pub enum XrefEntry {
    /// Byte offset of `N G obj` in the file.
    Offset { offset: usize, gen: u16 },
    /// Packed inside an object stream: its object number and the index within.
    InStream { stm: u32, idx: u32 },
}

/// Hook for the standard security handler (implemented in `crypt.rs`).
/// Kept as a trait so the object layer has no dependency on the crypto code.
pub trait Decryptor {
    fn decrypt_stream(&self, num: u32, gen: u16, data: &[u8]) -> Vec<u8>;
    fn decrypt_string(&self, num: u32, gen: u16, data: &[u8]) -> Vec<u8>;
}

struct ObjStmData {
    data: Vec<u8>,
    /// (object number, offset within `data`) for each packed object.
    pairs: Vec<(u32, usize)>,
}

pub struct PdfFile {
    pub bytes: Vec<u8>,
    pub entries: HashMap<u32, XrefEntry>,
    pub trailer: Dict,
    /// Set when the xref chain was unusable and objects came from a full scan.
    pub recovered: bool,
    pub decryptor: Option<Box<dyn Decryptor>>,
    cache: RefCell<HashMap<u32, Obj>>,
    objstm_cache: RefCell<HashMap<u32, Rc<ObjStmData>>>,
    /// Guards against reference cycles while resolving.
    in_progress: RefCell<HashSet<u32>>,
}

impl PdfFile {
    // ── construction ─────────────────────────────────────────────────────────

    pub fn open(bytes: Vec<u8>) -> Result<PdfFile, String> {
        let mut file = PdfFile {
            bytes,
            entries: HashMap::new(),
            trailer: HashMap::new(),
            recovered: false,
            decryptor: None,
            cache: RefCell::new(HashMap::new()),
            objstm_cache: RefCell::new(HashMap::new()),
            in_progress: RefCell::new(HashSet::new()),
        };
        file.build_xref();
        if file.catalog_ref().is_none() {
            file.recover();
        }
        if file.catalog_ref().is_none() {
            return Err("pdf: no document catalog found".to_string());
        }
        Ok(file)
    }

    fn build_xref(&mut self) {
        let start = match self.find_startxref() {
            Some(s) => s,
            None => return,
        };
        let mut visited: HashSet<usize> = HashSet::new();
        let mut queue = vec![start];
        // The chain runs newest-first; the first writer of an entry wins.
        while let Some(off) = queue.pop() {
            if off >= self.bytes.len() || !visited.insert(off) {
                continue;
            }
            let trailer = match self.parse_xref_section(off) {
                Some(t) => t,
                None => continue,
            };
            // A hybrid-reference file points at an extra xref stream.
            if let Some(x) = trailer.get("XRefStm").and_then(|o| o.as_usize()) {
                queue.push(x);
            }
            if let Some(p) = trailer.get("Prev").and_then(|o| o.as_usize()) {
                queue.push(p);
            }
            for (k, v) in trailer {
                self.trailer.entry(k).or_insert(v);
            }
        }
    }

    /// Locates the `startxref` offset written near the end of the file.
    fn find_startxref(&self) -> Option<usize> {
        let n = self.bytes.len();
        let tail_start = n.saturating_sub(2048);
        let tail = &self.bytes[tail_start..];
        let idx = rfind(tail, b"startxref")?;
        let mut lx = Lexer::at(&self.bytes, tail_start + idx + b"startxref".len());
        lx.skip_ws();
        let kw = lx.read_keyword();
        std::str::from_utf8(kw).ok()?.trim().parse::<usize>().ok()
    }

    /// Parses one xref section (table or stream) and returns its trailer.
    fn parse_xref_section(&mut self, offset: usize) -> Option<Dict> {
        let mut lx = Lexer::at(&self.bytes, offset);
        lx.skip_ws();
        let save = lx.pos;
        if lx.read_keyword() == b"xref" {
            return self.parse_xref_table(lx.pos);
        }
        lx.pos = save;
        self.parse_xref_stream(offset)
    }

    /// Classic `xref` table: a series of `first count` subsections, each with
    /// `count` twenty-byte entries, terminated by `trailer <<…>>`.
    fn parse_xref_table(&mut self, pos: usize) -> Option<Dict> {
        let bytes = std::mem::take(&mut self.bytes);
        let mut lx = Lexer::at(&bytes, pos);
        let mut trailer: Option<Dict> = None;

        loop {
            lx.skip_ws();
            let save = lx.pos;
            let kw = lx.read_keyword();
            if kw == b"trailer" {
                lx.skip_ws();
                if lx.peek() == Some(b'<') && lx.peek_at(1) == Some(b'<') {
                    lx.pos += 2;
                    trailer = Some(lx.parse_dict_body(0));
                }
                break;
            }
            let first: u32 = match std::str::from_utf8(kw).ok().and_then(|s| s.parse().ok()) {
                Some(v) => v,
                None => {
                    lx.pos = save;
                    break;
                }
            };
            lx.skip_ws();
            let count: u32 = match std::str::from_utf8(lx.read_keyword())
                .ok()
                .and_then(|s| s.parse().ok())
            {
                Some(v) => v,
                None => break,
            };
            // Cap absurd counts from corrupt headers.
            if count > 8_000_000 {
                break;
            }
            for i in 0..count {
                lx.skip_ws();
                let off_tok = lx.read_keyword();
                lx.skip_ws();
                let gen_tok = lx.read_keyword();
                lx.skip_ws();
                let kind = lx.read_keyword();
                let off: usize = match std::str::from_utf8(off_tok).ok().and_then(|s| s.parse().ok())
                {
                    Some(v) => v,
                    None => break,
                };
                let gen: u16 = std::str::from_utf8(gen_tok)
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                if kind == b"n" && off > 0 {
                    self.entries
                        .entry(first + i)
                        .or_insert(XrefEntry::Offset { offset: off, gen });
                }
            }
        }

        self.bytes = bytes;
        trailer
    }

    /// Xref stream (PDF 1.5+): the table itself is a compressed stream whose
    /// `/W` array gives the field widths of each entry.
    fn parse_xref_stream(&mut self, offset: usize) -> Option<Dict> {
        let obj = self.parse_object_at(offset, None)?;
        let stm = obj.as_stream()?.clone();
        let dict = stm.dict.clone();
        let data = self.stream_data_of(&stm)?;

        let w: Vec<usize> = dict
            .get("W")?
            .as_array()?
            .iter()
            .map(|o| o.as_usize().unwrap_or(0))
            .collect();
        if w.len() < 3 {
            return None;
        }
        let size = dict.get("Size").and_then(|o| o.as_i64()).unwrap_or(0);
        let index: Vec<i64> = match dict.get("Index").and_then(|o| o.as_array()) {
            Some(a) => a.iter().filter_map(|o| o.as_i64()).collect(),
            None => vec![0, size],
        };

        let entry_len: usize = w.iter().sum();
        if entry_len == 0 {
            return None;
        }
        let mut pos = 0usize;
        let mut pair = index.chunks(2);
        while let Some(chunk) = pair.next() {
            if chunk.len() < 2 {
                break;
            }
            let (first, count) = (chunk[0], chunk[1]);
            if first < 0 || count < 0 {
                break;
            }
            for i in 0..count as u32 {
                if pos + entry_len > data.len() {
                    break;
                }
                let mut fields = [0u64; 3];
                let mut p = pos;
                for (fi, &width) in w.iter().enumerate().take(3) {
                    let mut v: u64 = 0;
                    for _ in 0..width {
                        v = (v << 8) | data[p] as u64;
                        p += 1;
                    }
                    fields[fi] = v;
                }
                pos += entry_len;
                // A zero-width type field means type 1 (in-use).
                let kind = if w[0] == 0 { 1 } else { fields[0] };
                let num = first as u32 + i;
                match kind {
                    1 => {
                        if fields[1] > 0 {
                            self.entries.entry(num).or_insert(XrefEntry::Offset {
                                offset: fields[1] as usize,
                                gen: fields[2] as u16,
                            });
                        }
                    }
                    2 => {
                        self.entries.entry(num).or_insert(XrefEntry::InStream {
                            stm: fields[1] as u32,
                            idx: fields[2] as u32,
                        });
                    }
                    // type 0 = free
                    _ => {}
                }
            }
        }
        Some(dict)
    }

    /// Scans the whole file for `N G obj` headers. Used when the xref chain is
    /// missing or wrong — far more common than the spec would suggest.
    pub fn recover(&mut self) {
        self.recovered = true;
        self.cache.borrow_mut().clear();
        self.objstm_cache.borrow_mut().clear();

        let bytes = std::mem::take(&mut self.bytes);
        let n = bytes.len();
        let mut found: HashMap<u32, XrefEntry> = HashMap::new();
        let mut i = 0usize;

        while i + 3 < n {
            if bytes[i] == b'o' && bytes[i + 1] == b'b' && bytes[i + 2] == b'j' {
                // Walk backwards over `N G ` to find the header start.
                let mut j = i;
                // whitespace before `obj`
                while j > 0 && is_ws(bytes[j - 1]) {
                    j -= 1;
                }
                let gen_end = j;
                while j > 0 && bytes[j - 1].is_ascii_digit() {
                    j -= 1;
                }
                let gen_start = j;
                while j > 0 && is_ws(bytes[j - 1]) {
                    j -= 1;
                }
                let num_end = j;
                while j > 0 && bytes[j - 1].is_ascii_digit() {
                    j -= 1;
                }
                let num_start = j;
                if num_start < num_end && gen_start < gen_end {
                    let num = std::str::from_utf8(&bytes[num_start..num_end])
                        .ok()
                        .and_then(|s| s.parse::<u32>().ok());
                    let gen = std::str::from_utf8(&bytes[gen_start..gen_end])
                        .ok()
                        .and_then(|s| s.parse::<u16>().ok())
                        .unwrap_or(0);
                    if let Some(num) = num {
                        // Later definitions win: incremental updates append.
                        found.insert(num, XrefEntry::Offset { offset: num_start, gen });
                    }
                }
                i += 3;
                continue;
            }
            i += 1;
        }

        self.bytes = bytes;
        for (k, v) in found {
            self.entries.insert(k, v);
        }

        // Rebuild the trailer if it is missing or has no usable /Root.
        if self.catalog_ref().is_none() {
            if let Some(t) = self.find_trailer_dict() {
                for (k, v) in t {
                    self.trailer.insert(k, v);
                }
            }
        }
        if self.catalog_ref().is_none() {
            // Last resort: find an object whose /Type is /Catalog.
            let nums: Vec<u32> = self.entries.keys().copied().collect();
            for num in nums {
                let obj = self.get_object(num);
                let is_catalog = obj.get("Type").and_then(|o| o.as_name()) == Some("Catalog");
                if is_catalog {
                    self.trailer.insert("Root".to_string(), Obj::Ref(num, 0));
                    break;
                }
            }
        }
        // Object streams are invisible to the header scan; unpack any we find.
        self.absorb_object_streams();
    }

    /// After a recovery scan, register the contents of every `/Type /ObjStm`
    /// so packed objects become reachable.
    fn absorb_object_streams(&mut self) {
        let nums: Vec<u32> = self.entries.keys().copied().collect();
        let mut extra: Vec<(u32, XrefEntry)> = Vec::new();
        for num in nums {
            let is_objstm = matches!(self.entries.get(&num), Some(XrefEntry::Offset { .. }))
                && self
                    .get_object(num)
                    .get("Type")
                    .and_then(|o| o.as_name())
                    == Some("ObjStm");
            if !is_objstm {
                continue;
            }
            if let Some(stm) = self.load_objstm(num) {
                for (idx, (onum, _)) in stm.pairs.iter().enumerate() {
                    if !self.entries.contains_key(onum) {
                        extra.push((*onum, XrefEntry::InStream { stm: num, idx: idx as u32 }));
                    }
                }
            }
        }
        for (num, e) in extra {
            self.entries.insert(num, e);
        }
    }

    fn find_trailer_dict(&self) -> Option<Dict> {
        let idx = rfind(&self.bytes, b"trailer")?;
        let mut lx = Lexer::at(&self.bytes, idx + b"trailer".len());
        lx.skip_ws();
        if lx.peek() == Some(b'<') && lx.peek_at(1) == Some(b'<') {
            lx.pos += 2;
            return Some(lx.parse_dict_body(0));
        }
        None
    }

    // ── object access ────────────────────────────────────────────────────────

    pub fn catalog_ref(&self) -> Option<Obj> {
        let root = self.trailer.get("Root")?;
        let cat = self.resolve(root);
        if cat.as_dict().is_some() {
            Some(cat)
        } else {
            None
        }
    }

    /// Follows indirect references until a direct object is reached.
    pub fn resolve(&self, obj: &Obj) -> Obj {
        let mut cur = obj.clone();
        for _ in 0..64 {
            match cur {
                Obj::Ref(num, _) => cur = self.get_object(num),
                other => return other,
            }
        }
        Obj::Null
    }

    /// Dictionary lookup that resolves the value.
    pub fn dget(&self, dict: &Dict, key: &str) -> Option<Obj> {
        let v = dict.get(key)?;
        let r = self.resolve(v);
        if r.is_null() {
            None
        } else {
            Some(r)
        }
    }

    /// Same as `dget` but reads from any object that has a dictionary view.
    pub fn oget(&self, obj: &Obj, key: &str) -> Option<Obj> {
        let d = obj.as_dict()?;
        self.dget(d, key)
    }

    pub fn get_object(&self, num: u32) -> Obj {
        if let Some(o) = self.cache.borrow().get(&num) {
            return o.clone();
        }
        if !self.in_progress.borrow_mut().insert(num) {
            // Cycle (e.g. /Length pointing at its own object).
            return Obj::Null;
        }
        let entry = self.entries.get(&num).copied();
        let obj = match entry {
            Some(XrefEntry::Offset { offset, .. }) => self
                .parse_object_at(offset, Some(num))
                .unwrap_or(Obj::Null),
            Some(XrefEntry::InStream { stm, idx }) => self.get_from_objstm(stm, idx, num),
            None => Obj::Null,
        };
        let obj = match &self.decryptor {
            Some(d) => decrypt_obj(obj, num, self.gen_of(num), d.as_ref(), &self.entries),
            None => obj,
        };
        self.in_progress.borrow_mut().remove(&num);
        self.cache.borrow_mut().insert(num, obj.clone());
        obj
    }

    fn gen_of(&self, num: u32) -> u16 {
        match self.entries.get(&num) {
            Some(XrefEntry::Offset { gen, .. }) => *gen,
            _ => 0,
        }
    }

    /// Parses `N G obj … endobj` at `offset`. When `expect` is given and the
    /// header names a different object, the offset is treated as stale.
    fn parse_object_at(&self, offset: usize, expect: Option<u32>) -> Option<Obj> {
        if offset >= self.bytes.len() {
            return None;
        }
        let mut lx = Lexer::at(&self.bytes, offset);
        lx.skip_ws();
        let num: u32 = std::str::from_utf8(lx.read_keyword()).ok()?.parse().ok()?;
        lx.skip_ws();
        let gen: u16 = std::str::from_utf8(lx.read_keyword())
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        lx.skip_ws();
        if lx.read_keyword() != b"obj" {
            return None;
        }
        if let Some(e) = expect {
            if e != num {
                return None;
            }
        }

        let value = lx.parse_obj()?;

        // A dictionary followed by `stream` is a stream object.
        let save = lx.pos;
        lx.skip_ws();
        if lx.read_keyword() == b"stream" {
            if let Obj::Dict(d) = &value {
                if let Some(stm) = self.read_stream_body(&mut lx, d.as_ref().clone(), num, gen) {
                    return Some(Obj::Stream(Rc::new(stm)));
                }
            }
        }
        lx.pos = save;
        Some(value)
    }

    /// Reads the bytes between `stream` and `endstream`. Trusts `/Length` only
    /// when `endstream` actually follows; otherwise scans for the keyword.
    fn read_stream_body(&self, lx: &mut Lexer, dict: Dict, num: u32, gen: u16) -> Option<Stream> {
        // The spec requires CRLF or LF after `stream`.
        if lx.peek() == Some(b'\r') {
            lx.pos += 1;
        }
        if lx.peek() == Some(b'\n') {
            lx.pos += 1;
        }
        let start = lx.pos;

        let declared = dict.get("Length").and_then(|o| match o {
            Obj::Int(v) if *v >= 0 => Some(*v as usize),
            Obj::Ref(n, _) => self.get_object(*n).as_usize(),
            _ => None,
        });

        let end = match declared {
            Some(len) if start + len <= self.bytes.len() && follows_endstream(&self.bytes, start + len) => {
                start + len
            }
            _ => match find(&self.bytes[start..], b"endstream") {
                Some(rel) => {
                    // Trim the EOL that precedes `endstream`.
                    let mut e = start + rel;
                    if e > start && self.bytes[e - 1] == b'\n' {
                        e -= 1;
                    }
                    if e > start && self.bytes[e - 1] == b'\r' {
                        e -= 1;
                    }
                    e
                }
                None => self.bytes.len(),
            },
        };

        let raw = self.bytes.get(start..end)?.to_vec();
        lx.pos = end;
        Some(Stream { dict, raw, obj_num: num, obj_gen: gen })
    }

    // ── object streams ───────────────────────────────────────────────────────

    fn load_objstm(&self, stm_num: u32) -> Option<Rc<ObjStmData>> {
        if let Some(s) = self.objstm_cache.borrow().get(&stm_num) {
            return Some(s.clone());
        }
        let obj = self.get_object(stm_num);
        let stm = obj.as_stream()?;
        let data = self.stream_data_of(stm)?;
        let n = self.dget(&stm.dict, "N").and_then(|o| o.as_usize()).unwrap_or(0);
        let first = self.dget(&stm.dict, "First").and_then(|o| o.as_usize()).unwrap_or(0);

        let mut pairs = Vec::with_capacity(n);
        let mut lx = Lexer::new(&data);
        for _ in 0..n {
            lx.skip_ws();
            let num: u32 = match std::str::from_utf8(lx.read_keyword()).ok().and_then(|s| s.parse().ok()) {
                Some(v) => v,
                None => break,
            };
            lx.skip_ws();
            let off: usize = match std::str::from_utf8(lx.read_keyword()).ok().and_then(|s| s.parse().ok()) {
                Some(v) => v,
                None => break,
            };
            pairs.push((num, first + off));
        }
        let rc = Rc::new(ObjStmData { data, pairs });
        self.objstm_cache.borrow_mut().insert(stm_num, rc.clone());
        Some(rc)
    }

    fn get_from_objstm(&self, stm_num: u32, idx: u32, want: u32) -> Obj {
        let stm = match self.load_objstm(stm_num) {
            Some(s) => s,
            None => return Obj::Null,
        };
        // Prefer the recorded index, but fall back to a search by number:
        // some producers write indices that do not match the pair table.
        let entry = stm
            .pairs
            .get(idx as usize)
            .filter(|(n, _)| *n == want)
            .or_else(|| stm.pairs.iter().find(|(n, _)| *n == want));
        let (_, off) = match entry {
            Some(e) => *e,
            None => return Obj::Null,
        };
        if off >= stm.data.len() {
            return Obj::Null;
        }
        Lexer::at(&stm.data, off).parse_obj().unwrap_or(Obj::Null)
    }

    // ── stream data ──────────────────────────────────────────────────────────

    /// Fully decoded stream contents. Returns `None` when the stream ends in an
    /// image filter (use `stream_data_raw` plus the image decoder for those).
    pub fn stream_data_of(&self, stm: &Stream) -> Option<Vec<u8>> {
        let d = self.decode_stream(stm);
        if d.image_filter.is_some() {
            None
        } else {
            Some(d.data)
        }
    }

    /// Runs the filter chain, decrypting first when the file is encrypted.
    pub fn decode_stream(&self, stm: &Stream) -> filter::Decoded {
        let raw = match &self.decryptor {
            // XRef streams and streams with an /Identity crypt filter are never
            // encrypted; the caller checks /Type before we get here.
            Some(d) if !is_unencrypted_stream(&stm.dict) => {
                d.decrypt_stream(stm.obj_num, stm.obj_gen, &stm.raw)
            }
            _ => stm.raw.clone(),
        };
        let names = filter::filter_names(stm.dict.get("Filter").map(|o| self.resolve(o)).as_ref());
        let parms = self.decode_parms(&stm.dict, names.len());
        filter::decode_chain(raw, &names, &parms)
    }

    fn decode_parms(&self, dict: &Dict, count: usize) -> Vec<Option<Dict>> {
        let parm = dict
            .get("DecodeParms")
            .or_else(|| dict.get("DP"))
            .map(|o| self.resolve(o));
        let mut out = vec![None; count.max(1)];
        match parm {
            Some(Obj::Dict(d)) => {
                if !out.is_empty() {
                    out[0] = Some(d.as_ref().clone());
                }
            }
            Some(Obj::Array(a)) => {
                for (i, item) in a.iter().enumerate() {
                    if i >= out.len() {
                        break;
                    }
                    if let Obj::Dict(d) = self.resolve(item) {
                        out[i] = Some(d.as_ref().clone());
                    }
                }
            }
            _ => {}
        }
        out
    }
}

/// `/Filter` values that mean the bytes are already plaintext.
fn is_unencrypted_stream(dict: &Dict) -> bool {
    if dict.get("Type").and_then(|o| o.as_name()) == Some("XRef") {
        return true;
    }
    false
}

fn follows_endstream(bytes: &[u8], mut pos: usize) -> bool {
    if pos > bytes.len() {
        return false;
    }
    let mut budget = 4usize; // allow a stray EOL or two
    while pos < bytes.len() && is_ws(bytes[pos]) && budget > 0 {
        pos += 1;
        budget -= 1;
    }
    bytes[pos..].starts_with(b"endstream")
}

/// Recursively decrypts every string inside a freshly parsed indirect object.
fn decrypt_obj(
    obj: Obj,
    num: u32,
    gen: u16,
    dec: &dyn Decryptor,
    _entries: &HashMap<u32, XrefEntry>,
) -> Obj {
    match obj {
        Obj::Str(s) => Obj::Str(Rc::new(dec.decrypt_string(num, gen, &s))),
        Obj::Array(a) => Obj::Array(Rc::new(
            a.iter()
                .map(|o| decrypt_obj(o.clone(), num, gen, dec, _entries))
                .collect(),
        )),
        Obj::Dict(d) => Obj::Dict(Rc::new(
            d.iter()
                .map(|(k, v)| (k.clone(), decrypt_obj(v.clone(), num, gen, dec, _entries)))
                .collect(),
        )),
        // Stream bytes are decrypted in `decode_stream`; only its dictionary
        // strings are handled here.
        Obj::Stream(s) => {
            let dict = s
                .dict
                .iter()
                .map(|(k, v)| (k.clone(), decrypt_obj(v.clone(), num, gen, dec, _entries)))
                .collect();
            Obj::Stream(Rc::new(Stream {
                dict,
                raw: s.raw.clone(),
                obj_num: s.obj_num,
                obj_gen: s.obj_gen,
            }))
        }
        other => other,
    }
}

// ── byte search helpers ──────────────────────────────────────────────────────

pub fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

pub fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).rposition(|w| w == needle)
}
