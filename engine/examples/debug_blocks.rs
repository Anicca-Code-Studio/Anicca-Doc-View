//! Debug: print block sequence + measured pages for a DOCX file.
//! Run: cargo run --release --example debug_blocks -- <path.docx>

use anicca_engine::model::Block;

fn main() {
    let path = std::env::args().nth(1).expect("usage: debug_blocks <file.docx>");
    let bytes = std::fs::read(&path).expect("read file");
    let doc = anicca_engine::docx::parse(&bytes).expect("parse");

    println!(
        "page {}x{}pt margins l{} r{} t{} b{} hdr_margin {} ftr_margin {} pg_start {}",
        doc.page_w_pt, doc.page_h_pt, doc.margin_l_pt, doc.margin_r_pt,
        doc.margin_t_pt, doc.margin_b_pt, doc.header_margin_pt, doc.footer_margin_pt,
        doc.page_num_start
    );
    println!(
        "header: {} footer: {} header_first: {} footer_first: {}",
        doc.header.is_some(), doc.footer.is_some(),
        doc.header_first.is_some(), doc.footer_first.is_some()
    );

    let mut fs = anicca_engine::render::new_font_system();
    anicca_engine::render::load_embedded_fonts(&mut fs, &doc.embedded_fonts);
    let (pages, block_pages) = anicca_engine::render::measure(&mut fs, &doc);
    println!("measured pages: {pages}");

    for (i, block) in doc.blocks.iter().enumerate() {
        let pg = block_pages.get(i).copied().unwrap_or(999);
        match block {
            Block::Paragraph(p) => {
                let txt: String = p.text().chars().take(60).collect();
                println!("{i:3} pg{pg:2} PARA  ol={:?} runs={} '{txt}'", p.outline_level, p.runs.len());
            }
            Block::Table(t) => {
                println!("{i:3} pg{pg:2} TABLE rows={} cols={}", t.rows.len(), t.grid_col_widths.len());
            }
            Block::PageBreak => {
                println!("{i:3} pg{pg:2} PAGEBREAK");
            }
        }
    }
}
