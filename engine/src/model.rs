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
    /// Font size in points.
    pub size_pt: f32,
    /// RGB text color.
    pub color: [u8; 3],
}

impl Default for RunStyle {
    fn default() -> Self {
        RunStyle {
            bold: false,
            italic: false,
            size_pt: 11.0,
            color: [0, 0, 0],
        }
    }
}

#[derive(Clone, Debug)]
pub struct Run {
    pub text: String,
    pub style: RunStyle,
}

#[derive(Clone, Debug)]
pub struct Paragraph {
    pub runs: Vec<Run>,
    pub align: Align,
    /// Spacing before/after the paragraph, in points.
    pub space_before_pt: f32,
    pub space_after_pt: f32,
    /// Line-height multiplier (1.0 = single spacing).
    pub line_pct: f32,
    /// Heading outline level (0 = Heading 1) if this paragraph is a heading.
    pub outline_level: Option<u8>,
}

impl Default for Paragraph {
    fn default() -> Self {
        Paragraph {
            runs: Vec::new(),
            align: Align::Left,
            space_before_pt: 0.0,
            space_after_pt: 0.0,
            line_pct: 1.0,
            outline_level: None,
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
pub struct Document {
    pub paragraphs: Vec<Paragraph>,
    /// Page geometry in points.
    pub page_w_pt: f32,
    pub page_h_pt: f32,
    pub margin_pt: f32,
    /// Computed during layout; number of laid-out pages (>= 1).
    pub page_count: usize,
    /// Computed during layout; start page index for each paragraph.
    pub paragraph_pages: Vec<usize>,
    /// Original file bytes (returned by get_bytes).
    pub bytes: Vec<u8>,
}

impl Document {
    pub fn content_w_pt(&self) -> f32 {
        (self.page_w_pt - 2.0 * self.margin_pt).max(1.0)
    }

    pub fn content_h_pt(&self) -> f32 {
        (self.page_h_pt - 2.0 * self.margin_pt).max(1.0)
    }
}
