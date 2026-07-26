//! Standalone image files as a first-class document format.
//!
//! Every decoder here is written from scratch against its format spec and reuses
//! only the engine's own primitives (`pdf::filter` for inflate/predictor,
//! `raster::Bitmap`/`Canvas` for pixels). No third-party image codec is used.
//!
//! An image opens as a one-page document sized from its pixel dimensions at the
//! file's declared resolution (or 96 DPI when none is given).

pub mod bmp;
pub mod gif;
pub mod ico;
pub mod jpeg;
pub mod png;
pub mod pnm;
pub mod tga;
pub mod tiff;
pub mod webp;

use crate::raster::Bitmap;

/// Points per inch a screen image maps to when the file gives no resolution.
const DEFAULT_DPI: f32 = 96.0;

/// A decoded standalone image, presented to the viewer as one page.
pub struct ImageDocument {
    pub bytes: Vec<u8>,
    pub bitmap: Bitmap,
    pub w_pt: f32,
    pub h_pt: f32,
    pub format: String,
}

impl ImageDocument {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// True when `bytes` carries a recognised image signature.
pub fn is_image(bytes: &[u8]) -> bool {
    png::is_png(bytes)
        || bmp::is_bmp(bytes)
        || gif::is_gif(bytes)
        || jpeg::is_jpeg(bytes)
        || tiff::is_tiff(bytes)
        || webp::is_webp(bytes)
        || ico::is_ico(bytes)
        || pnm::is_pnm(bytes)
        || tga::is_tga(bytes)
}

/// Decodes any supported image to RGBA plus a physical DPI hint.
/// Returns `(bitmap, format_name, dpi)`.
fn decode(bytes: &[u8]) -> Option<(Bitmap, &'static str, f32)> {
    // Each candidate is tried when its signature matches; if its decoder fails
    // (e.g. a signature collision like TGA's cursor-type byte vs. the CUR magic),
    // fall through to the next candidate rather than giving up.
    if png::is_png(bytes) {
        if let Some(d) = png::decode(bytes) {
            return Some((d.bitmap, "png", d.dpi.unwrap_or(DEFAULT_DPI)));
        }
    }
    if bmp::is_bmp(bytes) {
        if let Some(b) = bmp::decode(bytes) {
            return Some((b, "bmp", DEFAULT_DPI));
        }
    }
    if gif::is_gif(bytes) {
        if let Some(b) = gif::decode(bytes) {
            return Some((b, "gif", DEFAULT_DPI));
        }
    }
    if jpeg::is_jpeg(bytes) {
        if let Some(d) = jpeg::decode(bytes) {
            return Some((d.bitmap, "jpeg", d.dpi.unwrap_or(DEFAULT_DPI)));
        }
    }
    if webp::is_webp(bytes) {
        if let Some(b) = webp::decode(bytes) {
            return Some((b, "webp", DEFAULT_DPI));
        }
    }
    if tiff::is_tiff(bytes) {
        if let Some(b) = tiff::decode(bytes) {
            return Some((b, "tiff", DEFAULT_DPI));
        }
    }
    if ico::is_ico(bytes) {
        if let Some(b) = ico::decode(bytes) {
            return Some((b, "ico", DEFAULT_DPI));
        }
    }
    if pnm::is_pnm(bytes) {
        if let Some(b) = pnm::decode(bytes) {
            return Some((b, "pnm", DEFAULT_DPI));
        }
    }
    if tga::is_tga(bytes) {
        if let Some(b) = tga::decode(bytes) {
            return Some((b, "tga", DEFAULT_DPI));
        }
    }
    None
}

/// Parses image bytes into a one-page document.
pub fn parse(bytes: &[u8]) -> Result<ImageDocument, String> {
    let (bitmap, format, dpi) = match decode(bytes) {
        Some(v) => v,
        None => return Err("anicca-engine: could not decode this image".to_string()),
    };
    if bitmap.w == 0 || bitmap.h == 0 {
        return Err("anicca-engine: image has zero dimensions".to_string());
    }
    let dpi = if dpi > 1.0 { dpi } else { DEFAULT_DPI };
    let w_pt = bitmap.w as f32 * 72.0 / dpi;
    let h_pt = bitmap.h as f32 * 72.0 / dpi;
    Ok(ImageDocument {
        bytes: bytes.to_vec(),
        bitmap,
        w_pt,
        h_pt,
        format: format.to_string(),
    })
}
