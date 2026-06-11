/*!
 * anicca-doc-view
 * Universal document viewer for the web.
 *
 * Copyright (c) 2026 Anicca Code Studio
 * Licensed under the MIT License. See LICENSE for details.
 */

// Main classes
export { AniccaClient } from "./AniccaClient.js";
export type {
    ClientOptions,
    ViewerOptions,
    DocumentSource,
    LicenseInfo,
    Pick,
    Composition,
    ComposePick,
    FontEntry,
    FontInfo,
    CustomPageOverlayRenderer,
} from "./AniccaClient.js";

export { AniccaViewer } from "./AniccaViewer.js";
export type {
    RenderOptions,
    RenderedPage,
    DocumentMetadata,
    OutlineItem,
    Destination,
    DestinationDisplay,
    ScrollAlignment,
    Annotation,
    ViewerEventMap,
    LoadProgress,
    UIComponent,
} from "./AniccaViewer.js";

// Print dialog types
export type { PrintDialogResult, PrintPageRange, PrintQuality } from "./ui/viewer/components/PrintDialog.js";

// i18n types
export type { TranslationKeys } from "./ui/viewer/i18n/index.js";
export type { I18n } from "./ui/viewer/i18n/index.js";
export { createI18n } from "./ui/viewer/i18n/index.js";

// View mode and panel types
export type {
    ViewMode,
    ScrollMode,
    LayoutMode,
    ZoomMode,
    PageRotation,
    SpacingMode,
    PanelTab,
    LeftPanelTab,
    RightPanelTab,
    ThemeMode,
    SearchMatch,
    ActiveTool,
    ToolKind,
    SubTool,
    AnnotateSubTool,
    MarkupSubTool,
} from "./ui/viewer/state.js";

// Worker internals (types only, for advanced usage)
export type {
    PageInfo,
    WorkerRequest,
    WorkerResponse,
    RenderCacheStats,
    RenderCacheBucket,
} from "./worker/index.js";

// Font usage types
export type { FontSource, ResolvedFontInfo, FontUsageEntry } from "./worker/index.js";

// Performance tracking
export type {
    IPerformanceCounter,
    PerformanceEventType,
    PerformanceEventContext,
    PerformanceLogEntry,
    PerformanceLogCallback,
    PerformanceEventStats,
    PerformanceCounterSummary,
} from "./performance/index.js";
