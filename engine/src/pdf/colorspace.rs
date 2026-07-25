//! Colour spaces (ISO 32000-1 clause 8.6).
//!
//! Every space reduces to sRGB-ish device RGB, which is what the canvas holds.
//! Spaces that need a tint transform function (`Separation`, `DeviceN`) fall
//! back to a subtractive approximation until the function evaluator lands.

use std::rc::Rc;

use super::object::Obj;
use super::xref::PdfFile;

#[derive(Clone, Debug)]
pub enum ColorSpace {
    DeviceGray,
    DeviceRGB,
    DeviceCMYK,
    /// CIE L*a*b* with a white point.
    Lab { wp: [f32; 3], range: [f32; 4] },
    Indexed {
        base: Rc<ColorSpace>,
        hival: usize,
        lookup: Rc<Vec<u8>>,
    },
    /// `Separation` / `DeviceN` without an evaluated tint transform.
    /// `n` is the number of tint components.
    Separation { n: usize, alt: Rc<ColorSpace> },
    /// Pattern space; the colour comes from the pattern, not from components.
    Pattern { base: Option<Rc<ColorSpace>> },
}

impl ColorSpace {
    pub fn n_comps(&self) -> usize {
        match self {
            ColorSpace::DeviceGray => 1,
            ColorSpace::DeviceRGB => 3,
            ColorSpace::DeviceCMYK => 4,
            ColorSpace::Lab { .. } => 3,
            ColorSpace::Indexed { .. } => 1,
            ColorSpace::Separation { n, .. } => *n,
            ColorSpace::Pattern { .. } => 1,
        }
    }

    /// Initial colour when this space is selected by `cs`/`CS`: black in every
    /// device space, index 0 for Indexed, tint 1 for Separation.
    pub fn initial_color(&self) -> Vec<f32> {
        match self {
            ColorSpace::DeviceRGB => vec![0.0, 0.0, 0.0],
            ColorSpace::DeviceCMYK => vec![0.0, 0.0, 0.0, 1.0],
            ColorSpace::Lab { .. } => vec![0.0, 0.0, 0.0],
            ColorSpace::Separation { n, .. } => vec![1.0; *n],
            _ => vec![0.0; self.n_comps()],
        }
    }

    pub fn to_rgb(&self, c: &[f32]) -> [u8; 3] {
        match self {
            ColorSpace::DeviceGray => {
                let g = to_byte(c.first().copied().unwrap_or(0.0));
                [g, g, g]
            }
            ColorSpace::DeviceRGB => [
                to_byte(c.first().copied().unwrap_or(0.0)),
                to_byte(c.get(1).copied().unwrap_or(0.0)),
                to_byte(c.get(2).copied().unwrap_or(0.0)),
            ],
            ColorSpace::DeviceCMYK => cmyk_to_rgb(
                c.first().copied().unwrap_or(0.0),
                c.get(1).copied().unwrap_or(0.0),
                c.get(2).copied().unwrap_or(0.0),
                c.get(3).copied().unwrap_or(0.0),
            ),
            ColorSpace::Lab { wp, .. } => lab_to_rgb(
                c.first().copied().unwrap_or(0.0),
                c.get(1).copied().unwrap_or(0.0),
                c.get(2).copied().unwrap_or(0.0),
                *wp,
            ),
            ColorSpace::Indexed { base, hival, lookup } => {
                let idx = (c.first().copied().unwrap_or(0.0).round().max(0.0) as usize).min(*hival);
                let n = base.n_comps();
                let start = idx * n;
                let mut comps = Vec::with_capacity(n);
                for i in 0..n {
                    let byte = lookup.get(start + i).copied().unwrap_or(0);
                    // Lab components in a lookup table use the space's ranges;
                    // every other base is a plain 0..1 fraction.
                    comps.push(match base.as_ref() {
                        ColorSpace::Lab { range, .. } => {
                            let v = byte as f32 / 255.0;
                            match i {
                                0 => v * 100.0,
                                1 => range[0] + v * (range[1] - range[0]),
                                _ => range[2] + v * (range[3] - range[2]),
                            }
                        }
                        _ => byte as f32 / 255.0,
                    });
                }
                base.to_rgb(&comps)
            }
            // Approximation: treat the tint as ink coverage. Correct for the
            // common single-ink case and for /All; replaced by the real tint
            // transform once functions are evaluated.
            ColorSpace::Separation { alt, .. } => {
                let tint = c.iter().copied().fold(0.0f32, f32::max).clamp(0.0, 1.0);
                match alt.as_ref() {
                    ColorSpace::DeviceCMYK => cmyk_to_rgb(0.0, 0.0, 0.0, tint),
                    _ => {
                        let g = to_byte(1.0 - tint);
                        [g, g, g]
                    }
                }
            }
            ColorSpace::Pattern { .. } => [0, 0, 0],
        }
    }

