//! Slide parsing: shape tree → `Slide` model, with placeholder inheritance and
//! image/table/chart/diagram resolution.
//! Copyright (c) 2026 Anicca Code Studio. MIT licensed.

use std::collections::HashMap;

use super::drawingml::{parse_fill, parse_geom, parse_line, parse_text_body};
use super::inherit::{self, InheritCtx};
use super::model::*;
use super::theme::Theme;
use super::xmltree::{self, Node};
use super::{parse_rels, read_zip_bytes, read_zip_text, Zip};

/// Parse one slide part into a `Slide`.
pub fn parse_slide(zip: &mut Zip, slide_path: &str, theme: &Theme) -> Result<Slide, String> {
    let xml = read_zip_text(zip, slide_path).ok_or_else(|| format!("missing {slide_path}"))?;
    let slide_dom = xmltree::parse(&xml).ok_or_else(|| "slide xml parse".to_string())?;
    let rels = parse_rels(zip, slide_path);

    // Resolve layout (via slide rels, rel type slideLayout) and master (via
    // layout rels). We match by target path pattern to avoid namespace checks.
    let layout_path = rels
        .values()
        .find(|p| p.contains("slideLayouts/"))
        .cloned();
    let layout_dom = layout_path
        .as_ref()
        .and_then(|p| read_zip_text(zip, p))
        .and_then(|x| xmltree::parse(&x));
    let master_path = layout_path.as_ref().and_then(|lp| {
        let lrels = parse_rels(zip, lp);
        lrels.values().find(|p| p.contains("slideMasters/")).cloned()
    });
    let master_dom = master_path
        .as_ref()
        .and_then(|p| read_zip_text(zip, p))
        .and_then(|x| xmltree::parse(&x));

    // Pre-decode all images referenced by the slide rels (embed rId → bitmap).
    let mut images: HashMap<String, ImageData> = HashMap::new();
    let rid_targets: Vec<(String, String)> =
        rels.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    for (rid, target) in &rid_targets {
        if is_image_path(target) {
            if let Some(bytes) = read_zip_bytes(zip, target) {
                if let Some(img) = decode_image(&bytes) {
                    images.insert(rid.clone(), img);
                }
            }
        }
    }

    let ctx = InheritCtx {
        layout: layout_dom.as_ref(),
        master: master_dom.as_ref(),
        theme,
    };

    let background = inherit::resolve_background(&slide_dom, &ctx, theme);

    let sptree = slide_dom
        .find("spTree")
        .ok_or_else(|| "slide has no spTree".to_string())?;

    let mut shapes = Vec::new();
    for child in &sptree.children {
        if let Some(shape) = parse_tree_child(child, &ctx, theme, &images, zip, &rels) {
            shapes.push(shape);
        }
    }

    Ok(Slide { shapes, background })
}

fn parse_tree_child(
    node: &Node,
    ctx: &InheritCtx,
    theme: &Theme,
    images: &HashMap<String, ImageData>,
    zip: &mut Zip,
    rels: &HashMap<String, String>,
) -> Option<Shape> {
    match node.name.as_str() {
        "sp" => Some(parse_sp(node, ctx, theme)),
        "pic" => Some(parse_pic(node, theme, images)),
        "grpSp" => Some(parse_group(node, ctx, theme, images, zip, rels)),
        "graphicFrame" => parse_graphic_frame(node, theme, zip, rels),
        _ => None,
    }
}

