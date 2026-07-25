//! PDF object model (ISO 32000-1 clause 7.3).
//!
//! Composite payloads are behind `Rc` so cloning an `Obj` is cheap: the
//! resolver hands out clones constantly while walking dictionaries.

use std::collections::HashMap;
use std::rc::Rc;

pub type Dict = HashMap<String, Obj>;

/// A raw (still encoded) stream: its dictionary plus the bytes between
/// `stream` and `endstream`.
#[derive(Clone, Debug)]
pub struct Stream {
    pub dict: Dict,
    pub raw: Vec<u8>,
    /// Object number this stream came from; needed to derive the per-object
    /// decryption key.
    pub obj_num: u32,
    pub obj_gen: u16,
}

#[derive(Clone, Debug)]
pub enum Obj {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Str(Rc<Vec<u8>>),
    Name(Rc<String>),
    Array(Rc<Vec<Obj>>),
    Dict(Rc<Dict>),
    Stream(Rc<Stream>),
    /// Indirect reference: object number, generation.
    Ref(u32, u16),
}

impl Obj {
    pub fn name(s: &str) -> Obj {
        Obj::Name(Rc::new(s.to_string()))
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Obj::Null)
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Obj::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Obj::Int(i) => Some(*i),
            Obj::Real(r) => Some(*r as i64),
            _ => None,
        }
    }

    pub fn as_usize(&self) -> Option<usize> {
        match self.as_i64() {
            Some(i) if i >= 0 => Some(i as usize),
            _ => None,
        }
    }

    pub fn as_f32(&self) -> Option<f32> {
        self.as_f64().map(|v| v as f32)
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Obj::Int(i) => Some(*i as f64),
            Obj::Real(r) => Some(*r),
            _ => None,
        }
    }

    /// Name value without the leading slash.
    pub fn as_name(&self) -> Option<&str> {
        match self {
            Obj::Name(n) => Some(n.as_str()),
            _ => None,
        }
    }

    pub fn as_str_bytes(&self) -> Option<&[u8]> {
        match self {
            Obj::Str(s) => Some(s.as_slice()),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Obj]> {
        match self {
            Obj::Array(a) => Some(a.as_slice()),
            _ => None,
        }
    }

    /// Dictionary view. A stream also answers here: its dictionary is the
    /// natural thing to look attributes up in.
    pub fn as_dict(&self) -> Option<&Dict> {
        match self {
            Obj::Dict(d) => Some(d),
            Obj::Stream(s) => Some(&s.dict),
            _ => None,
        }
    }

    pub fn as_stream(&self) -> Option<&Stream> {
        match self {
            Obj::Stream(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_ref_id(&self) -> Option<(u32, u16)> {
        match self {
            Obj::Ref(n, g) => Some((*n, *g)),
            _ => None,
        }
    }

    /// Direct dictionary lookup, no indirect-reference resolution.
    /// Use `PdfFile::dget` when the value may be a reference.
    pub fn get(&self, key: &str) -> Option<&Obj> {
        self.as_dict().and_then(|d| d.get(key))
    }
}

/// Reads a rectangle written as `[x0 y0 x1 y1]`, normalized so that
/// `x0 <= x1` and `y0 <= y1` (PDF allows the corners in any order).
pub fn rect_from(obj: &Obj) -> Option<[f64; 4]> {
    let a = obj.as_array()?;
    if a.len() < 4 {
        return None;
    }
    let x0 = a[0].as_f64()?;
    let y0 = a[1].as_f64()?;
    let x1 = a[2].as_f64()?;
    let y1 = a[3].as_f64()?;
    Some([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)])
}
