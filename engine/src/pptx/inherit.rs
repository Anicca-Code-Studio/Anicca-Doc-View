//! Placeholder inheritance: slide → slideLayout → slideMaster, plus master
//! list styles (`p:txStyles`) and layered background resolution.
//! Copyright (c) 2026 Anicca Code Studio. MIT licensed.

use super::drawingml::{apply_run_props, parse_fill, resolve_theme_font};
use super::model::{Fill, Run, Xfrm};
use super::theme::Theme;
use super::xmltree::Node;

/// Resolved layout + master context for one slide.
pub struct InheritCtx<'a> {
    pub layout: Option<&'a Node>,
    pub master: Option<&'a Node>,
    pub theme: &'a Theme,
}

/// Which master list style applies to a placeholder type.
fn style_group(ph_type: Option<&str>) -> &'static str {
    match ph_type {
        Some("title") | Some("ctrTitle") => "titleStyle",
        Some("body") | Some("subTitle") | Some("obj") | None => "bodyStyle",
        _ => "otherStyle",
    }
}

/// Compute the default run style for a placeholder at a paragraph level,
/// following master list styles then the placeholder's own layout/master rPr.
pub fn default_run(ctx: &InheritCtx, ph_type: Option<&str>, ph_idx: Option<u32>, level: u8) -> Run {
    let mut run = Run { size_pt: 18.0, ..Default::default() };

    // 1. Master list style (titleStyle/bodyStyle/otherStyle) for this level.
    if let Some(master) = ctx.master {
        if let Some(txstyles) = master.child("txStyles") {
            let group = style_group(ph_type);
            if let Some(style) = txstyles.child(group) {
                apply_level_defrpr(style, level, ctx.theme, &mut run);
            }
        }
    }

    // 2. Placeholder's own default run props from layout, then master shape.
    if let Some(layout) = ctx.layout {
        if let Some(sp) = find_placeholder(layout, ph_type, ph_idx) {
            apply_ph_lvl_defrpr(sp, level, ctx.theme, &mut run);
        }
    }
    if let Some(master) = ctx.master {
        if let Some(sp) = find_placeholder(master, ph_type, ph_idx) {
            apply_ph_lvl_defrpr(sp, level, ctx.theme, &mut run);
        }
    }

    // Default title/body sizing if the master gave nothing.
    if run.size_pt <= 0.0 {
        run.size_pt = match style_group(ph_type) {
            "titleStyle" => 44.0,
            _ => 18.0,
        };
    }
    // Default font from theme when unset.
    if run.font.is_none() {
        run.font = match style_group(ph_type) {
            "titleStyle" => ctx.theme.major_latin.clone(),
            _ => ctx.theme.minor_latin.clone(),
        };
    }
    run
}

fn apply_level_defrpr(style: &Node, level: u8, theme: &Theme, run: &mut Run) {
    let tag = format!("lvl{}pPr", level + 1);
    if let Some(lvl) = style.child(&tag) {
        if let Some(defrpr) = lvl.child("defRPr") {
            apply_defrpr(defrpr, theme, run);
        }
    }
}

fn apply_ph_lvl_defrpr(sp: &Node, level: u8, theme: &Theme, run: &mut Run) {
    // txBody → lstStyle → lvlNpPr → defRPr, else bodyPr defaults.
    if let Some(txbody) = sp.child("txBody") {
        if let Some(lst) = txbody.child("lstStyle") {
            let tag = format!("lvl{}pPr", level + 1);
            if let Some(lvl) = lst.child(&tag) {
                if let Some(defrpr) = lvl.child("defRPr") {
                    apply_defrpr(defrpr, theme, run);
                }
            }
        }
    }
}

fn apply_defrpr(defrpr: &Node, theme: &Theme, run: &mut Run) {
    // Reuse run-property application; it reads sz/b/i/color/latin.
    apply_run_props(defrpr, run, theme);
    // Latin font token resolution already handled inside apply_run_props via
    // resolve_theme_font; but defRPr fonts may reference +mj/+mn.
    if let Some(latin) = defrpr.child("latin").and_then(|n| n.attr("typeface")) {
        if let Some(f) = resolve_theme_font(latin, theme) {
            run.font = Some(f);
        }
    }
}

/// Find a placeholder shape in a layout/master by type and/or index.
pub fn find_placeholder<'a>(root: &'a Node, ph_type: Option<&str>, ph_idx: Option<u32>) -> Option<&'a Node> {
    let sptree = root.find("spTree")?;
    let mut fallback = None;
    for sp in sptree.children_named("sp") {
        let ph = sp
            .child("nvSpPr")
            .and_then(|n| n.child("nvPr"))
            .and_then(|n| n.child("ph"));
        let ph = match ph {
            Some(p) => p,
            None => continue,
        };
        let t = ph.attr("type");
        let idx = ph.attr_i64("idx").map(|v| v as u32);
        // Exact idx match wins.
        if ph_idx.is_some() && idx == ph_idx {
            return Some(sp);
        }
        // Type match (normalize title/ctrTitle equivalence).
        if types_match(t, ph_type) && fallback.is_none() {
            fallback = Some(sp);
        }
    }
    fallback
}

fn norm_type(t: Option<&str>) -> &str {
    match t {
        Some("ctrTitle") => "title",
        Some("subTitle") => "body",
        None => "body",
        Some(x) => x,
    }
}

fn types_match(a: Option<&str>, b: Option<&str>) -> bool {
    norm_type(a) == norm_type(b)
}

/// Inherit a placeholder's transform from layout then master when the slide
/// shape did not specify its own `a:xfrm`.
pub fn inherit_xfrm(ctx: &InheritCtx, ph_type: Option<&str>, ph_idx: Option<u32>) -> Option<Xfrm> {
    for root in [ctx.layout, ctx.master].into_iter().flatten() {
        if let Some(sp) = find_placeholder(root, ph_type, ph_idx) {
            if let Some(xfrm) = sp
                .child("spPr")
                .and_then(|spr| spr.child("xfrm"))
                .map(super::slide::parse_xfrm_node)
            {
                if xfrm.has_size() {
                    return Some(xfrm);
                }
            }
        }
    }
    None
}

/// Resolve the slide background, following slide → layout → master.
pub fn resolve_background(
    slide: &Node,
    ctx: &InheritCtx,
    theme: &Theme,
) -> Option<Fill> {
    for root in [Some(slide), ctx.layout, ctx.master].into_iter().flatten() {
        if let Some(bg) = root.find("bg") {
            // bg → bgPr (fill) or bgRef (scheme index).
            if let Some(bgpr) = bg.child("bgPr") {
                if let Some(f) = parse_fill(bgpr, theme, None) {
                    return Some(f);
                }
            }
            if let Some(bgref) = bg.child("bgRef") {
                // bgRef idx points into theme fill styles; approximate with its color.
                if let Some(c) = super::drawingml::first_color(bgref, theme, None) {
                    return Some(Fill::Solid { color: c.rgb, alpha: c.alpha });
                }
            }
        }
    }
    None
}