    /// Component range for image samples of this space, used to scale integer
    /// samples before conversion.
    pub fn default_decode(&self, bpc: usize) -> Vec<(f32, f32)> {
        match self {
            ColorSpace::Indexed { .. } => {
                let max = ((1usize << bpc) - 1) as f32;
                vec![(0.0, max)]
            }
            ColorSpace::Lab { range, .. } => vec![
                (0.0, 100.0),
                (range[0], range[1]),
                (range[2], range[3]),
            ],
            _ => vec![(0.0, 1.0); self.n_comps()],
        }
    }
}

#[inline]
fn to_byte(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

pub fn cmyk_to_rgb(c: f32, m: f32, y: f32, k: f32) -> [u8; 3] {
    let c = c.clamp(0.0, 1.0);
    let m = m.clamp(0.0, 1.0);
    let y = y.clamp(0.0, 1.0);
    let k = k.clamp(0.0, 1.0);
    [
        to_byte((1.0 - c) * (1.0 - k)),
        to_byte((1.0 - m) * (1.0 - k)),
        to_byte((1.0 - y) * (1.0 - k)),
    ]
}

fn lab_to_rgb(l: f32, a: f32, b: f32, wp: [f32; 3]) -> [u8; 3] {
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let g = |t: f32| {
        if t > 6.0 / 29.0 {
            t * t * t
        } else {
            3.0 * (6.0f32 / 29.0).powi(2) * (t - 4.0 / 29.0)
        }
    };
    let (x, y, z) = (wp[0] * g(fx), wp[1] * g(fy), wp[2] * g(fz));
    // XYZ (D50-ish) to linear sRGB.
    let r = 3.2406 * x - 1.5372 * y - 0.4986 * z;
    let gg = -0.9689 * x + 1.8758 * y + 0.0415 * z;
    let bb = 0.0557 * x - 0.2040 * y + 1.0570 * z;
    let gamma = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.0031308 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    };
    [to_byte(gamma(r)), to_byte(gamma(gg)), to_byte(gamma(bb))]
}

