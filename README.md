# anicca-doc-view

Universal document viewer for the web by Anicca Code Studio. Framework-agnostic, powered by `anicca-engine`, an independent from-scratch WebAssembly rendering engine (Rust). No third-party engine code or binaries.

## Supported formats

- Word (`.docx`) — **implemented** (text, bold/italic, size, color, alignment, pagination, styles.xml resolution, heading outline, paragraph spacing, tables, images)
- Excel (`.xlsx`) — **implemented** (multi-sheet workbooks, cell text and styles, bold/italic/color/font, borders, background fills, MDW column widths, VAlign, row heights, theme colors, shared strings, inlineStr rich text)

Roadmap (not yet available):

- Images (`.png`, `.jpg`, …)
- PDF (`.pdf`)
- PowerPoint (`.pptx`)

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
covers DOCX and XLSX.

## Theming

All styles are scoped under `.adv-viewer-root` and driven by `--adv-*` CSS custom properties. Toggle dark mode with the `adv-viewer-dark` class or the `theme` viewer option.

## Support & Donations

This project is built and maintained in my free time after my day job, similar to how Laravel was first developed. Everything is completely free and open source. Your support really helps me keep developing, maintaining, and improving these tools for the developer community.

On a personal note, I'm also saving up to finally meet my girlfriend in Kyrgyzstan. We've been in a long-distance relationship between Indonesia and Kyrgyzstan for quite a while. Every donation brings me one step closer to closing this distance and building our future together.

Thank you so much. Your support means a lot to me, both for the project and for this part of my life. 🙏

| Platform | Link |
|----------|------|
| PayPal | [paypal.me/AdjieDev](https://paypal.me/AdjieDev) |
| Saweria | [saweria.co/RikuKzry](https://saweria.co/RikuKzry) |
| Ko-fi | [ko-fi.com/aniccacodestudio](https://ko-fi.com/aniccacodestudio) |
| Trakteer | [trakteer.id/adjie.dev](https://trakteer.id/adjie.dev) |

## License

MIT License, Copyright (c) 2026 Anicca Code Studio. See [LICENSE](./LICENSE).

The WebAssembly engine in `src/wasm/` is built from the MIT-licensed `anicca-engine` Rust source in [`engine/`](./engine). Bundled fonts: DejaVu Sans (permissive license).