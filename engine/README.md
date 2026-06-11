# anicca-engine

From-scratch document rendering engine for `anicca-doc-view`, written in Rust and
compiled to WebAssembly. Copyright (c) 2026 Anicca Code Studio. MIT licensed.

This engine is fully independent: it shares no code or binaries with any third
party. It exposes the same JS interface the viewer's worker expects (`Wasm`
class + `parseFontInfo`), so it drops in as `src/wasm/anicca-engine*`.

## Status

- **DOCX**: implemented natively — paragraphs, runs, bold/italic, font size,
  color, alignment, page geometry, pagination, and text rasterization via bundled
  DejaVu Sans. Resolves `styles.xml` (docDefaults + named styles + basedOn chains),
  paragraph spacing (before/after/line), and heading styles, which also populate
  the outline panel. Deterministic and self-contained.
- **PDF / PPTX / XLSX / images**: not yet implemented; `load` returns an explicit
  error. These are future milestones.

## Layout

```
engine/
  Cargo.toml
  build.ps1          # compile + install bindings into ../src/wasm
  fonts/             # bundled DejaVu Sans (regular/bold/oblique/bold-oblique)
  src/
    lib.rs           # #[wasm_bindgen] Wasm API + parseFontInfo
    model.rs         # document model (paragraphs, runs, styles)
    docx.rs          # OOXML unzip + WordprocessingML parser
    render.rs        # cosmic-text layout + glyph rasterization to RGBA
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
