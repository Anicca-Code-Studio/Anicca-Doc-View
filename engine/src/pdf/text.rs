//! Collected text, for the selectable text layer.
//!
//! The content-stream interpreter reports every glyph it draws here, with its
//! baseline position and advance already in device space. Grouping those glyphs
//! into lines and emitting the viewer's layout structures happens on top of
//! this record.

use crate::raster::Transform;
use crate::render::{
    LpFrame, LpGlyph, LpLine, LpLineContent, LpPage, LpParcel, LpRun, LpRunContent, LpRunList,
    LpTransform,
};

use super::content::TextParams;
use super::font::Font;

/// A gap wider than this fraction of the em size is read as a word space.
const SPACE_GAP: f64 = 0.22;
/// Baselines further apart than this fraction of the em size start a new line.
const LINE_TOLERANCE: f64 = 0.35;
/// A font-size change beyond this ratio starts a new run.
const SIZE_TOLERANCE: f64 = 0.02;

/// One glyph as painted on the page.
#[derive(Clone, Debug)]
pub struct PlacedGlyph {
    /// Unicode text this glyph stands for; empty when unmappable.
    pub text: String,
    /// Baseline origin, in device pixels.
    pub x: f64,
    pub y: f64,
    /// Advance vector, in device pixels.
    pub adv_x: f64,
    pub adv_y: f64,
    /// Em size in device pixels (the glyph's vertical scale).
    pub size: f64,
    /// Font ascent and descent, in em units.
    pub ascent: f64,
    pub descent: f64,
    /// Normalized writing direction in device space.
    pub dir_x: f64,
    pub dir_y: f64,
    /// True when this code is a space character.
    pub is_space: bool,
    /// Character code, kept so the text layer can report offsets.
    pub code: u32,
}

#[derive(Default)]
pub struct Collector {
    pub glyphs: Vec<PlacedGlyph>,
    /// Cap so a pathological page cannot exhaust memory.
    limit: usize,
}

impl Collector {
    pub fn new() -> Collector {
        Collector { glyphs: Vec::new(), limit: 400_000 }
    }

    /// Records one painted glyph.
    ///
    /// `trm` is the glyph-space to device-space matrix, so its translation is
    /// the baseline origin and its y axis length is the em size in pixels.
    /// `adv` is the pen movement in text space before the text matrix.
    pub fn push_glyph(
        &mut self,
        font: &Font,
        code: u32,
        cid: u32,
        adv: f64,
        params: &TextParams,
        trm: &Transform,
        text_to_device: &Transform,
    ) {
        if self.glyphs.len() >= self.limit {
            return;
        }
        let text = font.to_text(code, cid).unwrap_or_default();
        let (ascent, descent) = font.ascent_descent();

        let size = (trm.c * trm.c + trm.d * trm.d).sqrt();
        let (dx, dy) = text_to_device.apply_vec(1.0, 0.0);
        let dlen = (dx * dx + dy * dy).sqrt().max(1e-12);

        // Advance in text space, including spacing, then mapped to device.
        let word = if !font.composite && code == 32 { params.word_spacing } else { 0.0 };
        let tx = (adv * params.size + params.char_spacing + word) * params.h_scale;
        let (adv_x, adv_y) = text_to_device.apply_vec(tx, 0.0);

        let is_space = text == " " || (text.is_empty() && code == 32);

        self.glyphs.push(PlacedGlyph {
            text,
            x: trm.e,
            y: trm.f,
            adv_x,
            adv_y,
            size,
            ascent,
            descent,
            dir_x: dx / dlen,
            dir_y: dy / dlen,
            is_space,
            code,
        });
    }

    /// Builds the viewer's layout structure: one line per detected text line,
    /// each holding runs positioned by an absolute transform.
    ///
    /// The viewer treats a layout as "flat" when runs carry non-identity
    /// transforms, which is exactly the PDF case: there is no paragraph
    /// structure to mirror, only glyphs at coordinates.
    pub fn layout_page(&self, width: f32, height: f32) -> LpPage {
        let mut lines: Vec<LpLine> = Vec::new();
        let mut line: Option<LineBuilder> = None;

        for g in &self.glyphs {
            if g.size <= 0.0 {
                continue;
            }
            match &mut line {
                Some(lb) if lb.accepts(g) => lb.push(g),
                _ => {
                    if let Some(done) = line.take() {
                        if let Some(l) = done.finish() {
                            lines.push(l);
                        }
                    }
                    let mut lb = LineBuilder::new(g);
                    lb.push(g);
                    line = Some(lb);
                }
            }
        }
        if let Some(done) = line {
            if let Some(l) = done.finish() {
                lines.push(l);
            }
        }

        LpPage {
            width,
            height,
            frames: vec![LpFrame {
                transform: LpTransform::identity(),
                parcel: LpParcel { x: 0.0, y: 0.0, width, height, lines },
            }],
        }
    }

