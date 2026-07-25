//! DrawingML parsing shared by slides, layouts, masters, tables and diagrams:
//! colors, fills, outlines and text bodies, over the `xmltree::Node` DOM.
//! Copyright (c) 2026 Anicca Code Studio. MIT licensed.

use super::model::*;
use super::theme::{parse_hex, Theme};
use super::xmltree::Node;

/// A color reference plus resolved alpha (0.0..=1.0).
#[derive(Clone, Copy, Debug)]
pub struct Rgba {
    pub rgb: [u8; 3],
    pub alpha: f32,
}

/// Resolve a color-choice element (`a:srgbClr`, `a:schemeClr`, `a:sysClr`,
/// `a:prstClr`, `a:scrgbClr`) with its modifier children. `ph` is the
/// placeholder color used when a nested `schemeClr val="phClr"` appears.
pub fn resolve_color(node: &Node, theme: &Theme, ph: Option<[u8; 3]>) -> Option<Rgba> {
    let base = match node.name.as_str() {
        "srgbClr" => parse_hex(node.attr("val")?)?,
        "sysClr" => node
            .attr("lastClr")
            .and_then(parse_hex)
            .unwrap_or([0, 0, 0]),
        "schemeClr" => {
            let v = node.attr("val")?;
            if v == "phClr" {
                ph.unwrap_or([0, 0, 0])
            } else {
                theme.scheme_color(v).unwrap_or([0, 0, 0])
            }
        }
        "prstClr" => preset_color(node.attr("val")?),
        "scrgbClr" => {
            // Percentages 0..100000.
            let r = pct(node.attr("r")) * 255.0;
            let g = pct(node.attr("g")) * 255.0;
            let b = pct(node.attr("b")) * 255.0;
            [r as u8, g as u8, b as u8]
        }
        _ => return None,
    };
    let mut hsl = rgb_to_hsl(base);
    let mut alpha = 1.0f32;
    for m in &node.children {
        let v = m.attr("val");
        match m.name.as_str() {
            "alpha" => alpha = pct(v),
            "lumMod" => hsl.2 *= pct(v),
            "lumOff" => hsl.2 += pct(v),
            "shade" => {
                // Multiply toward black in linear-ish space (approx).
                let f = pct(v);
                hsl.2 *= f;
            }
            "tint" => {
                // Blend toward white.
                let f = pct(v);
                hsl.2 = hsl.2 * f + (1.0 - f);
            }
            "satMod" => hsl.1 = (hsl.1 * pct(v)).min(1.0),
            "satOff" => hsl.1 = (hsl.1 + pct(v)).clamp(0.0, 1.0),
            "hueMod" => hsl.0 = (hsl.0 * pct(v)) % 360.0,
            _ => {}
        }
    }
    hsl.2 = hsl.2.clamp(0.0, 1.0);
    Some(Rgba { rgb: hsl_to_rgb(hsl), alpha })
}

/// Find the first color-choice child of a container element and resolve it.
pub fn first_color(parent: &Node, theme: &Theme, ph: Option<[u8; 3]>) -> Option<Rgba> {
    for c in &parent.children {
        if is_color_elem(&c.name) {
            return resolve_color(c, theme, ph);
        }
    }
    None
}

fn is_color_elem(name: &str) -> bool {
    matches!(name, "srgbClr" | "schemeClr" | "sysClr" | "prstClr" | "scrgbClr")
}

/// Parse a fill from a properties container (`spPr`, `tcPr`, `bg`, `grpSpPr`).
/// Recognizes `a:noFill`, `a:solidFill`, `a:gradFill`, `a:blipFill`.
/// `ph` supplies the placeholder color. Returns None if no fill element present.
pub fn parse_fill(container: &Node, theme: &Theme, ph: Option<[u8; 3]>) -> Option<Fill> {
    for c in &container.children {
        match c.name.as_str() {
            "noFill" => return Some(Fill::None),
            "solidFill" => {
                if let Some(rgba) = first_color(c, theme, ph) {
                    return Some(Fill::Solid { color: rgba.rgb, alpha: rgba.alpha });
                }
                return Some(Fill::None);
            }
            "gradFill" => return Some(parse_grad_fill(c, theme, ph)),
            "blipFill" => {
                // The image bytes are resolved by the caller (needs rels+zip);
                // here we only flag that a blip fill exists.
                return Some(Fill::Blip { rgba: None });
            }
            "pattFill" => {
                // Approximate a pattern by its foreground color.
                if let Some(fg) = c.child("fgClr").and_then(|n| first_color(n, theme, ph)) {
                    return Some(Fill::Solid { color: fg.rgb, alpha: fg.alpha });
                }
                return Some(Fill::None);
            }
            _ => {}
        }
    }
    None
}