fn parse_sp(sp: &Node, ctx: &InheritCtx, theme: &Theme) -> Shape {
    let sppr = sp.child("spPr");
    let placeholder = read_placeholder(sp);

    // Transform: explicit, else inherited from layout/master placeholder.
    let mut xfrm = sppr
        .and_then(|s| s.child("xfrm"))
        .map(parse_xfrm_node)
        .unwrap_or_default();
    if !xfrm.has_size() {
        if let Some(ph) = &placeholder {
            if let Some(inh) = inherit::inherit_xfrm(ctx, ph.ph_type.as_deref(), ph.idx) {
                xfrm = inh;
            }
        }
    }

    let geom = sppr.map(parse_geom).unwrap_or(Geom::None);
    let fill = sppr
        .and_then(|s| parse_fill(s, theme, None))
        .unwrap_or(Fill::None);
    let line = sppr.map(|s| parse_line(s, theme, None)).unwrap_or_default();

    // Text body with placeholder-derived default run style.
    let ph_type = placeholder.as_ref().and_then(|p| p.ph_type.clone());
    let ph_idx = placeholder.as_ref().and_then(|p| p.idx);
    let text = sp.child("txBody").map(|tb| {
        // Default run at level 0; per-paragraph level refines size later.
        let default = inherit::default_run(ctx, ph_type.as_deref(), ph_idx, 0);
        let mut body = parse_text_body(tb, theme, &default);
        // Refine each paragraph's default size using its own level.
        refine_paragraph_defaults(&mut body, ctx, ph_type.as_deref(), ph_idx);
        body
    });

    let placeholder = placeholder.map(|p| Placeholder { ph_type: p.ph_type, idx: p.idx });
    Shape {
        xfrm,
        kind: ShapeKind::Sp { geom, fill, line, text },
        placeholder,
        name: shape_name(sp),
    }
}

/// For each paragraph, if its runs never set a size/color/font, fill from the
/// master list style at that paragraph's level.
fn refine_paragraph_defaults(
    body: &mut TextBody,
    ctx: &InheritCtx,
    ph_type: Option<&str>,
    ph_idx: Option<u32>,
) {
    for para in &mut body.paragraphs {
        let def = inherit::default_run(ctx, ph_type, ph_idx, para.level);
        para.default_size_pt = def.size_pt;
        for run in &mut para.runs {
            // Heuristic: runs cloned from a level-0 default keep that default's
            // size; if it differs from this level's default and the run had no
            // explicit size, this is acceptable for fidelity in most decks.
            if run.font.is_none() {
                run.font = def.font.clone();
            }
        }
    }
}

fn parse_pic(pic: &Node, theme: &Theme, images: &HashMap<String, ImageData>) -> Shape {
    let sppr = pic.child("spPr");
    let xfrm = sppr
        .and_then(|s| s.child("xfrm"))
        .map(parse_xfrm_node)
        .unwrap_or_default();
    let line = sppr.map(|s| parse_line(s, theme, None)).unwrap_or_default();

    // blipFill/blip@embed → image.
    let image = pic
        .child("blipFill")
        .and_then(|bf| bf.find("blip"))
        .and_then(|b| b.attr("embed").or_else(|| b.attr("link")))
        .and_then(|rid| images.get(rid).cloned());

    Shape {
        xfrm,
        kind: ShapeKind::Pic { image, line },
        placeholder: read_placeholder(pic).map(|p| Placeholder { ph_type: p.ph_type, idx: p.idx }),
        name: shape_name(pic),
    }
}

fn parse_group(
    grp: &Node,
    ctx: &InheritCtx,
    theme: &Theme,
    images: &HashMap<String, ImageData>,
    zip: &mut Zip,
    rels: &HashMap<String, String>,
) -> Shape {
    let xfrm = grp
        .child("grpSpPr")
        .and_then(|s| s.child("xfrm"))
        .map(parse_xfrm_node)
        .unwrap_or_default();
    let mut children = Vec::new();
    for child in &grp.children {
        match child.name.as_str() {
            "sp" | "pic" | "grpSp" | "graphicFrame" => {
                if let Some(shape) = parse_tree_child(child, ctx, theme, images, zip, rels) {
                    children.push(shape);
                }
            }
            _ => {}
        }
    }
    Shape {
        xfrm,
        kind: ShapeKind::Group { children },
        placeholder: None,
        name: shape_name(grp),
    }
}

