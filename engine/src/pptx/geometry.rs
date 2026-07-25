//! Preset + custom geometry → `raster::Path` (device pixels).
//! Copyright (c) 2026 Anicca Code Studio. MIT licensed.
//!
//! Paths are emitted directly in device space: the caller passes the shape's
//! device rectangle (x, y, w, h) after the slide/group transform, so rotation
//! is applied by the caller via a point transform closure.

use crate::raster::Path;

use super::model::{CustomPath, PathCmd};

/// Result of building a shape outline.
pub struct BuiltGeom {
    pub path: Path,
    /// True when the geometry encloses an area (fillable). Lines are not.
    pub closed: bool,
}

/// A point mapper from local geometry space to device space. Given (gx, gy) in
/// the shape's local rectangle [0,w]x[0,h] returns device (px, py).
pub type Map<'a> = dyn Fn(f64, f64) -> (f64, f64) + 'a;

fn adj_val(adj: &[(String, i64)], name: &str, default: i64) -> i64 {
    adj.iter().find(|(n, _)| n == name).map(|(_, v)| *v).unwrap_or(default)
}

/// Build a preset geometry. `w`,`h` are the shape's local size (device pixels
/// before rotation); `map` applies position + rotation to each point.
pub fn build_preset(name: &str, adj: &[(String, i64)], w: f64, h: f64, map: &Map) -> BuiltGeom {
    let mut p = Path::new();
    let closed;
    match name {
        "line" | "straightConnector1" | "bentConnector2" | "bentConnector3" | "curvedConnector2"
        | "curvedConnector3" => {
            let (x0, y0) = map(0.0, 0.0);
            let (x1, y1) = map(w, h);
            p.move_to(x0, y0);
            p.line_to(x1, y1);
            closed = false;
        }
        "ellipse" | "chord" | "pie" | "arc" => {
            ellipse(&mut p, w, h, map);
            closed = true;
        }
        "roundRect" | "round1Rect" | "round2SameRect" | "snip1Rect" => {
            let a = adj_val(adj, "adj", 16667) as f64 / 100000.0;
            let r = (w.min(h) * a).max(0.0);
            round_rect(&mut p, w, h, r, map);
            closed = true;
        }
        "triangle" | "isoscelesTriangle" => {
            let a = adj_val(adj, "adj", 50000) as f64 / 100000.0;
            poly(&mut p, &[(w * a, 0.0), (w, h), (0.0, h)], map);
            closed = true;
        }
        "rtTriangle" => {
            poly(&mut p, &[(0.0, 0.0), (0.0, h), (w, h)], map);
            closed = true;
        }
        "diamond" => {
            poly(&mut p, &[(w * 0.5, 0.0), (w, h * 0.5), (w * 0.5, h), (0.0, h * 0.5)], map);
            closed = true;
        }
        "parallelogram" => {
            let a = (adj_val(adj, "adj", 25000) as f64 / 100000.0) * w;
            poly(&mut p, &[(a, 0.0), (w, 0.0), (w - a, h), (0.0, h)], map);
            closed = true;
        }
        "trapezoid" => {
            let a = (adj_val(adj, "adj", 25000) as f64 / 100000.0) * w;
            poly(&mut p, &[(a, 0.0), (w - a, 0.0), (w, h), (0.0, h)], map);
            closed = true;
        }
        "pentagon" => {
            regular_ngon(&mut p, 5, w, h, map);
            closed = true;
        }
        "hexagon" => {
            regular_ngon(&mut p, 6, w, h, map);
            closed = true;
        }
        "heptagon" => {
            regular_ngon(&mut p, 7, w, h, map);
            closed = true;
        }
        "octagon" => {
            regular_ngon(&mut p, 8, w, h, map);
            closed = true;
        }
        "star4" | "star5" | "star6" | "star7" | "star8" | "star10" | "star12" | "star16"
        | "star24" | "star32" => {
            let points: usize = name[4..].parse().unwrap_or(5);
            star(&mut p, points, w, h, map);
            closed = true;
        }
        "rightArrow" => {
            arrow_right(&mut p, adj, w, h, map);
            closed = true;
        }
        "leftArrow" => {
            arrow_left(&mut p, adj, w, h, map);
            closed = true;
        }
        "upArrow" => {
            arrow_up(&mut p, adj, w, h, map);
            closed = true;
        }
        "downArrow" => {
            arrow_down(&mut p, adj, w, h, map);
            closed = true;
        }
        "plus" | "mathPlus" => {
            let a = adj_val(adj, "adj", 25000) as f64 / 100000.0;
            let ix = w * a;
            let iy = h * a;
            poly(
                &mut p,
                &[
                    (ix, 0.0), (w - ix, 0.0), (w - ix, iy), (w, iy),
                    (w, h - iy), (w - ix, h - iy), (w - ix, h), (ix, h),
                    (ix, h - iy), (0.0, h - iy), (0.0, iy), (ix, iy),
                ],
                map,
            );
            closed = true;
        }
        "chevron" => {
            let a = adj_val(adj, "adj", 50000) as f64 / 100000.0;
            let ax = w * a;
            poly(
                &mut p,
                &[(0.0, 0.0), (w - ax, 0.0), (w, h * 0.5), (w - ax, h), (0.0, h), (ax, h * 0.5)],
                map,
            );
            closed = true;
        }
        "homePlate" => {
            let a = adj_val(adj, "adj", 50000) as f64 / 100000.0;
            let ax = w * a;
            poly(&mut p, &[(0.0, 0.0), (w - ax, 0.0), (w, h * 0.5), (w - ax, h), (0.0, h)], map);
            closed = true;
        }
        // Rectangle and everything unrecognized: fall back to the bounding rect.
        _ => {
            rect(&mut p, w, h, map);
            closed = true;
        }
    }
    BuiltGeom { path: p, closed }
}

