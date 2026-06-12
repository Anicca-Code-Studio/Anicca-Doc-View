//! Debug: rasterize every page of a DOCX to PNG files.
//! Run: cargo run --release --example render_pages -- <path.docx> <out_dir>

fn main() {
    let path = std::env::args().nth(1).expect("usage: render_pages <file.docx> <out_dir>");
    let out_dir = std::env::args().nth(2).unwrap_or_else(|| ".".to_string());
    let bytes = std::fs::read(&path).expect("read file");
    let doc = anicca_engine::docx::parse(&bytes).expect("parse");

    let mut fs = anicca_engine::render::new_font_system();
    anicca_engine::render::load_embedded_fonts(&mut fs, &doc.embedded_fonts);
    let mut swash = cosmic_text::SwashCache::new();

    let (pages, _) = anicca_engine::render::measure(&mut fs, &doc);
    let out_w = 794usize;
    let out_h = (out_w as f32 * doc.page_h_pt / doc.page_w_pt) as usize;
    println!("rendering {pages} pages at {out_w}x{out_h}");

    for p in 0..pages {
        let rgba = anicca_engine::render::render_page(&mut fs, &mut swash, &doc, p, out_w, out_h);
        let img = image::RgbaImage::from_raw(out_w as u32, out_h as u32, rgba).expect("buffer");
        let file = format!("{out_dir}/page{:02}.png", p);
        img.save(&file).expect("save png");
        println!("wrote {file}");
    }
}