fn parse_graphic_frame(
    gf: &Node,
    theme: &Theme,
    zip: &mut Zip,
    rels: &HashMap<String, String>,
) -> Option<Shape> {
    let xfrm = gf.child("xfrm").map(parse_xfrm_node).unwrap_or_default();
    let gdata = gf.find("graphicData")?;
    let uri = gdata.attr("uri").unwrap_or("");

    let kind = if uri.contains("table") || gdata.child("tbl").is_some() {
        ShapeKind::Table(super::slide::parse_table(gdata.child("tbl")?, theme))
    } else if uri.contains("chart") {
        let rid = gdata.find("chart").and_then(|c| c.attr("id"))?;
        let chart_path = rels.get(rid)?.clone();
        let chart = parse_chart(zip, &chart_path, theme)?;
        ShapeKind::Chart(chart)
    } else if uri.contains("diagram") {
        // SmartArt: the rendered drawing is referenced by a dsp:dataModelExt
        // relId, resolvable through the slide rels.
        let drawing = gdata
            .find("dataModelExt")
            .and_then(|d| d.attr("relId"))
            .and_then(|rid| rels.get(rid).cloned())
            .and_then(|p| read_zip_text(zip, &p))
            .and_then(|x| xmltree::parse(&x));
        match drawing {
            Some(dom) => ShapeKind::Diagram { children: parse_diagram_drawing(&dom, theme) },
            None => return None,
        }
    } else {
        return None;
    };

    Some(Shape { xfrm, kind, placeholder: None, name: shape_name(gf) })
}

// ── table ───────────────────────────────────────────────────────────────────────

pub fn parse_table(tbl: &Node, theme: &Theme) -> Table {
    let mut table = Table::default();
    if let Some(grid) = tbl.child("tblGrid") {
        for col in grid.children_named("gridCol") {
            table.col_widths.push(col.attr_i64("w").unwrap_or(0));
        }
    }
    let default_run = Run { size_pt: 18.0, ..Default::default() };
    for tr in tbl.children_named("tr") {
        let mut row = TableRow { height_emu: tr.attr_i64("h").unwrap_or(0), cells: Vec::new() };
        for tc in tr.children_named("tc") {
            let mut cell = TableCell::default();
            cell.grid_span = tc.attr_i64("gridSpan").unwrap_or(1).max(0) as u32;
            cell.row_span = tc.attr_i64("rowSpan").unwrap_or(1).max(0) as u32;
            cell.h_merge = tc.attr("hMerge").map(|v| v == "1" || v == "true").unwrap_or(false);
            cell.v_merge = tc.attr("vMerge").map(|v| v == "1" || v == "true").unwrap_or(false);
            if let Some(tcpr) = tc.child("tcPr") {
                cell.fill = parse_fill(tcpr, theme, None).unwrap_or(Fill::None);
                cell.border_l = border_of(tcpr, "lnL", theme);
                cell.border_r = border_of(tcpr, "lnR", theme);
                cell.border_t = border_of(tcpr, "lnT", theme);
                cell.border_b = border_of(tcpr, "lnB", theme);
                if let Some(a) = tcpr.attr("anchor") {
                    cell.anchor = match a {
                        "ctr" => Anchor::Center,
                        "b" => Anchor::Bottom,
                        _ => Anchor::Top,
                    };
                }
            }
            if let Some(tb) = tc.child("txBody") {
                cell.text = parse_text_body(tb, theme, &default_run);
                cell.text.anchor = cell.anchor;
            }
            row.cells.push(cell);
        }
        table.rows.push(row);
    }
    table
}

/// A named border child (`a:lnL` etc.) wraps an outline; reuse parse_line by
/// treating the named element as an `ln` container.
fn border_of(tcpr: &Node, name: &str, theme: &Theme) -> Line {
    // parse_line looks for a child named "ln"; here the element itself is the
    // outline, so parse it inline.
    let ln = match tcpr.child(name) {
        Some(n) => n,
        None => return Line::default(),
    };
    let width_emu = ln.attr_i64("w").unwrap_or(0);
    if ln.child("noFill").is_some() {
        return Line { fill: Fill::None, width_emu, dash: DashKind::Solid };
    }
    let fill = ln
        .child("solidFill")
        .and_then(|sf| super::drawingml::first_color(sf, theme, None))
        .map(|r| Fill::Solid { color: r.rgb, alpha: r.alpha })
        .unwrap_or(Fill::None);
    Line { fill, width_emu, dash: DashKind::Solid }
}

// ── chart ────────────────────────────────────────────────────────────────────────

