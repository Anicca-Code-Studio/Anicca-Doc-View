//! Test layout_page(): prints frame/line/glyph counts per page to verify non-empty output.
//! Run: cargo run --example test_layout -- <path.docx>

fn main() {
    let path = std::env::args().nth(1).expect("usage: test_layout <file.docx>");
    let bytes = std::fs::read(&path).expect("read file");
    let doc = anicca_engine::docx::parse(&bytes).expect("parse");

    let mut fs = anicca_engine::render::new_font_system();
    anicca_engine::render::load_embedded_fonts(&mut fs, &doc.embedded_fonts);

    let (pages, _) = anicca_engine::render::measure(&mut fs, &doc);
    println!("document: {pages} pages, size={:.1}x{:.1}pt", doc.page_w_pt, doc.page_h_pt);

    let mut total_lines = 0usize;
    let mut total_glyphs = 0usize;

    for p in 0..pages {
        let lp = anicca_engine::render::layout_page(&mut fs, &doc, p);
        let frame_count = lp.frames.len();
        let line_count: usize = lp.frames.iter()
            .flat_map(|f| &f.parcel.lines)
            .count();
        let glyph_count: usize = lp.frames.iter()
            .flat_map(|f| &f.parcel.lines)
            .map(|l| match &l.content {
                anicca_engine::render::LpLineContent::RunList(rl) => {
                    rl.runs.iter().map(|r| match &r.content {
                        anicca_engine::render::LpRunContent::Glyphs { glyphs, .. } => glyphs.len(),
                        _ => 0,
                    }).sum::<usize>()
                }
                anicca_engine::render::LpLineContent::Table(tbl) => {
                    tbl.rows.iter().flat_map(|r| &r.cells)
                        .filter_map(|c| c.parcel.as_ref())
                        .flat_map(|p| &p.lines)
                        .map(|l| match &l.content {
                            anicca_engine::render::LpLineContent::RunList(rl) => {
                                rl.runs.iter().map(|r| match &r.content {
                                    anicca_engine::render::LpRunContent::Glyphs { glyphs, .. } => glyphs.len(),
                                    _ => 0,
                                }).sum::<usize>()
                            }
                            _ => 0,
                        }).sum::<usize>()
                }
            })
            .sum();

        total_lines += line_count;
        total_glyphs += glyph_count;

        if p < 5 || line_count > 0 {
            println!("  page {:02}: frames={frame_count} lines={line_count} glyphs={glyph_count}", p);
        }

        // Print first line details on page 1
        if p == 1 {
            for frame in &lp.frames {
                for (i, line) in frame.parcel.lines.iter().take(3).enumerate() {
                    if let anicca_engine::render::LpLineContent::RunList(rl) = &line.content {
                        for run in &rl.runs {
                            if let anicca_engine::render::LpRunContent::Glyphs { text, font_size, ascent, descent, glyphs } = &run.content {
                                println!("    line[{i}] y={:.2}pt baseline={:.2}pt text={:?} font={:.1}pt asc={:.2} desc={:.2} glyphs={}",
                                    line.y, rl.baseline, &text[..text.len().min(40)], font_size, ascent, descent, glyphs.len());
                            }
                        }
                    }
                }
            }
        }
    }

    println!("\nTOTAL: lines={total_lines} glyphs={total_glyphs}");
    if total_glyphs == 0 {
        eprintln!("ERROR: zero glyphs! get_layout_page returns empty data.");
        std::process::exit(1);
    } else {
        println!("OK: layout_page() returns real text data.");
    }
}
