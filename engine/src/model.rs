//! Document model produced by the DOCX parser and consumed by layout/render.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Align {
    Left,
    Center,
    Right,
    Justify,
}

impl Default for Align {
    fn default() -> Self {
        Align::Left
    }
}

#[derive(Clone, Debug)]
pub struct RunStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// Font size in points.
    pub size_pt: f32,
    /// RGB text color.
    pub color: [u8; 3],
    /// Font family name as specified in the document (e.g. "Calibri", "Arial").
    pub font_name: Option<String>,
}

impl Default for RunStyle {
    fn default() -> Self {
        RunStyle {
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            size_pt: 11.0,
            color: [0, 0, 0],
            font_name: None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    Bmp,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct InlineImage {
    /// Dimensions in English Metric Units (914400 EMU = 1 inch).
    pub width_emu: u64,
    pub height_emu: u64,
    pub data: Vec<u8>,
    pub format: ImageFormat,
}

/// Floating/anchored image with absolute position (wp:anchor).
#[derive(Clone, Debug)]
pub struct AnchorImage {
    pub width_emu: u64,
    pub height_emu: u64,
    pub data: Vec<u8>,
    pub format: ImageFormat,
    /// Horizontal offset in EMU from reference point (signed).
    pub pos_x_emu: i64,
    /// Vertical offset in EMU from reference point (signed, negative = above).
    pub pos_y_emu: i64,
    /// Horizontal reference: 0=column, 1=page, 2=margin.
    pub pos_ref_h: u8,
    /// Vertical reference: 0=paragraph, 1=page, 2=margin.
    pub pos_ref_v: u8,
    /// True if image is rendered behind text (behindDoc).
    pub behind_doc: bool,
}

#[derive(Clone, Debug)]
pub struct Run {
    pub text: String,
    pub style: RunStyle,
    /// Inline image replaces text for this run when Some.
    pub inline_image: Option<InlineImage>,
    /// True when this run is a PAGE field result; text is replaced with the
    /// actual page number at render time.
    pub is_page_number: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TabAlign {
    Left,
    Right,
    Center,
    Decimal,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TabLeader {
    None,
    Dot,
    Hyphen,
    Underscore,
}

#[derive(Clone, Debug)]
pub struct TabStop {
    /// Position in DXA (twentieths of a point) from left margin.
    pub pos_dxa: u32,
    pub align: TabAlign,
    pub leader: TabLeader,
}

#[derive(Clone, Debug)]
pub struct Paragraph {
    pub runs: Vec<Run>,
    /// Floating images anchored relative to this paragraph.
    pub anchor_images: Vec<AnchorImage>,
    pub align: Align,
    /// Spacing before/after the paragraph, in points.
    pub space_before_pt: f32,
    pub space_after_pt: f32,
    /// Line-height multiplier (1.0 = single spacing).
    pub line_pct: f32,
    /// Heading outline level (0 = Heading 1) if this paragraph is a heading.
    pub outline_level: Option<u8>,
    /// Left indent in points.
    pub indent_left_pt: f32,
    /// Right indent in points.
    pub indent_right_pt: f32,
    /// First-line indent in points (positive = indent, negative = hanging).
    pub indent_first_line_pt: f32,
    /// Resolved list prefix string (e.g. "1.", "•", "a)").
    pub list_prefix: Option<String>,
    /// Additional left indent for list content (hanging indent amount), in points.
    pub list_hanging_pt: f32,
    /// Tab stops defined in paragraph properties.
    pub tab_stops: Vec<TabStop>,
}

impl Default for Paragraph {
    fn default() -> Self {
        Paragraph {
            runs: Vec::new(),
            anchor_images: Vec::new(),
            align: Align::Left,
            space_before_pt: 0.0,
            space_after_pt: 0.0,
            line_pct: 1.0,
            outline_level: None,
            indent_left_pt: 0.0,
            indent_right_pt: 0.0,
            indent_first_line_pt: 0.0,
            list_prefix: None,
            list_hanging_pt: 0.0,
            tab_stops: Vec::new(),
        }
    }
}

impl Paragraph {
    /// Plain-text content (used for outline titles).
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

#[derive(Clone, Debug)]
pub struct BorderLine {
    /// Border size in eighths of a point (0 = none).
    pub size_eighth_pt: u32,
    pub color: [u8; 3],
    pub style: BorderStyle,
    /// True when this border was explicitly specified in the document.
    /// An explicit cell border (even "nil") overrides the table-level border.
    pub explicit: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BorderStyle {
    None,
    Single,
    Double,
    Dashed,
    Dotted,
}

impl Default for BorderLine {
    fn default() -> Self {
        BorderLine {
            size_eighth_pt: 0,
            color: [0, 0, 0],
            style: BorderStyle::None,
            explicit: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Borders {
    pub top: BorderLine,
    pub bottom: BorderLine,
    pub left: BorderLine,
    pub right: BorderLine,
    pub inside_h: BorderLine,
    pub inside_v: BorderLine,
}

#[derive(Clone, Debug)]
pub struct TableCell {
    pub blocks: Vec<Block>,
    /// Cell width in DXA (twentieths of a point). 0 = auto.
    pub width_dxa: u32,
    /// Number of grid columns this cell spans (default 1).
    pub grid_span: u32,
    pub borders: Borders,
    /// Cell background color (None = transparent/white).
    pub bg_color: Option<[u8; 3]>,
}

impl Default for TableCell {
    fn default() -> Self {
        TableCell {
            blocks: Vec::new(),
            width_dxa: 0,
            grid_span: 1,
            borders: Borders::default(),
            bg_color: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TableRow {
    pub cells: Vec<TableCell>,
    /// Row height in DXA (0 = auto).
    pub height_dxa: u32,
    /// If true, height is exact; if false, at-least.
    pub height_exact: bool,
}

impl Default for TableRow {
    fn default() -> Self {
        TableRow {
            cells: Vec::new(),
            height_dxa: 0,
            height_exact: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Table {
    pub rows: Vec<TableRow>,
    /// Table total width in DXA (when width_is_pct=false) or 50ths-of-percent (when true).
    pub width_dxa: u32,
    /// True if width_dxa is in 50ths-of-percent (pct type), false = DXA.
    pub width_is_pct: bool,
    /// Table left indent in DXA (can be negative, e.g. header bars that
    /// extend left of the margin).
    pub indent_dxa: i32,
    pub borders: Borders,
    /// Cell margin in DXA (applied to all cells unless overridden).
    pub cell_margin_dxa: u32,
    /// Grid column widths in DXA from <w:tblGrid>. Authoritative when non-empty.
    pub grid_col_widths: Vec<u32>,
}

impl Default for Table {
    fn default() -> Self {
        Table {
            rows: Vec::new(),
            width_dxa: 0,
            width_is_pct: false,
            indent_dxa: 0,
            borders: Borders::default(),
            cell_margin_dxa: 72, // 72 DXA = ~3.6pt default Word cell margin
            grid_col_widths: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Block {
    Paragraph(Paragraph),
    Table(Table),
    PageBreak,
}

/// Parsed page header or footer content (from word/header*.xml / footer*.xml).
#[derive(Clone, Debug)]
pub struct HeaderFooter {
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug)]
pub struct Document {
    pub blocks: Vec<Block>,
    /// Page geometry in points.
    pub page_w_pt: f32,
    pub page_h_pt: f32,
    /// Left/right margin in points.
    pub margin_l_pt: f32,
    pub margin_r_pt: f32,
    /// Top/bottom margin in points.
    pub margin_t_pt: f32,
    pub margin_b_pt: f32,
    /// Computed during layout; number of laid-out pages (>= 1).
    pub page_count: usize,
    /// Per-block starting page index (parallel to blocks), populated after measure().
    pub block_pages: Vec<usize>,
    /// Original file bytes (returned by get_bytes).
    pub bytes: Vec<u8>,
    /// Embedded font bytes extracted and deobfuscated from the DOCX.
    pub embedded_fonts: Vec<Vec<u8>>,
    /// Default header/footer (pages 1+ when a distinct first-section variant exists).
    pub header: Option<HeaderFooter>,
    pub footer: Option<HeaderFooter>,
    /// Header/footer for the first page (first section of the document).
    pub header_first: Option<HeaderFooter>,
    pub footer_first: Option<HeaderFooter>,
    /// Distance from page top to header content start, in points (w:pgMar w:header).
    pub header_margin_pt: f32,
    /// Distance from page bottom to footer content end, in points (w:pgMar w:footer).
    pub footer_margin_pt: f32,
    /// Page number of the first page (w:pgNumType w:start, default 1).
    pub page_num_start: i32,
}

impl Document {
    pub fn content_w_pt(&self) -> f32 {
        (self.page_w_pt - self.margin_l_pt - self.margin_r_pt).max(1.0)
    }

    pub fn content_h_pt(&self) -> f32 {
        (self.page_h_pt - self.margin_t_pt - self.margin_b_pt).max(1.0)
    }
}
