//! Debug: decode a standalone image via the engine's own decoders and rasterize
//! it to a PNG, exactly as the viewer would.
//! Run: cargo run --release --example render_image -- <path> <out_dir> [width_px]

use anicca_engine::image_fmt;
use anicca_engine::raster::{Canvas, Clip, Transform};

fn main() {
    let path = std::env::args().nth(1).expect("usage: render_image <file> <out_dir> [width]");
    let out_dir = std::env::args().nth(2).unwrap_or_else(|| ".".to_string());
    let want_w: usize = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(1000);

    let bytes = std::fs::read(&path).expect("read file");
    let doc = image_fmt::parse(&bytes).expect("decode image");
    std::fs::create_dir_all(&out_dir).ok();

    let stem = std::path::Path::new(&path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".to_string());
    let stem: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();

    let out_w = want_w;
    let out_h = ((want_w as f32) * doc.h_pt / doc.w_pt).round().max(1.0) as usize;

    let mut canvas = Canvas::new(out_w, out_h);
    canvas.clear([255, 255, 255]);
    let ctm = Transform::new(out_w as f64, 0.0, 0.0, -(out_h as f64), 0.0, out_h as f64);
    canvas.draw_image(&doc.bitmap, &ctm, &Clip::full(out_w, out_h), 1.0, true);

    let img = image::RgbaImage::from_raw(out_w as u32, out_h as u32, canvas.data).expect("buffer");
    let file = format!("{out_dir}/{stem}_{}.png", doc.format);
    img.save(&file).expect("save png");
    println!(
        "{}: {}x{}px  page {:.0}x{:.0}pt  -> {file} ({out_w}x{out_h})",
        doc.format, doc.bitmap.w, doc.bitmap.h, doc.w_pt, doc.h_pt
    );
}