/// Build a custom geometry path. Local coordinates use the path's own w/h space,
/// remapped to the shape rectangle then through `map`.
pub fn build_custom(paths: &[CustomPath], sw: f64, sh: f64, map: &Map) -> BuiltGeom {
    let mut out = Path::new();
    let mut any_fill = false;
    for cp in paths {
        if cp.fill {
            any_fill = true;
        }
        let sx = if cp.w > 0 { sw / cp.w as f64 } else { 1.0 };
        let sy = if cp.h > 0 { sh / cp.h as f64 } else { 1.0 };
        let m = |gx: f64, gy: f64| map(gx * sx, gy * sy);
        let mut cur = (0.0f64, 0.0f64);
        for cmd in &cp.cmds {
            match *cmd {
                PathCmd::Move(x, y) => {
                    let (dx, dy) = m(x as f64, y as f64);
                    out.move_to(dx, dy);
                    cur = (x as f64, y as f64);
                }
                PathCmd::Line(x, y) => {
                    let (dx, dy) = m(x as f64, y as f64);
                    out.line_to(dx, dy);
                    cur = (x as f64, y as f64);
                }
                PathCmd::Cubic(x1, y1, x2, y2, x3, y3) => {
                    let (a, b) = m(x1 as f64, y1 as f64);
                    let (c, d) = m(x2 as f64, y2 as f64);
                    let (e, f) = m(x3 as f64, y3 as f64);
                    out.curve_to(a, b, c, d, e, f);
                    cur = (x3 as f64, y3 as f64);
                }
                PathCmd::Arc(wr, hr, st, sw_ang) => {
                    arc_to(&mut out, cur, wr as f64, hr as f64, st as f64, sw_ang as f64, &m, &mut cur);
                }
                PathCmd::Close => out.close(),
            }
        }
    }
    BuiltGeom { path: out, closed: any_fill }
}

// ── primitive builders ──────────────────────────────────────────────────────────

