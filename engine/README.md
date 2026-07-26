# anicca-engine

From-scratch document rendering engine for `anicca-doc-view`, written in Rust and
compiled to WebAssembly. Copyright (c) 2026 Anicca Code Studio. MIT licensed.

This engine is fully independent: it shares no code or binaries with any third
party. It exposes the same JS interface the viewer's worker expects (`Wasm`
class + `parseFontInfo`), so it drops in as `src/wasm/anicca-engine*`.

## Status

- **DOCX**: implemented natively: paragraphs, runs, bold/italic, font size,
  color, alignment, page geometry, pagination, and text rasterization via bundled
  DejaVu Sans. Resolves `styles.xml` (docDefaults + named styles + basedOn chains),
  paragraph spacing (before/after/line), and heading styles, which also populate
  the outline panel. Tables, inline and anchored images. Deterministic and self-contained.
- **XLSX**: implemented natively: multi-sheet workbooks, cell text and rich text
  (shared strings and inlineStr with per-run `<rPr>`), numeric and date values,
  cell styles via ECMA-376 `cellXfs`/`cellStyleXfs` resolution, bold/italic/color/font,
  all border styles, background fills, theme color resolution (OOXML theme + tint/shade),
  MDW-based column widths (per-font lookup table), VAlign (top/middle/bottom),
  `defaultRowHeight` and per-row `customHeight`, `no-wrap` clipping, two-pass
  page height measurement to prevent canvas under-allocation.
- **PDF**: implemented natively: a from-scratch parser (classic and stream
  xref, object streams, incremental updates, and a recovery scan for broken
  files) and content-stream interpreter drawing into an own 2D rasterizer
  (`raster.rs`: antialiased nonzero/even-odd fills, stroking with caps/joins/
  dashes, clip masks, affine image blits). Fonts: embedded TrueType, CFF
  (Type1C/CIDFontType0C), Type 1 (eexec + charstrings), Type 3, and Type0/CID,
  with metric-compatible substitutes for the standard 14. Colour: Device
  Gray/RGB/CMYK, ICCBased (via N/Alternate), Indexed, Separation/DeviceN, Lab.
  Images: 1/2/4/8/16 bpc, Flate/LZW/DCT/RunLength/ASCII filters, image and soft
  masks, colour-key masking. A selectable, searchable text layer is produced
  via `get_layout_page` (glyph positions in points, Unicode from `/ToUnicode`
  or the embedded font's cmap). Not yet: encryption, shadings/patterns
  (type 1-7 gradients), scan filters (CCITT/JBIG2), and annotation editing.
- **PPTX**: implemented natively via a dedicated `pptx/` module (own DrawingML
  parser and shape model, separate from the block-flow document model). Slides
  render as pages: autoshapes with preset and custom (`custGeom`) geometry,
  solid and gradient fills, outlines, pictures (PNG/JPEG), group transforms with
  rotation and flips, tables, and bar/line/pie/area charts plotted from scratch
  on `raster.rs`. Placeholder inheritance resolves slide layouts, slide masters,
  and the theme (color scheme, font scheme, list styles, layered backgrounds).
  Text is shaped with cosmic-text, and a selectable text layer is produced via
  `get_layout_page` (one frame per text box, plus table cells). Not yet:
  rotating a text box's glyphs, EMF/WMF/TIFF media, 3D/shadow/bevel effects, and
  SmartArt is best-effort (renders the cached diagram drawing when present).
- **Images**: implemented natively via a dedicated `image_fmt/` module of
  from-scratch decoders: PNG, JPEG (baseline and progressive), GIF (incl. Adam7
  interlace and palette + transparency), BMP, TIFF, ICO, TGA, PNM, and WebP
  (lossy VP8, lossless VP8L, and VP8X extended with an alpha plane). An image
  file opens as a single-page document sized from its pixel dimensions at its
  declared DPI (fallback 96), rasterized on `raster.rs`. Not yet: TIFF files
  that mix bit depths across samples.

## Layout

```
engine/
  Cargo.toml
  build.ps1          # compile + install bindings into ../src/wasm
  fonts/             # bundled DejaVu Sans (regular/bold/oblique/bold-oblique)
  src/
    lib.rs           # #[wasm_bindgen] Wasm API + parseFontInfo
    model.rs         # document model shared by DOCX and XLSX
    docx.rs          # OOXML unzip + WordprocessingML parser
    xlsx.rs          # OOXML unzip + SpreadsheetML parser
    render.rs        # cosmic-text layout + glyph rasterization to RGBA
    raster.rs        # own 2D rasterizer (paths, fills, strokes, clips, images)
    pdf/             # from-scratch PDF: xref, filters, content, fonts, text
    pptx/            # from-scratch PPTX: DrawingML, geometry, theme, slides, charts
    image_fmt/       # from-scratch image decoders (PNG/JPEG/GIF/BMP/TIFF/ICO/TGA/PNM/WebP)
```

## Prerequisites

```powershell
rustup default stable
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.100   # or use a prebuilt 0.2.100 binary
```

On Windows without the MSVC C++ toolchain, use the GNU host toolchain
(`rustup default stable-x86_64-pc-windows-gnu`), which bundles its own linker.

## Build

```powershell
./build.ps1
```

This compiles the crate, runs `wasm-bindgen`, and copies `anicca-engine.js`,
`anicca-engine_bg.wasm`, and `anicca-engine_bg.wasm.d.ts` into `../src/wasm`.

> The rich TypeScript declaration `src/wasm/anicca-engine.d.ts` is maintained by
> hand (it carries the full `Js*` data-shape types the viewer imports) and is
> intentionally **not** overwritten by the build script.

After building the engine, run `npm run build` at the package root.
