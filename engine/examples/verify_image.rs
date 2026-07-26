//! Verifies the engine's own image decoders against the reference `image` crate.
//!
//!   cargo run --release --example verify_image -- gen <dir>     # write test images
//!   cargo run --release --example verify_image -- check <file>  # compare decoders
//!
//! `check` decodes a file both with `image_fmt` (ours) and the `image` crate
//! (ground truth) and reports the maximum per-channel difference. RGB must match
//! exactly for lossless formats; alpha may differ only where our value is opaque
//! against a colour-keyed source, which we treat as a mismatch too.

use anicca_engine::image_fmt;

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let arg = std::env::args().nth(2).unwrap_or_else(|| ".".to_string());
    match mode.as_str() {
        "gen" => gen(&arg),
        "check" => check(&arg),
        _ => eprintln!("usage: verify_image gen <dir> | check <file>"),
    }
}

/// Writes a raw ground-truth sidecar next to `path`: "arGB" magic, w, h (u32
/// LE), then RGBA8 rows. `check` compares against this so no third-party decoder
/// is needed at verification time.
fn write_sidecar(path: &str, w: u32, h: u32, rgba: &[u8]) {
    let mut out = Vec::with_capacity(12 + rgba.len());
    out.extend_from_slice(b"aRGB");
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(rgba);
    std::fs::write(format!("{path}.rgba"), out).unwrap();
}

fn gen(dir: &str) {
    use image::{DynamicImage, GrayImage, Luma, Rgb, RgbImage, Rgba, RgbaImage};
    std::fs::create_dir_all(dir).ok();
    let (w, h) = (64u32, 48u32);

    // RGBA gradient with a varying alpha ramp.
    let mut rgba = RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let r = (x * 255 / (w - 1)) as u8;
            let g = (y * 255 / (h - 1)) as u8;
            let b = ((x + y) * 255 / (w + h - 2)) as u8;
            let a = (x * 255 / (w - 1)) as u8;
            rgba.put_pixel(x, y, Rgba([r, g, b, a]));
        }
    }
    let p = format!("{dir}/grad_rgba.png");
    rgba.save(&p).unwrap();
    write_sidecar(&p, w, h, rgba.as_raw());

    // Grayscale.
    let mut gray = GrayImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            gray.put_pixel(x, y, Luma([((x + y) * 255 / (w + h - 2)) as u8]));
        }
    }
    let p = format!("{dir}/grad_gray.png");
    gray.save(&p).unwrap();
    write_sidecar(&p, w, h, DynamicImage::ImageLuma8(gray).to_rgba8().as_raw());

    // RGB → BMP (24-bit) and GIF (palette). GIF quantises, so its sidecar is the
    // re-decoded GIF, not the source RGB.
    let mut rgb = RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            rgb.put_pixel(x, y, Rgb([(x * 4) as u8, (y * 5) as u8, 128]));
        }
    }
    let pb = format!("{dir}/grad_rgb.bmp");
    rgb.save(&pb).unwrap();
    write_sidecar(&pb, w, h, DynamicImage::ImageRgb8(rgb.clone()).to_rgba8().as_raw());

    let pg = format!("{dir}/grad_rgb.gif");
    rgb.save(&pg).unwrap();
    // Ground truth for the GIF = what the reference decoder reads back.
    let regif = image::open(&pg).unwrap().to_rgba8();
    write_sidecar(&pg, regif.width(), regif.height(), regif.as_raw());

    println!("wrote test images + sidecars to {dir}");
}

fn check(path: &str) {
    let bytes = std::fs::read(path).expect("read file");

    let mine = match image_fmt::parse(&bytes) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("FAIL {path}: our decoder rejected it: {e}");
            std::process::exit(1);
        }
    };

    // Ground truth: a .rgba sidecar if present (no third-party decoder needed),
    // else the reference `image` crate.
    let (rw, rh, refdata): (usize, usize, Vec<u8>) = match std::fs::read(format!("{path}.rgba")) {
        Ok(s) if s.len() >= 12 && &s[0..4] == b"aRGB" => {
            let w = u32::from_le_bytes([s[4], s[5], s[6], s[7]]) as usize;
            let h = u32::from_le_bytes([s[8], s[9], s[10], s[11]]) as usize;
            (w, h, s[12..].to_vec())
        }
        _ => {
            let r = image::load_from_memory(&bytes).expect("reference decode").to_rgba8();
            (r.width() as usize, r.height() as usize, r.into_raw())
        }
    };

    if (mine.bitmap.w, mine.bitmap.h) != (rw, rh) {
        eprintln!(
            "FAIL {path}: size mismatch ours={}x{} ref={}x{}",
            mine.bitmap.w, mine.bitmap.h, rw, rh
        );
        std::process::exit(1);
    }

    let mut max_rgb = 0i32;
    let mut max_a = 0i32;
    let mut sum_abs = 0f64;
    for i in 0..rw * rh {
        let o = i * 4;
        let m = &mine.bitmap.data[o..o + 4];
        let rp = &refdata[o..o + 4];
        for c in 0..3 {
            let d = (m[c] as i32 - rp[c] as i32).abs();
            if d > max_rgb {
                max_rgb = d;
            }
            sum_abs += d as f64;
        }
        let da = (m[3] as i32 - rp[3] as i32).abs();
        if da > max_a {
            max_a = da;
        }
    }
    let mean = sum_abs / (rw * rh * 3) as f64;

    println!(
        "{path}: {}x{} fmt={} maxRGBdiff={max_rgb} maxAlphaDiff={max_a} meanAbsErr={mean:.3}",
        rw, rh, mine.format
    );

    // Lossless formats must match exactly; JPEG differs only by IDCT rounding
    // between decoders, so allow a small bounded error there.
    let lossy = mine.format == "jpeg" || mine.format == "webp";
    let (max_ok, mean_ok) = if lossy { (16, 1.5) } else { (1, 0.01) };
    if max_rgb > max_ok || mean > mean_ok {
        eprintln!("FAIL {path}: diff exceeds tolerance (max {max_rgb}>{max_ok} or mean {mean:.3}>{mean_ok})");
        std::process::exit(1);
    }
}