    /// Plain text of the page, in the order the glyphs were painted.
    pub fn plain_text_of_layout(page: &LpPage) -> String {
        let mut out = String::new();
        for frame in &page.frames {
            for line in &frame.parcel.lines {
                if let LpLineContent::RunList(rl) = &line.content {
                    for run in &rl.runs {
                        match &run.content {
                            LpRunContent::Glyphs { text, .. } => out.push_str(text),
                            LpRunContent::Break => out.push('\n'),
                            LpRunContent::Space { .. } => out.push(' '),
                            _ => {}
                        }
                    }
                }
            }
        }
        out
    }

    /// Plain text of the page, with line breaks inferred from baseline changes.
    /// Useful for diagnostics and as the basis of the text layer.
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        let mut last: Option<&PlacedGlyph> = None;
        for g in &self.glyphs {
            if let Some(p) = last {
                let same_line = (g.y - p.y).abs() <= p.size * 0.5
                    && (g.dir_x - p.dir_x).abs() < 0.01
                    && (g.dir_y - p.dir_y).abs() < 0.01;
                if !same_line {
                    out.push('\n');
                } else {
                    // Insert a space when the pen jumped further than a glyph's
                    // worth of slack: PDFs often omit space characters.
                    let expected = p.x + p.adv_x;
                    let gap = g.x - expected;
                    if !g.is_space && !out.ends_with(' ') && gap > p.size * 0.2 {
                        out.push(' ');
                    }
                }
            }
            out.push_str(&g.text);
            last = Some(g);
        }
        out
    }
}

// ── line and run assembly ────────────────────────────────────────────────────

/// Accumulates the glyphs of one visual line into runs of uniform size.
struct LineBuilder {
    /// Writing direction of the line.
    dir_x: f64,
    dir_y: f64,
    /// Origin of the line, used to measure perpendicular drift.
    origin_x: f64,
    origin_y: f64,
    size: f64,
    runs: Vec<LpRun>,
    current: Option<RunBuilder>,
}

impl LineBuilder {
    fn new(g: &PlacedGlyph) -> LineBuilder {
        LineBuilder {
            dir_x: g.dir_x,
            dir_y: g.dir_y,
            origin_x: g.x,
            origin_y: g.y,
            size: g.size,
            runs: Vec::new(),
            current: None,
        }
    }

    /// True when `g` belongs to the line being built: same writing direction
    /// and no meaningful drift away from its baseline.
    fn accepts(&self, g: &PlacedGlyph) -> bool {
        if (g.dir_x - self.dir_x).abs() > 0.02 || (g.dir_y - self.dir_y).abs() > 0.02 {
            return false;
        }
        let ref_size = self.size.max(g.size).max(1e-6);
        // Perpendicular offset from the line's baseline.
        let perp = (g.x - self.origin_x) * -self.dir_y + (g.y - self.origin_y) * self.dir_x;
        perp.abs() <= ref_size * LINE_TOLERANCE
    }

    fn push(&mut self, g: &PlacedGlyph) {
        let along = (g.x - self.origin_x) * self.dir_x + (g.y - self.origin_y) * self.dir_y;
        let advance = (g.adv_x * g.adv_x + g.adv_y * g.adv_y).sqrt();

        let same_run = match &self.current {
            Some(r) => (r.size - g.size).abs() <= r.size * SIZE_TOLERANCE,
            None => false,
        };
        if !same_run {
            if let Some(done) = self.current.take() {
                if let Some(run) = done.finish() {
                    self.runs.push(run);
                }
            }
            self.current = Some(RunBuilder::new(g, along));
        }
        if let Some(r) = self.current.as_mut() {
            r.push(g, along, advance);
        }
    }

