//! Theme parsing: color scheme (`a:clrScheme`) and font scheme (`a:fontScheme`).
//! Copyright (c) 2026 Anicca Code Studio. MIT licensed.

use std::collections::HashMap;

use quick_xml::events::Event;
use quick_xml::Reader;

use super::{attr, local, read_zip_text, zip_has, Zip};

#[derive(Clone, Debug, Default)]
pub struct Theme {
    /// Scheme name (dk1, lt1, dk2, lt2, accent1..6, hlink, folHlink) → RGB.
    pub colors: HashMap<String, [u8; 3]>,
    pub major_latin: Option<String>,
    pub minor_latin: Option<String>,
    /// clrMap from the slide master: maps p:clrMap logical names (bg1, tx1, …)
    /// to scheme names (lt1, dk1, …).
    pub clr_map: HashMap<String, String>,
}

impl Theme {
    /// Resolve a scheme color name (as used by `a:schemeClr val=`) to RGB.
    /// Handles clrMap indirection (bg1/tx1/bg2/tx2 → scheme slot).
    pub fn scheme_color(&self, name: &str) -> Option<[u8; 3]> {
        let mut key = name.to_string();
        // Map logical → scheme via clrMap when needed.
        if let Some(mapped) = self.clr_map.get(&key) {
            key = mapped.clone();
        }
        // "phClr" is resolved by the caller (placeholder color); not here.
        if let Some(c) = self.colors.get(&key) {
            return Some(*c);
        }
        // Common aliases.
        let alias = match key.as_str() {
            "bg1" | "lt1" | "background1" => "lt1",
            "tx1" | "dk1" | "text1" => "dk1",
            "bg2" | "lt2" | "background2" => "lt2",
            "tx2" | "dk2" | "text2" => "dk2",
            "hlink" | "hyperlink" => "hlink",
            _ => return None,
        };
        self.colors.get(alias).copied()
    }
}

/// Load the first theme part found in the archive (theme1 preferred).
pub fn load_first_theme(zip: &mut Zip) -> Theme {
    // Prefer theme1; else scan.
    let mut path = None;
    if zip_has(zip, "ppt/theme/theme1.xml") {
        path = Some("ppt/theme/theme1.xml".to_string());
    } else {
        for i in 0..zip.len() {
            if let Ok(f) = zip.by_index(i) {
                let n = f.name();
                if n.starts_with("ppt/theme/theme") && n.ends_with(".xml") {
                    path = Some(n.to_string());
                    break;
                }
            }
        }
    }
    let mut theme = match path.and_then(|p| read_zip_text(zip, &p)) {
        Some(xml) => parse_theme(&xml),
        None => Theme::default(),
    };
    // Load clrMap from slide master 1 if present.
    if let Some(xml) = read_zip_text(zip, "ppt/slideMasters/slideMaster1.xml") {
        theme.clr_map = parse_clr_map(&xml);
    }
    theme
}

fn parse_theme(xml: &str) -> Theme {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut theme = Theme::default();

    // Parser state.
    let mut in_clr_scheme = false;
    let mut in_font_scheme = false;
    let mut font_slot: Option<&str> = None; // "major" | "minor"
    let mut cur_scheme_name: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let ln = local(e.name().as_ref()).to_vec();
                match ln.as_slice() {
                    b"clrScheme" => in_clr_scheme = true,
                    b"fontScheme" => in_font_scheme = true,
                    b"majorFont" if in_font_scheme => font_slot = Some("major"),
                    b"minorFont" if in_font_scheme => font_slot = Some("minor"),
                    name if in_clr_scheme => {
                        cur_scheme_name = Some(String::from_utf8_lossy(name).into_owned());
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                let ln = local(e.name().as_ref()).to_vec();
                match ln.as_slice() {
                    b"srgbClr" if in_clr_scheme => {
                        if let (Some(nm), Some(hex)) = (&cur_scheme_name, attr(&e, b"val")) {
                            if let Some(rgb) = parse_hex(&hex) {
                                theme.colors.insert(nm.clone(), rgb);
                            }
                        }
                    }
                    b"sysClr" if in_clr_scheme => {
                        // sysClr carries a lastClr fallback hex.
                        if let (Some(nm), Some(hex)) = (&cur_scheme_name, attr(&e, b"lastClr")) {
                            if let Some(rgb) = parse_hex(&hex) {
                                theme.colors.insert(nm.clone(), rgb);
                            }
                        }
                    }
                    b"latin" if in_font_scheme => {
                        if let Some(tf) = attr(&e, b"typeface") {
                            if !tf.is_empty() {
                                match font_slot {
                                    Some("major") => theme.major_latin = Some(tf),
                                    Some("minor") => theme.minor_latin = Some(tf),
                                    _ => {}
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                let ln = local(e.name().as_ref()).to_vec();
                match ln.as_slice() {
                    b"clrScheme" => in_clr_scheme = false,
                    b"fontScheme" => in_font_scheme = false,
                    b"majorFont" | b"minorFont" => font_slot = None,
                    name if in_clr_scheme => {
                        if Some(String::from_utf8_lossy(name).into_owned()) == cur_scheme_name {
                            cur_scheme_name = None;
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    theme
}

/// Parse `p:clrMap` element attributes from the slide master.
fn parse_clr_map(xml: &str) -> HashMap<String, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut map = HashMap::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                if local(e.name().as_ref()) == b"clrMap" {
                    for a in e.attributes().flatten() {
                        let key = String::from_utf8_lossy(a.key.local_name().as_ref()).into_owned();
                        let val = String::from_utf8_lossy(&a.value).into_owned();
                        map.insert(key, val);
                    }
                    break;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    map
}

pub fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    if h.len() < 6 {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some([r, g, b])
}
