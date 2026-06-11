# anicca-doc-view

Universal document viewer for the web by Anicca Code Studio. Framework-agnostic, powered by `anicca-engine`, an independent from-scratch WebAssembly rendering engine (Rust). No third-party engine code or binaries.

## Supported formats

- Word (`.docx`) — **implemented** (text, bold/italic, size, color, alignment, pagination, styles.xml resolution, heading outline, paragraph spacing)

Roadmap (engine in progress, not yet available):

- Images (`.png`, `.jpg`, …)
- PDF (`.pdf`)
- PowerPoint (`.pptx`)
- Excel (`.xlsx`)

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

The viewer shell carries over the full feature set (search, annotations, panels,
transitions); availability of each depends on engine support, which currently
covers DOCX.

## Theming

All styles are scoped under `.adv-viewer-root` and driven by `--adv-*` CSS custom properties. Toggle dark mode with the `adv-viewer-dark` class or the `theme` viewer option.

## License

MIT License, Copyright (c) 2026 Anicca Code Studio. See [LICENSE](./LICENSE).

The WebAssembly engine in `src/wasm/` is built from the MIT-licensed `anicca-engine` Rust source in [`engine/`](./engine). Bundled fonts: DejaVu Sans (permissive license).