/// Resolves a colour space object: a name, or an array like
/// `[/ICCBased 5 0 R]` / `[/Indexed base hival lookup]`.
pub fn parse(file: &PdfFile, obj: &Obj, resources: &Obj, depth: usize) -> ColorSpace {
    if depth > 8 {
        return ColorSpace::DeviceGray;
    }
    let obj = file.resolve(obj);

    if let Some(name) = obj.as_name() {
        return match name {
            "DeviceGray" | "G" | "CalGray" => ColorSpace::DeviceGray,
            "DeviceRGB" | "RGB" | "CalRGB" => ColorSpace::DeviceRGB,
            "DeviceCMYK" | "CMYK" => ColorSpace::DeviceCMYK,
            "Pattern" => ColorSpace::Pattern { base: None },
            // Anything else is a name defined in /Resources /ColorSpace.
            other => lookup_named(file, other, resources, depth),
        };
    }

    let arr = match obj.as_array() {
        Some(a) if !a.is_empty() => a,
        _ => return ColorSpace::DeviceGray,
    };
    let family = file.resolve(&arr[0]);
    let family = family.as_name().unwrap_or("");

    match family {
        "DeviceGray" | "G" => ColorSpace::DeviceGray,
        "DeviceRGB" | "RGB" => ColorSpace::DeviceRGB,
        "DeviceCMYK" | "CMYK" => ColorSpace::DeviceCMYK,
        // Calibrated device spaces are rendered as their device equivalents.
        "CalGray" => ColorSpace::DeviceGray,
        "CalRGB" => ColorSpace::DeviceRGB,
        "Lab" => {
            let d = arr.get(1).map(|o| file.resolve(o));
            let wp = d
                .as_ref()
                .and_then(|o| o.get("WhitePoint").map(|w| file.resolve(w)))
                .and_then(|w| {
                    let a = w.as_array()?;
                    Some([
                        a.first()?.as_f32()?,
                        a.get(1)?.as_f32()?,
                        a.get(2)?.as_f32()?,
                    ])
                })
                .unwrap_or([0.9505, 1.0, 1.089]);
            let range = d
                .as_ref()
                .and_then(|o| o.get("Range").map(|r| file.resolve(r)))
                .and_then(|r| {
                    let a = r.as_array()?;
                    Some([
                        a.first()?.as_f32()?,
                        a.get(1)?.as_f32()?,
                        a.get(2)?.as_f32()?,
                        a.get(3)?.as_f32()?,
                    ])
                })
                .unwrap_or([-100.0, 100.0, -100.0, 100.0]);
            ColorSpace::Lab { wp, range }
        }
        "ICCBased" => {
            // The profile itself is not interpreted; /N selects the device
            // space with matching component count, and /Alternate wins if set.
            let stream = arr.get(1).map(|o| file.resolve(o));
            if let Some(alt) = stream
                .as_ref()
                .and_then(|s| s.get("Alternate").cloned())
            {
                return parse(file, &alt, resources, depth + 1);
            }
            let n = stream
                .as_ref()
                .and_then(|s| s.get("N"))
                .and_then(|o| o.as_usize())
                .unwrap_or(3);
            match n {
                1 => ColorSpace::DeviceGray,
                4 => ColorSpace::DeviceCMYK,
                _ => ColorSpace::DeviceRGB,
            }
        }
        "Indexed" | "I" => {
            let base = arr
                .get(1)
                .map(|o| parse(file, o, resources, depth + 1))
                .unwrap_or(ColorSpace::DeviceRGB);
            let hival = arr.get(2).and_then(|o| file.resolve(o).as_usize()).unwrap_or(255);
            let lookup = match arr.get(3).map(|o| file.resolve(o)) {
                Some(Obj::Str(s)) => s.as_ref().clone(),
                Some(Obj::Stream(s)) => file.stream_data_of(&s).unwrap_or_default(),
                _ => Vec::new(),
            };
            ColorSpace::Indexed {
                base: Rc::new(base),
                hival: hival.min(255),
                lookup: Rc::new(lookup),
            }
        }
        "Separation" => {
            let alt = arr
                .get(2)
                .map(|o| parse(file, o, resources, depth + 1))
                .unwrap_or(ColorSpace::DeviceGray);
            ColorSpace::Separation { n: 1, alt: Rc::new(alt) }
        }
        "DeviceN" => {
            let n = arr
                .get(1)
                .and_then(|o| file.resolve(o).as_array().map(|a| a.len()))
                .unwrap_or(1);
            let alt = arr
                .get(2)
                .map(|o| parse(file, o, resources, depth + 1))
                .unwrap_or(ColorSpace::DeviceGray);
            ColorSpace::Separation { n: n.max(1), alt: Rc::new(alt) }
        }
        "Pattern" => {
            let base = arr.get(1).map(|o| Rc::new(parse(file, o, resources, depth + 1)));
            ColorSpace::Pattern { base }
        }
        _ => ColorSpace::DeviceGray,
    }
}

fn lookup_named(file: &PdfFile, name: &str, resources: &Obj, depth: usize) -> ColorSpace {
    let table = match file.oget(resources, "ColorSpace") {
        Some(t) => t,
        None => return ColorSpace::DeviceGray,
    };
    match table.as_dict().and_then(|d| d.get(name)) {
        Some(o) => {
            let o = o.clone();
            parse(file, &o, resources, depth + 1)
        }
        None => ColorSpace::DeviceGray,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmyk_black_and_white() {
        assert_eq!(cmyk_to_rgb(0.0, 0.0, 0.0, 1.0), [0, 0, 0]);
        assert_eq!(cmyk_to_rgb(0.0, 0.0, 0.0, 0.0), [255, 255, 255]);
        assert_eq!(cmyk_to_rgb(1.0, 0.0, 0.0, 0.0), [0, 255, 255]);
    }

    #[test]
    fn gray_and_rgb() {
        assert_eq!(ColorSpace::DeviceGray.to_rgb(&[0.5]), [128, 128, 128]);
        assert_eq!(ColorSpace::DeviceRGB.to_rgb(&[1.0, 0.0, 0.5]), [255, 0, 128]);
    }

    #[test]
    fn indexed_picks_from_lookup() {
        let cs = ColorSpace::Indexed {
            base: Rc::new(ColorSpace::DeviceRGB),
            hival: 1,
            lookup: Rc::new(vec![255, 0, 0, 0, 0, 255]),
        };
        assert_eq!(cs.to_rgb(&[0.0]), [255, 0, 0]);
        assert_eq!(cs.to_rgb(&[1.0]), [0, 0, 255]);
        // Out-of-range indices clamp instead of panicking.
        assert_eq!(cs.to_rgb(&[9.0]), [0, 0, 255]);
    }

    #[test]
    fn lab_white_is_white() {
        let cs = ColorSpace::Lab { wp: [0.9505, 1.0, 1.089], range: [-100.0, 100.0, -100.0, 100.0] };
        let rgb = cs.to_rgb(&[100.0, 0.0, 0.0]);
        assert!(rgb.iter().all(|&c| c > 250), "L*=100 should be white, got {rgb:?}");
    }
}