fn parse_grad_fill(node: &Node, theme: &Theme, ph: Option<[u8; 3]>) -> Fill {
    let mut stops: Vec<GradStop> = Vec::new();
    if let Some(lst) = node.child("gsLst") {
        for gs in lst.children_named("gs") {
            let pos = gs.attr_f64("pos").unwrap_or(0.0) as f32 / 100000.0;
            if let Some(rgba) = first_color(gs, theme, ph) {
                stops.push(GradStop { pos, color: rgba.rgb, alpha: rgba.alpha });
            }
        }
    }
    stops.sort_by(|a, b| a.pos.partial_cmp(&b.pos).unwrap_or(std::cmp::Ordering::Equal));
    let radial = node.child("path").is_some();
    let angle_deg = node
        .child("lin")
        .and_then(|l| l.attr_f64("ang"))
        .map(|a| (a / 60000.0) as f32)
        .unwrap_or(0.0);
    if stops.is_empty() {
        return Fill::None;
    }
    Fill::Gradient { stops, angle_deg, radial }
}

/// Parse an outline (`a:ln`) into a `Line`.
pub fn parse_line(container: &Node, theme: &Theme, ph: Option<[u8; 3]>) -> Line {
    let ln = match container.child("ln") {
        Some(n) => n,
        None => return Line::default(),
    };
    let width_emu = ln.attr_i64("w").unwrap_or(0);
    // noFill outline → no stroke.
    if ln.child("noFill").is_some() {
        return Line { fill: Fill::None, width_emu, dash: DashKind::Solid };
    }
    let fill = ln
        .child("solidFill")
        .and_then(|sf| first_color(sf, theme, ph))
        .map(|r| Fill::Solid { color: r.rgb, alpha: r.alpha })
        .or_else(|| ln.child("gradFill").map(|g| parse_grad_fill(g, theme, ph)))
        .unwrap_or(Fill::None);
    let dash = match ln.child("prstDash").and_then(|d| d.attr("val")) {
        Some("dash") | Some("sysDash") | Some("lgDash") => DashKind::Dash,
        Some("dot") | Some("sysDot") => DashKind::Dot,
        Some("dashDot") | Some("lgDashDot") | Some("sysDashDot") => DashKind::DashDot,
        _ => DashKind::Solid,
    };
    Line { fill, width_emu, dash }
}

/// Parse geometry from a properties container (`a:prstGeom` / `a:custGeom`).
pub fn parse_geom(container: &Node) -> Geom {
    if let Some(pg) = container.child("prstGeom") {
        let name = pg.attr("prst").unwrap_or("rect").to_string();
        let mut adj = Vec::new();
        if let Some(lst) = pg.child("avLst") {
            for gd in lst.children_named("gd") {
                if let (Some(n), Some(f)) = (gd.attr("name"), gd.attr("fmla")) {
                    // fmla like "val 12345"
                    if let Some(v) = f.strip_prefix("val ").and_then(|s| s.trim().parse::<i64>().ok())
                    {
                        adj.push((n.to_string(), v));
                    }
                }
            }
        }
        return Geom::Preset { name, adj };
    }
    if let Some(cg) = container.child("custGeom") {
        return parse_cust_geom(cg);
    }
    Geom::None
}