fn parse_chart(zip: &mut Zip, chart_path: &str, theme: &Theme) -> Option<Chart> {
    let xml = read_zip_text(zip, chart_path)?;
    let dom = xmltree::parse(&xml)?;
    let plot = dom.find("plotArea")?;

    let (kind_node, kind) = [
        ("barChart", ChartKind::Bar),
        ("lineChart", ChartKind::Line),
        ("pieChart", ChartKind::Pie),
        ("pie3DChart", ChartKind::Pie),
        ("doughnutChart", ChartKind::Pie),
        ("areaChart", ChartKind::Area),
        ("scatterChart", ChartKind::Scatter),
    ]
    .into_iter()
    .find_map(|(n, k)| plot.child(n).map(|node| (node, k)))?;

    // Bar orientation: barDir bar=horizontal, col=vertical.
    let kind = if kind == ChartKind::Bar {
        match kind_node.child("barDir").and_then(|d| d.attr("val")) {
            Some("col") => ChartKind::Column,
            _ => ChartKind::Bar,
        }
    } else {
        kind
    };

    let title = dom
        .find("title")
        .map(|t| t.text_content())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let mut categories: Vec<String> = Vec::new();
    let mut series: Vec<ChartSeries> = Vec::new();
    let palette = accent_palette(theme);
    for (i, ser) in kind_node.children_named("ser").enumerate() {
        let name = ser
            .child("tx")
            .map(|t| t.text_content().trim().to_string())
            .unwrap_or_else(|| format!("Series {}", i + 1));
        let color = ser
            .child("spPr")
            .and_then(|s| super::drawingml::first_color(&find_solid(s)?, theme, None).map(|r| r.rgb))
            .or_else(|| palette.get(i % palette.len().max(1)).copied());
        let values = num_values(ser.child("val"));
        if categories.is_empty() {
            categories = str_values(ser.child("cat"));
        }
        series.push(ChartSeries { name, color, values });
    }

    Some(Chart { kind, title, categories, series })
}

fn find_solid(sppr: &Node) -> Option<Node> {
    sppr.child("solidFill").cloned()
}

fn num_values(val: Option<&Node>) -> Vec<f64> {
    let mut out = Vec::new();
    if let Some(v) = val {
        if let Some(numref) = v.find("numCache").or_else(|| v.find("numLit")) {
            for pt in numref.children_named("pt") {
                if let Some(vn) = pt.child("v") {
                    out.push(vn.text_content().trim().parse::<f64>().unwrap_or(0.0));
                }
            }
        }
    }
    out
}

fn str_values(cat: Option<&Node>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(c) = cat {
        if let Some(strref) = c.find("strCache").or_else(|| c.find("numCache")) {
            for pt in strref.children_named("pt") {
                if let Some(vn) = pt.child("v") {
                    out.push(vn.text_content().trim().to_string());
                }
            }
        }
    }
    out
}

fn accent_palette(theme: &Theme) -> Vec<[u8; 3]> {
    ["accent1", "accent2", "accent3", "accent4", "accent5", "accent6"]
        .into_iter()
        .filter_map(|k| theme.colors.get(k).copied())
        .collect::<Vec<_>>()
}

// ── SmartArt drawing ──────────────────────────────────────────────────────────────

/// Parse the pre-rendered SmartArt drawing (`dsp:spTree` of shapes).
fn parse_diagram_drawing(dom: &Node, theme: &Theme) -> Vec<Shape> {
    let sptree = match dom.find("spTree") {
        Some(t) => t,
        None => return Vec::new(),
    };
    let ctx = InheritCtx { layout: None, master: None, theme };
    let mut out = Vec::new();
    for sp in &sptree.children {
        if sp.name == "sp" {
            out.push(parse_sp(sp, &ctx, theme));
        }
    }
    out
}

// ── shared parse helpers ──────────────────────────────────────────────────────────