fn rect(p: &mut Path, w: f64, h: f64, map: &Map) {
    poly(p, &[(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)], map);
}

fn poly(p: &mut Path, pts: &[(f64, f64)], map: &Map) {
    if pts.is_empty() {
        return;
    }
    let (x0, y0) = map(pts[0].0, pts[0].1);
    p.move_to(x0, y0);
    for &(x, y) in &pts[1..] {
        let (dx, dy) = map(x, y);
        p.line_to(dx, dy);
    }
    p.close();
}

/// Ellipse inscribed in the w×h box, via four cubic Béziers.
fn ellipse(p: &mut Path, w: f64, h: f64, map: &Map) {
    let cx = w * 0.5;
    let cy = h * 0.5;
    let rx = w * 0.5;
    let ry = h * 0.5;
    let k = 0.5522847498307936; // 4/3 (sqrt2 - 1)
    let ox = rx * k;
    let oy = ry * k;
    let m = |x: f64, y: f64| map(x, y);
    let (sx, sy) = m(cx + rx, cy);
    p.move_to(sx, sy);
    curve(p, &m, cx + rx, cy + oy, cx + ox, cy + ry, cx, cy + ry);
    curve(p, &m, cx - ox, cy + ry, cx - rx, cy + oy, cx - rx, cy);
    curve(p, &m, cx - rx, cy - oy, cx - ox, cy - ry, cx, cy - ry);
    curve(p, &m, cx + ox, cy - ry, cx + rx, cy - oy, cx + rx, cy);
    p.close();
}

fn curve(p: &mut Path, m: &dyn Fn(f64, f64) -> (f64, f64), x1: f64, y1: f64, x2: f64, y2: f64, x3: f64, y3: f64) {
    let (a, b) = m(x1, y1);
    let (c, d) = m(x2, y2);
    let (e, f) = m(x3, y3);
    p.curve_to(a, b, c, d, e, f);
}

fn round_rect(p: &mut Path, w: f64, h: f64, r: f64, map: &Map) {
    let r = r.min(w * 0.5).min(h * 0.5);
    let k = 0.5522847498307936;
    let o = r * k;
    let m = |x: f64, y: f64| map(x, y);
    let (sx, sy) = m(r, 0.0);
    p.move_to(sx, sy);
    // top edge → top-right corner
    let (x, y) = m(w - r, 0.0);
    p.line_to(x, y);
    curve(p, &m, w - r + o, 0.0, w, r - o, w, r);
    let (x, y) = m(w, h - r);
    p.line_to(x, y);
    curve(p, &m, w, h - r + o, w - r + o, h, w - r, h);
    let (x, y) = m(r, h);
    p.line_to(x, y);
    curve(p, &m, r - o, h, 0.0, h - r + o, 0.0, h - r);
    let (x, y) = m(0.0, r);
    p.line_to(x, y);
    curve(p, &m, 0.0, r - o, r - o, 0.0, r, 0.0);
    p.close();
}

fn regular_ngon(p: &mut Path, n: usize, w: f64, h: f64, map: &Map) {
    let cx = w * 0.5;
    let cy = h * 0.5;
    let rx = w * 0.5;
    let ry = h * 0.5;
    let mut pts = Vec::with_capacity(n);
    let start = -std::f64::consts::FRAC_PI_2;
    for i in 0..n {
        let ang = start + i as f64 * std::f64::consts::TAU / n as f64;
        pts.push((cx + rx * ang.cos(), cy + ry * ang.sin()));
    }
    poly(p, &pts, map);
}

fn star(p: &mut Path, points: usize, w: f64, h: f64, map: &Map) {
    let cx = w * 0.5;
    let cy = h * 0.5;
    let rx = w * 0.5;
    let ry = h * 0.5;
    let inner = 0.38;
    let mut pts = Vec::with_capacity(points * 2);
    let start = -std::f64::consts::FRAC_PI_2;
    for i in 0..points * 2 {
        let ang = start + i as f64 * std::f64::consts::PI / points as f64;
        let (r0, r1) = if i % 2 == 0 { (rx, ry) } else { (rx * inner, ry * inner) };
        pts.push((cx + r0 * ang.cos(), cy + r1 * ang.sin()));
    }
    poly(p, &pts, map);
}