fn parse_cust_geom(cg: &Node) -> Geom {
    let mut paths = Vec::new();
    if let Some(list) = cg.child("pathLst") {
        for p in list.children_named("path") {
            let w = p.attr_i64("w").unwrap_or(0);
            let h = p.attr_i64("h").unwrap_or(0);
            let fill = p.attr("fill").map(|v| v != "none").unwrap_or(true);
            let stroke = p.attr("stroke").map(|v| v != "false").unwrap_or(true);
            let mut cmds = Vec::new();
            for cmd in &p.children {
                match cmd.name.as_str() {
                    "moveTo" => {
                        if let Some((x, y)) = pt_child(cmd, "pt") {
                            cmds.push(PathCmd::Move(x, y));
                        }
                    }
                    "lnTo" => {
                        if let Some((x, y)) = pt_child(cmd, "pt") {
                            cmds.push(PathCmd::Line(x, y));
                        }
                    }
                    "cubicBezTo" => {
                        let pts: Vec<(i64, i64)> =
                            cmd.children_named("pt").filter_map(pt_of).collect();
                        if pts.len() == 3 {
                            cmds.push(PathCmd::Cubic(
                                pts[0].0, pts[0].1, pts[1].0, pts[1].1, pts[2].0, pts[2].1,
                            ));
                        }
                    }
                    "quadBezTo" => {
                        let pts: Vec<(i64, i64)> =
                            cmd.children_named("pt").filter_map(pt_of).collect();
                        if pts.len() == 2 {
                            // Elevate quadratic to cubic (control points at 2/3).
                            cmds.push(PathCmd::Cubic(
                                pts[0].0, pts[0].1, pts[0].0, pts[0].1, pts[1].0, pts[1].1,
                            ));
                        }
                    }
                    "arcTo" => {
                        let wr = cmd.attr_i64("wR").unwrap_or(0);
                        let hr = cmd.attr_i64("hR").unwrap_or(0);
                        let st = cmd.attr_i64("stAng").unwrap_or(0);
                        let sw = cmd.attr_i64("swAng").unwrap_or(0);
                        cmds.push(PathCmd::Arc(wr, hr, st, sw));
                    }
                    "close" => cmds.push(PathCmd::Close),
                    _ => {}
                }
            }
            paths.push(CustomPath { w, h, cmds, fill, stroke });
        }
    }
    if paths.is_empty() {
        Geom::None
    } else {
        Geom::Custom { paths }
    }
}

fn pt_of(n: &Node) -> Option<(i64, i64)> {
    Some((n.attr_i64("x")?, n.attr_i64("y")?))
}

fn pt_child(parent: &Node, name: &str) -> Option<(i64, i64)> {
    parent.child(name).and_then(pt_of)
}

// ── text body ──────────────────────────────────────────────────────────────────

/// Parse a `p:txBody` / `a:txBody`. `default_run` supplies the fallback run
/// style (color/size/font) resolved from placeholder + master list styles.
pub fn parse_text_body(txbody: &Node, theme: &Theme, default_run: &Run) -> TextBody {
    let mut tb = TextBody { font_scale: 1.0, wrap: true, ..Default::default() };
    tb.inset_l_pt = 7.2; // 0.1"
    tb.inset_r_pt = 7.2;
    tb.inset_t_pt = 3.6; // 0.05"
    tb.inset_b_pt = 3.6;

    if let Some(body_pr) = txbody.child("bodyPr") {
        if let Some(a) = body_pr.attr("anchor") {
            tb.anchor = match a {
                "ctr" => Anchor::Center,
                "b" => Anchor::Bottom,
                _ => Anchor::Top,
            };
        }
        if let Some(v) = body_pr.attr("lIns") {
            tb.inset_l_pt = emu_to_pt(v);
        }
        if let Some(v) = body_pr.attr("rIns") {
            tb.inset_r_pt = emu_to_pt(v);
        }
        if let Some(v) = body_pr.attr("tIns") {
            tb.inset_t_pt = emu_to_pt(v);
        }
        if let Some(v) = body_pr.attr("bIns") {
            tb.inset_b_pt = emu_to_pt(v);
        }
        if let Some(w) = body_pr.attr("wrap") {
            tb.wrap = w != "none";
        }
        if let Some(af) = body_pr.child("normAutofit") {
            if let Some(fs) = af.attr_f64("fontScale") {
                tb.font_scale = (fs / 100000.0) as f32;
            }
        }
    }

    for p in txbody.children_named("p") {
        tb.paragraphs.push(parse_paragraph(p, theme, default_run));
    }
    tb
}

