//! Debug: rasterize every slide of a PPTX to PNG files.
//! Run: cargo run --release --example render_pptx -- <path.pptx> <out_dir> [width]

fn main() {
    let path = std::env::args().nth(1).expect("usage: render_pptx <file.pptx> <out_dir> [width]");
    let out_dir = std::env::args().nth(2).unwrap_or_else(|| ".".to_string());
    let out_w: usize = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(1280);

    let bytes = std::fs::read(&path).expect("read file");
    let doc = anicca_engine::pptx::parse(&bytes).expect("parse pptx");

    let (sw, sh) = doc.slide_size_pt();
    let out_h = (out_w as f32 * sh / sw).round() as usize;

    let mut fs = anicca_engine::render::new_font_system();
    let mut swash = cosmic_text::SwashCache::new();

    let pages = doc.page_count();
    println!("slides={pages} size={sw:.0}x{sh:.0}pt render={out_w}x{out_h}");

    for p in 0..pages {
        if std::env::var("LAYOUT").is_ok() {
            let page = anicca_engine::pptx::layout::layout_slide(&mut fs, &doc, p);
            let glyphs: usize = page.frames.iter().flat_map(|f| f.parcel.lines.iter()).count();
            eprintln!("[layout] slide{p} frames={} lines={}", page.frames.len(), glyphs);
        }
        let rgba = anicca_engine::pptx::render::render_slide_rgba(&mut fs, &mut swash, &doc, p, out_w, out_h);
        let img = image::RgbaImage::from_raw(out_w as u32, out_h as u32, rgba).expect("buffer");
        let file = format!("{out_dir}/slide{:02}.png", p);
        img.save(&file).expect("save png");
        println!("wrote {file}");
    }
}
