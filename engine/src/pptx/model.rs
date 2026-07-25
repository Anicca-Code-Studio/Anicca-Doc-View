//! Shape-based document model for PPTX (PowerPoint).
//! Copyright (c) 2026 Anicca Code Studio. MIT licensed.
//!
//! Unlike the block-flow `crate::model::Document` (Word/Excel), a slide is a
//! canvas of absolutely-positioned shapes. This model mirrors DrawingML: each
//! shape carries a transform (position/size/rotation in EMU), an optional
//! geometry + fill + outline, and an optional text body.

/// English Metric Units per point (914400 EMU = 1 inch, 72 pt = 1 inch).
pub const EMU_PER_PT: f64 = 12700.0;

/// A shape transform: `a:xfrm`. Offsets/extents in EMU.
#[derive(Clone, Copy, Debug, Default)]
pub struct Xfrm {
    pub off_x: i64,
    pub off_y: i64,
    pub ext_cx: i64,
    pub ext_cy: i64,
    /// Rotation in 60000ths of a degree (clockwise).
    pub rot: i32,
    pub flip_h: bool,
    pub flip_v: bool,
    /// Child coordinate origin/extent for group shapes (`a:chOff`/`a:chExt`).
    pub ch_off_x: i64,
    pub ch_off_y: i64,
    pub ch_ext_cx: i64,
    pub ch_ext_cy: i64,
    pub has_ch: bool,
}

impl Xfrm {
    pub fn has_size(&self) -> bool {
        self.ext_cx > 0 && self.ext_cy > 0
    }
}

/// A resolved gradient stop.
#[derive(Clone, Copy, Debug)]
pub struct GradStop {
    /// Position 0.0..=1.0.
    pub pos: f32,
    pub color: [u8; 3],
    /// Alpha 0.0..=1.0.
    pub alpha: f32,
}

#[derive(Clone, Debug)]
pub enum Fill {
    None,
    Solid { color: [u8; 3], alpha: f32 },
    Gradient {
        stops: Vec<GradStop>,
        /// Linear angle in degrees (0 = left→right), ignored for radial.
        angle_deg: f32,
        radial: bool,
    },
    /// Image fill: index into `PptxDocument`-level media is avoided; the decoded
    /// RGBA bitmap is carried directly for simplicity.
    Blip { rgba: Option<ImageData> },
}

impl Default for Fill {
    fn default() -> Self {
        Fill::None
    }
}