fn parse_paragraph(p: &Node, theme: &Theme, default_run: &Run) -> Para {
    let mut para = Para {
        default_size_pt: default_run.size_pt,
        line_pct: Some(1.0),
        ..Default::default()
    };

    let ppr = p.child("pPr");
    if let Some(pp) = ppr {
        para.level = pp.attr_i64("lvl").unwrap_or(0).clamp(0, 8) as u8;
        if let Some(al) = pp.attr("algn") {
            para.align = match al {
                "ctr" => TextAlign::Center,
                "r" => TextAlign::Right,
                "just" => TextAlign::Justify,
                _ => TextAlign::Left,
            };
        }
        if let Some(v) = pp.attr("marL") {
            para.margin_left_pt = emu_to_pt(v);
        }
        if let Some(v) = pp.attr("indent") {
            para.indent_pt = emu_to_pt(v);
        }
        // Spacing.
        if let Some(sb) = pp.child("spcBef").and_then(spacing_pts) {
            para.space_before_pt = sb;
        }
        if let Some(sa) = pp.child("spcAft").and_then(spacing_pts) {
            para.space_after_pt = sa;
        }
        if let Some(ln) = pp.child("lnSpc") {
            if let Some(pc) = ln.child("spcPct").and_then(|n| n.attr_f64("val")) {
                para.line_pct = Some((pc / 100000.0) as f32);
            } else if let Some(pts) = ln.child("spcPts").and_then(|n| n.attr_f64("val")) {
                para.line_exact_pt = Some((pts / 100.0) as f32);
                para.line_pct = None;
            }
        }
        // Bullet.
        parse_bullet(pp, &mut para, theme, default_run);
    }

    // Runs.
    for child in &p.children {
        match child.name.as_str() {
            "r" => {
                para.runs.push(parse_run(child, theme, default_run));
            }
            "br" => {
                // Explicit line break → empty run carrying a newline.
                let mut r = default_run.clone();
                r.text = "\n".to_string();
                para.runs.push(r);
            }
            "fld" => {
                // Field (slide number, etc.): use its cached text.
                let mut r = parse_run(child, theme, default_run);
                if r.text.is_empty() {
                    if let Some(t) = child.child("t") {
                        r.text = t.text_content();
                    }
                }
                if !r.text.is_empty() {
                    para.runs.push(r);
                }
            }
            _ => {}
        }
    }

    // Trailing paragraph mark run properties can set the empty-line height.
    if para.runs.is_empty() {
        if let Some(end) = p.child("endParaRPr") {
            if let Some(sz) = end.attr_f64("sz") {
                para.default_size_pt = (sz / 100.0) as f32;
            }
        }
    }
    para
}

fn parse_run(r: &Node, theme: &Theme, default_run: &Run) -> Run {
    let mut run = default_run.clone();
    run.text = r.child("t").map(|t| t.text_content()).unwrap_or_default();
    if let Some(rpr) = r.child("rPr") {
        apply_run_props(rpr, &mut run, theme);
    }
    run
}

/// Apply `a:rPr`/`a:defRPr`/`a:endParaRPr` character properties onto a run.
pub fn apply_run_props(rpr: &Node, run: &mut Run, theme: &Theme) {
    if let Some(sz) = rpr.attr_f64("sz") {
        run.size_pt = (sz / 100.0) as f32;
    }
    if let Some(b) = rpr.attr("b") {
        run.bold = b == "1" || b == "true";
    }
    if let Some(i) = rpr.attr("i") {
        run.italic = i == "1" || i == "true";
    }
    if let Some(u) = rpr.attr("u") {
        run.underline = u != "none";
    }
    if let Some(s) = rpr.attr("strike") {
        run.strike = s != "noStrike";
    }
    if let Some(sf) = rpr.child("solidFill") {
        if let Some(rgba) = first_color(sf, theme, None) {
            run.color = rgba.rgb;
        }
    }
    if let Some(latin) = rpr.child("latin").and_then(|n| n.attr("typeface")) {
        run.font = resolve_theme_font(latin, theme);
    }
    if let Some(ea) = rpr.child("ea").and_then(|n| n.attr("typeface")) {
        run.font_ea = resolve_theme_font(ea, theme);
    }
}