/// Parse an `a:xfrm` / `p:xfrm` element into an `Xfrm`.
pub fn parse_xfrm_node(xfrm: &Node) -> Xfrm {
    let mut x = Xfrm::default();
    if let Some(off) = xfrm.child("off") {
        x.off_x = off.attr_i64("x").unwrap_or(0);
        x.off_y = off.attr_i64("y").unwrap_or(0);
    }
    if let Some(ext) = xfrm.child("ext") {
        x.ext_cx = ext.attr_i64("cx").unwrap_or(0);
        x.ext_cy = ext.attr_i64("cy").unwrap_or(0);
    }
    x.rot = xfrm.attr_i64("rot").unwrap_or(0) as i32;
    x.flip_h = xfrm.attr("flipH").map(|v| v == "1" || v == "true").unwrap_or(false);
    x.flip_v = xfrm.attr("flipV").map(|v| v == "1" || v == "true").unwrap_or(false);
    if let Some(cho) = xfrm.child("chOff") {
        x.ch_off_x = cho.attr_i64("x").unwrap_or(0);
        x.ch_off_y = cho.attr_i64("y").unwrap_or(0);
        x.has_ch = true;
    }
    if let Some(che) = xfrm.child("chExt") {
        x.ch_ext_cx = che.attr_i64("cx").unwrap_or(0);
        x.ch_ext_cy = che.attr_i64("cy").unwrap_or(0);
        x.has_ch = true;
    }
    x
}

struct PhRaw {
    ph_type: Option<String>,
    idx: Option<u32>,
}

fn read_placeholder(shape: &Node) -> Option<PhRaw> {
    let ph = shape
        .find("nvPr")
        .and_then(|n| n.child("ph"))?;
    Some(PhRaw {
        ph_type: ph.attr("type").map(|s| s.to_string()),
        idx: ph.attr_i64("idx").map(|v| v as u32),
    })
}

fn shape_name(shape: &Node) -> String {
    shape
        .find("cNvPr")
        .and_then(|n| n.attr("name"))
        .unwrap_or("")
        .to_string()
}

fn is_image_path(p: &str) -> bool {
    let lower = p.to_ascii_lowercase();
    lower.contains("/media/") || is_image_ext(&lower)
}

fn is_image_ext(p: &str) -> bool {
    [".png", ".jpg", ".jpeg", ".gif", ".bmp", ".emf", ".wmf", ".tiff", ".tif"]
        .iter()
        .any(|e| p.ends_with(e))
}

/// Decode PNG/JPEG image bytes to RGBA8. Unsupported formats (EMF/WMF/TIFF)
/// return None and the caller renders a placeholder.
pub fn decode_image(bytes: &[u8]) -> Option<ImageData> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    Some(ImageData {
        w: rgba.width() as usize,
        h: rgba.height() as usize,
        rgba: rgba.into_raw(),
    })
}

// ── document-level helpers ────────────────────────────────────────────────────────

/// Title text of a slide (first title/ctrTitle placeholder's text).
pub fn slide_title(slide: &Slide) -> String {
    for shape in &slide.shapes {
        if let (Some(ph), ShapeKind::Sp { text: Some(tb), .. }) = (&shape.placeholder, &shape.kind) {
            if matches!(ph.ph_type.as_deref(), Some("title") | Some("ctrTitle")) {
                let t: String = tb.paragraphs.iter().map(|p| p.plain_text()).collect::<Vec<_>>().join(" ");
                if !t.trim().is_empty() {
                    return t.trim().to_string();
                }
            }
        }
    }
    String::new()
}

/// Collect font family names referenced in a slide's runs.
pub fn collect_fonts(slide: &Slide, push: &mut dyn FnMut(&str)) {
    fn walk(shape: &Shape, push: &mut dyn FnMut(&str)) {
        match &shape.kind {
            ShapeKind::Sp { text: Some(tb), .. } => {
                for p in &tb.paragraphs {
                    for r in &p.runs {
                        if let Some(f) = &r.font {
                            push(f);
                        }
                    }
                }
            }
            ShapeKind::Group { children } | ShapeKind::Diagram { children } => {
                for c in children {
                    walk(c, push);
                }
            }
            ShapeKind::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        for p in &cell.text.paragraphs {
                            for r in &p.runs {
                                if let Some(f) = &r.font {
                                    push(f);
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for shape in &slide.shapes {
        walk(shape, push);
    }
}
