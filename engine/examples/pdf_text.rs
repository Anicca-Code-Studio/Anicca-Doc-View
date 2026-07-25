//! Debug: dump the selectable text layer of a PDF page.
//! Run: cargo run --release --example pdf_text -- <path.pdf> [page] [--layout]

use anicca_engine::pdf;
use anicca_engine::render::{LpLineContent, LpRunContent};

fn main() {
    let path = std::env::args().nth(1).expect("usage: pdf_text <file.pdf> [page] [--layout]");
    let page_no: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    let show_layout = std::env::args().any(|a| a == "--layout");

    let bytes = std::fs::read(&path).expect("read file");
    let doc = pdf::parse(&bytes).expect("parse pdf");
    let page = doc.pages.get(page_no - 1).expect("page out of range");

    let t0 = std::time::Instant::now();
    let layout = pdf::content::layout_page(&doc.file, page);
    let ms = t0.elapsed().as_millis();

    let mut runs = 0usize;
    let mut chars = 0usize;
    for frame in &layout.frames {
        for line in &frame.parcel.lines {
            if let LpLineContent::RunList(rl) = &line.content {
                for run in &rl.runs {
                    if let LpRunContent::Glyphs { text, glyphs, .. } = &run.content {
                        runs += 1;
                        chars += text.chars().count();
                        assert_eq!(
                            text.chars().count(),
                            glyphs.len(),
                            "glyph/char count must match for search mapping"
                        );
                    }
                }
            }
        }
    }
    println!(
        "page {page_no}: {:.0}x{:.0} pt, {} lines, {runs} runs, {chars} chars ({ms} ms)",
        layout.width,
        layout.height,
        layout.frames.iter().map(|f| f.parcel.lines.len()).sum::<usize>()
    );

    if show_layout {
        for frame in &layout.frames {
            for (i, line) in frame.parcel.lines.iter().enumerate() {
                if let LpLineContent::RunList(rl) = &line.content {
                    for run in &rl.runs {
                        if let LpRunContent::Glyphs { text, font_size, .. } = &run.content {
                            println!(
                                "  line {i:>3} @({:>7.2},{:>7.2}) {font_size:>5.1}pt  {text:?}",
                                run.transform.translate_x, run.transform.translate_y
                            );
                        }
                    }
                }
            }
        }
    } else {
        println!("─── text ───");
        println!("{}", pdf::text::Collector::plain_text_of_layout(&layout));
    }
}