    fn finish(mut self) -> Option<LpLine> {
        if let Some(done) = self.current.take() {
            if let Some(run) = done.finish() {
                self.runs.push(run);
            }
        }
        if self.runs.is_empty() {
            return None;
        }
        // A break run gives the extracted text a newline between lines.
        self.runs.push(LpRun {
            x: 0.0,
            width: 0.0,
            transform: LpTransform::identity(),
            content: LpRunContent::Break,
        });

        let width = self
            .runs
            .iter()
            .map(|r| r.x + r.width)
            .fold(0.0f32, f32::max);
        let height = self.size as f32;
        Some(LpLine {
            y: 0.0,
            width,
            height,
            space_before: 0.0,
            space_after: 0.0,
            is_first_line_of_para: true,
            is_last_line_of_para: true,
            content: LpLineContent::RunList(LpRunList {
                baseline: 0.0,
                width,
                height,
                runs: self.runs,
            }),
        })
    }
}

/// One run: a stretch of characters at one size, positioned by an absolute
/// transform whose translation is the run's baseline origin.
struct RunBuilder {
    origin_x: f64,
    origin_y: f64,
    dir_x: f64,
    dir_y: f64,
    size: f64,
    ascent: f64,
    descent: f64,
    /// Position of the run origin along the line axis.
    base_along: f64,
    /// Pen position along the line axis, relative to the run origin.
    cursor: f64,
    text: String,
    glyphs: Vec<LpGlyph>,
}

impl RunBuilder {
    fn new(g: &PlacedGlyph, along: f64) -> RunBuilder {
        RunBuilder {
            origin_x: g.x,
            origin_y: g.y,
            dir_x: g.dir_x,
            dir_y: g.dir_y,
            size: g.size,
            ascent: g.ascent.abs() * g.size,
            descent: g.descent.abs() * g.size,
            base_along: along,
            cursor: 0.0,
            text: String::new(),
            glyphs: Vec::new(),
        }
    }

    fn push(&mut self, g: &PlacedGlyph, along: f64, advance: f64) {
        let x = along - self.base_along;

        // PDFs frequently position words by advancing the pen instead of
        // emitting a space, so a gap has to be turned back into one.
        if !self.text.is_empty() && !g.is_space && !self.text.ends_with(' ') {
            let gap = x - self.cursor;
            if gap > self.size * SPACE_GAP {
                self.glyphs.push(LpGlyph {
                    x: self.cursor as f32,
                    y: 0.0,
                    advance: gap as f32,
                    offset: self.text.len(),
                });
                self.text.push(' ');
            }
        }

        let chars: Vec<char> = if g.text.is_empty() {
            // An unmappable glyph still needs to occupy space so following
            // characters line up; the replacement character keeps the text and
            // glyph arrays in step.
            vec!['\u{FFFD}']
        } else {
            g.text.chars().collect()
        };
        let per = if chars.is_empty() { advance } else { advance / chars.len() as f64 };
        for (i, c) in chars.iter().enumerate() {
            self.glyphs.push(LpGlyph {
                x: (x + per * i as f64) as f32,
                y: 0.0,
                advance: per as f32,
                offset: self.text.len(),
            });
            self.text.push(*c);
        }
        self.cursor = x + advance;
    }

    fn finish(self) -> Option<LpRun> {
        if self.text.trim().is_empty() || self.glyphs.is_empty() {
            return None;
        }
        // The transform carries rotation and position; scale stays 1 so the
        // viewer reads `fontSize` directly in points.
        let transform = LpTransform {
            scale_x: self.dir_x as f32,
            skew_y: self.dir_y as f32,
            skew_x: -self.dir_y as f32,
            scale_y: self.dir_x as f32,
            translate_x: self.origin_x as f32,
            translate_y: self.origin_y as f32,
        };
        let width = self
            .glyphs
            .last()
            .map(|g| g.x + g.advance)
            .unwrap_or(0.0);
        Some(LpRun {
            x: 0.0,
            width,
            transform,
            content: LpRunContent::Glyphs {
                text: self.text,
                font_size: self.size as f32,
                ascent: self.ascent as f32,
                descent: self.descent as f32,
                glyphs: self.glyphs,
            },
        })
    }
}
