//! Debug: rasterize every page of a PDF to PNG files.
//! Run: cargo run --release --example render_pdf -- <path.pdf> <out_dir> [width_px] [max_pages]

fn main() {
    let path = std::env::args().nth(1).expect("usage: render_pdf <file.pdf> <out_dir> [width] [max_pages]");
    let out_dir = std::env::args().nth(2).unwrap_or_else(|| ".".to_string());
    let want_w: usize = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);
    let max_pages: usize = std::env::args()
        .nth(4)
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX);

    let bytes = std::fs::read(&path).expect("read file");
    let doc = anicca_engine::pdf::parse(&bytes).expect("parse pdf");
    std::fs::create_dir_all(&out_dir).ok();

    let stem = std::path::Path::new(&path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "page".to_string());
    let stem: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();

    let n = doc.page_count().min(max_pages);
    println!("{} pages, rendering {n} at width {want_w}px", doc.page_count());

    for i in 0..n {
        let page = &doc.pages[i];
        let (pw, ph) = page.size();
        let out_w = want_w;
        let out_h = ((want_w as f32) * ph / pw).round().max(1.0) as usize;

        let t0 = std::time::Instant::now();
        // Rotation is baked in here so the PNG matches what a viewer shows.
        let rgba =
            anicca_engine::pdf::content::render_page_rgba(&doc.file, page, out_w, out_h, true);
        let ms = t0.elapsed().as_millis();

        let img = image::RgbaImage::from_raw(out_w as u32, out_h as u32, rgba).expect("buffer");
        let file = format!("{out_dir}/{stem}_p{:02}.png", i + 1);
        img.save(&file).expect("save png");
        println!("wrote {file}  ({out_w}x{out_h}, {ms} ms)");
    }
}