fn arrow_right(p: &mut Path, adj: &[(String, i64)], w: f64, h: f64, map: &Map) {
    let tail = adj_val(adj, "adj2", 50000) as f64 / 100000.0; // body thickness
    let head = adj_val(adj, "adj1", 50000) as f64 / 100000.0; // head length ratio
    let hy = h * (1.0 - tail) * 0.5;
    let hx = w * (1.0 - head);
    poly(
        p,
        &[
            (0.0, hy), (hx, hy), (hx, 0.0), (w, h * 0.5), (hx, h), (hx, h - hy), (0.0, h - hy),
        ],
        map,
    );
}

fn arrow_left(p: &mut Path, adj: &[(String, i64)], w: f64, h: f64, map: &Map) {
    let tail = adj_val(adj, "adj2", 50000) as f64 / 100000.0;
    let head = adj_val(adj, "adj1", 50000) as f64 / 100000.0;
    let hy = h * (1.0 - tail) * 0.5;
    let hx = w * head;
    poly(
        p,
        &[
            (w, hy), (hx, hy), (hx, 0.0), (0.0, h * 0.5), (hx, h), (hx, h - hy), (w, h - hy),
        ],
        map,
    );
}

fn arrow_up(p: &mut Path, adj: &[(String, i64)], w: f64, h: f64, map: &Map) {
    let tail = adj_val(adj, "adj2", 50000) as f64 / 100000.0;
    let head = adj_val(adj, "adj1", 50000) as f64 / 100000.0;
    let hx = w * (1.0 - tail) * 0.5;
    let hy = h * head;
    poly(
        p,
        &[
            (hx, h), (hx, hy), (0.0, hy), (w * 0.5, 0.0), (w, hy), (w - hx, hy), (w - hx, h),
        ],
        map,
    );
}

fn arrow_down(p: &mut Path, adj: &[(String, i64)], w: f64, h: f64, map: &Map) {
    let tail = adj_val(adj, "adj2", 50000) as f64 / 100000.0;
    let head = adj_val(adj, "adj1", 50000) as f64 / 100000.0;
    let hx = w * (1.0 - tail) * 0.5;
    let hy = h * (1.0 - head);
    poly(
        p,
        &[
            (hx, 0.0), (hx, hy), (0.0, hy), (w * 0.5, h), (w, hy), (w - hx, hy), (w - hx, 0.0),
        ],
        map,
    );
}

/// Approximate an `arcTo` by flattening into line/curve segments.
fn arc_to(
    out: &mut Path,
    cur_local: (f64, f64),
    wr: f64,
    hr: f64,
    st_ang: f64,
    sw_ang: f64,
    m: &dyn Fn(f64, f64) -> (f64, f64),
    cur: &mut (f64, f64),
) {
    // Angles are in 60000ths of a degree.
    let st = st_ang / 60000.0 * std::f64::consts::PI / 180.0;
    let sw = sw_ang / 60000.0 * std::f64::consts::PI / 180.0;
    if wr.abs() < 1e-6 || hr.abs() < 1e-6 {
        return;
    }
    // Ellipse center so that the arc starts at the current point.
    let cx = cur_local.0 - wr * st.cos();
    let cy = cur_local.1 - hr * st.sin();
    let steps = ((sw.abs() / (std::f64::consts::PI / 18.0)).ceil() as usize).max(1);
    for i in 1..=steps {
        let a = st + sw * (i as f64 / steps as f64);
        let x = cx + wr * a.cos();
        let y = cy + hr * a.sin();
        let (dx, dy) = m(x, y);
        out.line_to(dx, dy);
        *cur = (x, y);
    }
}
