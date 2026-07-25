//! Debug: render the rasterizer's feature set to a PNG for visual inspection.
//! Run: cargo run --release --example raster_smoke -- <out.png>

use anicca_engine::raster::*;

fn star(cx: f64, cy: f64, r: f64) -> Path {
    // Five-pointed star drawn as one self-intersecting loop: the centre is
    // wound twice, so nonzero fills it and even-odd does not.
    let mut p = Path::new();
    for i in 0..5 {
        let a = -std::f64::consts::FRAC_PI_2 + (i as f64) * 4.0 * std::f64::consts::PI / 5.0;
        let (x, y) = (cx + r * a.cos(), cy + r * a.sin());
        if i == 0 {
            p.move_to(x, y);
        } else {
            p.line_to(x, y);
        }
    }
    p.close();
    p
}

fn label_box(canvas: &mut Canvas, x: f64, y: f64, w: f64, h: f64, clip: &Clip) {
    let mut p = Path::new();
    p.rect(x, y, w, h);
    let style = StrokeStyle { width: 1.0, ..Default::default() };
    canvas.stroke_path(&p, &Transform::identity(), &style, &Paint::Solid([200, 200, 200]), clip, 1.0);
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "raster_smoke.png".to_string());
    let (w, h) = (900usize, 620usize);
    let mut canvas = Canvas::new(w, h);
    canvas.clear([255, 255, 255]);
    let clip = Clip::full(w, h);
    let id = Transform::identity();

    // Row 1 ── fill rules on a self-intersecting star.
    label_box(&mut canvas, 20.0, 20.0, 200.0, 200.0, &clip);
    canvas.fill_path(&star(120.0, 120.0, 85.0), FillRule::NonZero, &Paint::Solid([210, 60, 40]), &clip, 1.0);

    label_box(&mut canvas, 240.0, 20.0, 200.0, 200.0, &clip);
    canvas.fill_path(&star(340.0, 120.0, 85.0), FillRule::EvenOdd, &Paint::Solid([40, 90, 200]), &clip, 1.0);

    // Row 1 ── circle plus a Bézier blob, to check curve flattening.
    label_box(&mut canvas, 460.0, 20.0, 200.0, 200.0, &clip);
    let mut c = Path::new();
    c.circle(560.0, 120.0, 80.0);
    canvas.fill_path(&c, FillRule::NonZero, &Paint::Solid([30, 160, 90]), &clip, 1.0);
    let mut hole = Path::new();
    hole.circle(560.0, 120.0, 40.0);
    canvas.fill_path(&hole, FillRule::NonZero, &Paint::Solid([255, 255, 255]), &clip, 1.0);

    label_box(&mut canvas, 680.0, 20.0, 200.0, 200.0, &clip);
    let mut b = Path::new();
    b.move_to(700.0, 190.0);
    b.curve_to(700.0, 40.0, 860.0, 40.0, 860.0, 190.0);
    b.curve_to(820.0, 120.0, 740.0, 120.0, 700.0, 190.0);
    b.close();
    canvas.fill_path(&b, FillRule::NonZero, &Paint::Solid([150, 60, 190]), &clip, 1.0);

    // Row 2 ── joins: miter, round, bevel on a sharp corner.
    let joins = [
        (LineJoin::Miter, [200, 40, 40], 20.0),
        (LineJoin::Round, [40, 140, 60], 240.0),
        (LineJoin::Bevel, [40, 80, 200], 460.0),
    ];
    for (join, color, ox) in joins {
        label_box(&mut canvas, ox, 250.0, 200.0, 160.0, &clip);
        let mut p = Path::new();
        p.move_to(ox + 25.0, 390.0);
        p.line_to(ox + 100.0, 270.0);
        p.line_to(ox + 175.0, 390.0);
        let style = StrokeStyle { width: 22.0, join, miter_limit: 10.0, ..Default::default() };
        canvas.stroke_path(&p, &id, &style, &Paint::Solid(color), &clip, 1.0);
    }

    // Row 2 ── caps and dashes.
    label_box(&mut canvas, 680.0, 250.0, 200.0, 160.0, &clip);
    for (i, cap) in [LineCap::Butt, LineCap::Round, LineCap::Square].iter().enumerate() {
        let y = 285.0 + i as f64 * 35.0;
        let mut p = Path::new();
        p.move_to(710.0, y);
        p.line_to(850.0, y);
        let style = StrokeStyle { width: 16.0, cap: *cap, ..Default::default() };
        canvas.stroke_path(&p, &id, &style, &Paint::Solid([90, 90, 90]), &clip, 1.0);
    }
    let mut d = Path::new();
    d.move_to(710.0, 392.0);
    d.line_to(850.0, 392.0);
    let style = StrokeStyle { width: 6.0, dash: vec![12.0, 6.0], dash_phase: 0.0, ..Default::default() };
    canvas.stroke_path(&d, &id, &style, &Paint::Solid([200, 120, 0]), &clip, 1.0);

    // Row 3 ── clipping: a star-shaped clip over a striped background.
    label_box(&mut canvas, 20.0, 440.0, 200.0, 160.0, &clip);
    let star_clip = clip.intersect_path(&star(120.0, 520.0, 70.0), FillRule::NonZero);
    for i in 0..20 {
        let mut s = Path::new();
        s.rect(20.0 + i as f64 * 10.0, 440.0, 5.0, 160.0);
        canvas.fill_path(&s, FillRule::NonZero, &Paint::Solid([220, 30, 120]), &star_clip, 1.0);
    }

    // Row 3 ── alpha ramp over a black bar.
    label_box(&mut canvas, 240.0, 440.0, 200.0, 160.0, &clip);
    let mut bar = Path::new();
    bar.rect(250.0, 500.0, 180.0, 40.0);
    canvas.fill_path(&bar, FillRule::NonZero, &Paint::Solid([0, 0, 0]), &clip, 1.0);
    for i in 0..9 {
        let mut sq = Path::new();
        sq.rect(250.0 + i as f64 * 20.0, 455.0, 18.0, 130.0);
        canvas.fill_path(&sq, FillRule::NonZero, &Paint::Solid([255, 180, 0]), &clip, (i as f32 + 1.0) / 9.0);
    }

    // Row 3 ── rotated and skewed image blit.
    label_box(&mut canvas, 460.0, 440.0, 200.0, 160.0, &clip);
    let mut img = Bitmap::new(8, 8);
    for y in 0..8 {
        for x in 0..8 {
            let i = (y * 8 + x) * 4;
            let dark = (x + y) % 2 == 0;
            let c: [u8; 4] = if dark { [20, 20, 20, 255] } else { [240, 200, 40, 255] };
            img.data[i..i + 4].copy_from_slice(&c);
        }
    }
    // Unit square -> 120x120, rotated 20 degrees, y flipped (PDF convention).
    let ang: f64 = 20.0f64.to_radians();
    let m = Transform::new(120.0, 0.0, 0.0, -120.0, 0.0, 120.0)
        .then(&Transform::new(ang.cos(), ang.sin(), -ang.sin(), ang.cos(), 0.0, 0.0))
        .then(&Transform::translate(530.0, 460.0));
    canvas.draw_image(&img, &m, &clip, 1.0, false);

    // Row 3 ── thin-line grid, to check hairline consistency.
    label_box(&mut canvas, 680.0, 440.0, 200.0, 160.0, &clip);
    for i in 0..10 {
        let x = 690.0 + i as f64 * 20.0;
        let mut p = Path::new();
        p.move_to(x, 450.0);
        p.line_to(x, 590.0);
        let style = StrokeStyle { width: 0.0, ..Default::default() };
        canvas.stroke_path(&p, &id, &style, &Paint::Solid([0, 0, 0]), &clip, 1.0);
    }

    let img = image::RgbaImage::from_raw(w as u32, h as u32, canvas.data).expect("buffer");
    img.save(&out).expect("save png");
    println!("wrote {out}");
}
