# anicca-doc-view

Universal document viewer for the web by Anicca Code Studio. Framework-agnostic, powered by `anicca-engine`, an independent from-scratch WebAssembly rendering engine (Rust). No third-party engine code or binaries.

## Supported formats

- PDF (`.pdf`) is **implemented** (vector graphics, embedded TrueType/CFF/Type1/Type3/CID fonts with standard-14 substitution, images with soft and stencil masks, device and ICC/Indexed/Separation color, page rotation, outline/bookmarks, and a selectable, searchable text layer)
- Word (`.docx`) is **implemented** (text, bold/italic, size, color, alignment, pagination, styles.xml resolution, heading outline, paragraph spacing)
- Excel (`.xlsx`) is **implemented** (multi-sheet workbooks, cell formatting, borders, fill colors, column and row sizing)
- PowerPoint (`.pptx`) is **implemented** (slides as pages, autoshapes with preset and custom geometry, solid and gradient fills, images, tables, bar/line/pie/area charts, placeholder inheritance from slide layouts and masters plus theme colors and fonts, and a selectable text layer)
- Images are **implemented** (`.png`, `.jpg`/`.jpeg`, `.gif`, `.bmp`, `.tiff`, `.ico`, `.tga`, `.pnm`, `.webp` incl. lossy and lossless with alpha), each decoded by a from-scratch decoder and opened as a single-page document sized from its pixel dimensions and declared DPI

Roadmap (engine in progress, not yet available):

- PDF extras: encryption, gradient shadings/patterns, and annotation editing

The rendering engine lives in [`engine/`](./engine) and is built separately; see [`engine/README.md`](./engine/README.md).

## Installation

```bash
npm install anicca-doc-view
```

## Quick start

```ts
import { AniccaClient } from "anicca-doc-view";

const client = await AniccaClient.create();

const viewer = await client.createViewer({
    container: document.getElementById("viewer")!,
});

await viewer.load({ url: "/documents/example.docx" });
```

## Features

- Rendering via the `anicca-engine` WASM engine running in a Web Worker
- Viewer UI: toolbar, thumbnails, outline, panels, zoom, print dialog
- Light and dark themes via CSS custom properties (`--adv-*`)
- 12 built-in UI locales
- Zero framework dependencies
- No external network calls (no permit/telemetry servers)

The viewer shell carries over the full feature set (search, text selection,
panels, transitions); engine support covers PDF, DOCX, XLSX, PPTX, and images.
Search and text selection run on the glyph text layer produced for PDF, DOCX,
XLSX, and PPTX; image documents render as pages but carry no text layer.

## Theming

All styles are scoped under `.adv-viewer-root` and driven by `--adv-*` CSS custom properties. Toggle dark mode with the `adv-viewer-dark` class or the `theme` viewer option.

## Support & Donations

This project is built and maintained in my free time after my day job, similar to how Laravel was first developed. Everything is completely free and open source. Your support really helps me keep developing, maintaining, and improving these tools for the developer community.

Thank you so much. Your support means a lot and keeps this project moving forward. 🙏

| Platform | Link |
|----------|------|
| PayPal | [paypal.me/AdjieDev](https://paypal.me/AdjieDev) |
| Saweria | [saweria.co/RikuKzry](https://saweria.co/RikuKzry) |
| Ko-fi | [ko-fi.com/aniccacodestudio](https://ko-fi.com/aniccacodestudio) |
| Trakteer | [trakteer.id/adjie.dev](https://trakteer.id/adjie.dev) |

## License

MIT License, Copyright (c) 2026 Anicca Code Studio. See [LICENSE](./LICENSE).

The WebAssembly engine in `src/wasm/` is built from the MIT-licensed `anicca-engine` Rust source in [`engine/`](./engine). Bundled fonts: DejaVu Sans (permissive license).