/// Map theme font tokens (`+mj-lt`, `+mn-lt`) to concrete family names.
pub fn resolve_theme_font(tf: &str, theme: &Theme) -> Option<String> {
    let name = match tf {
        "+mj-lt" | "+mj-ea" | "+mj-cs" => theme.major_latin.clone(),
        "+mn-lt" | "+mn-ea" | "+mn-cs" => theme.minor_latin.clone(),
        other if other.is_empty() => None,
        other => Some(other.to_string()),
    };
    name.filter(|s| !s.is_empty())
}

fn parse_bullet(pp: &Node, para: &mut Para, theme: &Theme, _default_run: &Run) {
    if pp.child("buNone").is_some() {
        para.bullet = None;
        return;
    }
    if let Some(clr) = pp.child("buClr").and_then(|n| first_color(n, theme, None)) {
        para.bullet_color = Some(clr.rgb);
    }
    if let Some(ch) = pp.child("buChar").and_then(|n| n.attr("char")) {
        para.bullet = Some(ch.to_string());
    } else if pp.child("buAutoNum").is_some() {
        // Auto-numbered; concrete number is computed at render time. Placeholder.
        para.bullet = Some("•".to_string());
    }
}

// ── numeric helpers ─────────────────────────────────────────────────────────────

fn emu_to_pt(v: &str) -> f32 {
    v.trim().parse::<f64>().map(|e| (e / EMU_PER_PT) as f32).unwrap_or(0.0)
}

fn spacing_pts(node: &Node) -> Option<f32> {
    if let Some(p) = node.child("spcPts").and_then(|n| n.attr_f64("val")) {
        return Some((p / 100.0) as f32);
    }
    // spcPct spacing is relative to line height; approximate as points of an
    // 18pt line for the "before/after" case when only a percent is given.
    if let Some(p) = node.child("spcPct").and_then(|n| n.attr_f64("val")) {
        return Some((p / 100000.0 * 18.0) as f32);
    }
    None
}

fn pct(v: Option<&str>) -> f32 {
    v.and_then(|s| s.trim().parse::<f64>().ok())
        .map(|x| (x / 100000.0) as f32)
        .unwrap_or(0.0)
}

// ── color space conversions ─────────────────────────────────────────────────────

fn rgb_to_hsl(rgb: [u8; 3]) -> (f32, f32, f32) {
    let r = rgb[0] as f32 / 255.0;
    let g = rgb[1] as f32 / 255.0;
    let b = rgb[2] as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < 1e-6 {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        ((g - b) / d + if g < b { 6.0 } else { 0.0 }) * 60.0
    } else if max == g {
        ((b - r) / d + 2.0) * 60.0
    } else {
        ((r - g) / d + 4.0) * 60.0
    };
    (h, s, l)
}

fn hsl_to_rgb(hsl: (f32, f32, f32)) -> [u8; 3] {
    let (h, s, l) = hsl;
    if s.abs() < 1e-6 {
        let v = (l * 255.0).round().clamp(0.0, 255.0) as u8;
        return [v, v, v];
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let hk = h / 360.0;
    let r = hue_to_rgb(p, q, hk + 1.0 / 3.0);
    let g = hue_to_rgb(p, q, hk);
    let b = hue_to_rgb(p, q, hk - 1.0 / 3.0);
    [
        (r * 255.0).round().clamp(0.0, 255.0) as u8,
        (g * 255.0).round().clamp(0.0, 255.0) as u8,
        (b * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        p + (q - p) * 6.0 * t
    } else if t < 1.0 / 2.0 {
        q
    } else if t < 2.0 / 3.0 {
        p + (q - p) * (2.0 / 3.0 - t) * 6.0
    } else {
        p
    }
}

/// A subset of DrawingML preset color names.
fn preset_color(name: &str) -> [u8; 3] {
    match name {
        "black" => [0, 0, 0],
        "white" => [255, 255, 255],
        "red" => [255, 0, 0],
        "green" => [0, 128, 0],
        "blue" => [0, 0, 255],
        "yellow" => [255, 255, 0],
        "cyan" => [0, 255, 255],
        "magenta" => [255, 0, 255],
        "gray" | "grey" => [128, 128, 128],
        "darkGray" | "dkGray" => [169, 169, 169],
        "lightGray" | "ltGray" => [211, 211, 211],
        "orange" => [255, 165, 0],
        "purple" => [128, 0, 128],
        "brown" => [165, 42, 42],
        "pink" => [255, 192, 203],
        _ => [0, 0, 0],
    }
}