/// Decoded image (RGBA8, premultiplied-agnostic; alpha kept separate).
#[derive(Clone, Debug)]
pub struct ImageData {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DashKind {
    Solid,
    Dash,
    Dot,
    DashDot,
}

#[derive(Clone, Debug)]
pub struct Line {
    pub fill: Fill,
    /// Width in EMU.
    pub width_emu: i64,
    pub dash: DashKind,
}

impl Default for Line {
    fn default() -> Self {
        Line { fill: Fill::None, width_emu: 0, dash: DashKind::Solid }
    }
}

/// Geometry: either a named preset (+ adjust values) or a custom path.
#[derive(Clone, Debug)]
pub enum Geom {
    Preset { name: String, adj: Vec<(String, i64)> },
    Custom { paths: Vec<CustomPath> },
    None,
}

impl Default for Geom {
    fn default() -> Self {
        Geom::None
    }
}

/// A `a:custGeom` path in local geometry units (`w`/`h` guide space).
#[derive(Clone, Debug)]
pub struct CustomPath {
    /// Path space width/height in geometry units (`a:path w=.. h=..`).
    pub w: i64,
    pub h: i64,
    pub cmds: Vec<PathCmd>,
    pub fill: bool,
    pub stroke: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum PathCmd {
    Move(i64, i64),
    Line(i64, i64),
    Cubic(i64, i64, i64, i64, i64, i64),
    /// arcTo: wR, hR, stAng, swAng (angles in 60000ths degree).
    Arc(i64, i64, i64, i64),
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
    Justify,
}

impl Default for TextAlign {
    fn default() -> Self {
        TextAlign::Left
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Anchor {
    Top,
    Center,
    Bottom,
}

impl Default for Anchor {
    fn default() -> Self {
        Anchor::Top
    }
}

/// A single run of text with resolved character formatting.
#[derive(Clone, Debug)]
pub struct Run {
    pub text: String,
    pub size_pt: f32,
    pub color: [u8; 3],
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub font: Option<String>,
    /// East-asian font (`a:ea`).
    pub font_ea: Option<String>,
}

impl Default for Run {
    fn default() -> Self {
        Run {
            text: String::new(),
            size_pt: 18.0,
            color: [0, 0, 0],
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            font: None,
            font_ea: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Para {
    pub runs: Vec<Run>,
    pub align: TextAlign,
    /// Indent level 0..=8 (`a:pPr lvl`).
    pub level: u8,
    /// Bullet text ("•", "-", "1." …). None = no bullet.
    pub bullet: Option<String>,
    pub bullet_color: Option<[u8; 3]>,
    /// Space before/after in points.
    pub space_before_pt: f32,
    pub space_after_pt: f32,
    /// Line spacing multiplier (1.0 = single). When line_pct is None and
    /// line_exact_pt is Some, use exact points.
    pub line_pct: Option<f32>,
    pub line_exact_pt: Option<f32>,
    /// Left margin / hanging indent in points (`marL` / `indent`).
    pub margin_left_pt: f32,
    pub indent_pt: f32,
    /// Default run size for an empty paragraph (drives blank-line height).
    pub default_size_pt: f32,
}

impl Para {
    pub fn plain_text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

#[derive(Clone, Debug, Default)]
pub struct TextBody {
    pub paragraphs: Vec<Para>,
    pub anchor: Anchor,
    /// Insets in points (default 0.1"/0.05" per OOXML).
    pub inset_l_pt: f32,
    pub inset_t_pt: f32,
    pub inset_r_pt: f32,
    pub inset_b_pt: f32,
    pub wrap: bool,
    /// Font scale from `a:normAutofit fontScale` (percent/100000), default 1.0.
    pub font_scale: f32,
}

/// Placeholder identity for inheritance matching (`p:ph`).
#[derive(Clone, Debug, Default)]
pub struct Placeholder {
    pub ph_type: Option<String>,
    pub idx: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct TableCell {
    pub text: TextBody,
    pub fill: Fill,
    pub border_l: Line,
    pub border_r: Line,
    pub border_t: Line,
    pub border_b: Line,
    pub anchor: Anchor,
    /// Horizontal span (default 1). 0 = merged/continuation cell (skipped).
    pub grid_span: u32,
    pub row_span: u32,
    pub h_merge: bool,
    pub v_merge: bool,
}

impl Default for TableCell {
    fn default() -> Self {
        TableCell {
            text: TextBody::default(),
            fill: Fill::None,
            border_l: Line::default(),
            border_r: Line::default(),
            border_t: Line::default(),
            border_b: Line::default(),
            anchor: Anchor::Top,
            grid_span: 1,
            row_span: 1,
            h_merge: false,
            v_merge: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TableRow {
    pub height_emu: i64,
    pub cells: Vec<TableCell>,
}

#[derive(Clone, Debug, Default)]
pub struct Table {
    /// Column widths in EMU.
    pub col_widths: Vec<i64>,
    pub rows: Vec<TableRow>,
}

/// Chart series + categories parsed from `chartN.xml`.
#[derive(Clone, Debug)]
pub struct ChartSeries {
    pub name: String,
    pub color: Option<[u8; 3]>,
    pub values: Vec<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChartKind {
    Bar,
    Column,
    Line,
    Pie,
    Area,
    Scatter,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct Chart {
    pub kind: ChartKind,
    pub title: Option<String>,
    pub categories: Vec<String>,
    pub series: Vec<ChartSeries>,
}

/// The concrete content a shape carries.
#[derive(Clone, Debug)]
pub enum ShapeKind {
    /// Autoshape / text box (`p:sp`).
    Sp {
        geom: Geom,
        fill: Fill,
        line: Line,
        text: Option<TextBody>,
    },
    /// Picture (`p:pic`).
    Pic {
        image: Option<ImageData>,
        line: Line,
    },
    /// Group (`p:grpSp`) with children in child coordinate space.
    Group { children: Vec<Shape> },
    /// Table (`p:graphicFrame` → `a:tbl`).
    Table(Table),
    /// Chart (`p:graphicFrame` → `c:chart`).
    Chart(Chart),
    /// Pre-rendered SmartArt drawing (shapes from `ppt/diagrams/drawingN.xml`).
    Diagram { children: Vec<Shape> },
}

#[derive(Clone, Debug)]
pub struct Shape {
    pub xfrm: Xfrm,
    pub kind: ShapeKind,
    pub placeholder: Option<Placeholder>,
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct Slide {
    pub shapes: Vec<Shape>,
    pub background: Option<Fill>,
}
