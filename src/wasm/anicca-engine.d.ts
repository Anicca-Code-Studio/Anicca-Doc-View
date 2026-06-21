/* tslint:disable */
/* eslint-disable */
export function parseFontInfo(data: Uint8Array): any;
export class Wasm {
  free(): void;
  page_count(document_id: string): number;
  get_outline(document_id: string): any;
  page_groups(document_id: string): any;
  pdf_compose(_compositions: any, _doc_ids: any): any;
  set_license(_license: string): any;
  authenticate(_document_id: string, _password: string): boolean;
  has_document(document_id: string): boolean;
  pdf_compress(_document_id: string): Uint8Array;
  all_page_info(document_id: string): any;
  get_font_usage(_document_id: string): any;
  license_status(): any;
  needs_password(_document_id: string): boolean;
  pdf_decompress(_document_id: string): Uint8Array;
  registerFonts(_fonts: any): void;
  document_format(document_id: string): string;
  get_layout_page(document_id: string, page_index: number): any;
  remove_document(document_id: string): boolean;
  render_page_gpu(document_id: string, page_index: number, width: number, height: number): Uint8Array;
  setup_telemetry(_distinct_id: string): void;
  disable_telemetry(): boolean;
  pdf_extract_fonts(_document_id: string): any;
  /**
   * Return the list of font family names declared in a document.
   * JS can use this to prefetch fonts before calling load().
   */
  getDeclaredFonts(bytes: Uint8Array): any;
  pdf_extract_images(_document_id: string, _convert: boolean): any;
  /**
   * Register a font from raw bytes. Call before load() for best results.
   * Accepts TTF/OTF/WOFF2 bytes fetched from any source (Google Fonts, custom URL, etc).
   */
  registerFontData(bytes: Uint8Array): void;
  enableGoogleFonts(): void;
  get_all_annotations(_document_id: string): any;
  render_page_to_rgba(document_id: string, page_index: number, width: number, height: number): Uint8Array;
  get_page_annotations(_document_id: string, _page_index: number): any;
  pdf_save_annotations(_document_id: string, _annotations_by_page: any): Uint8Array;
  pdf_split_by_outline(_document_id: string, _max_level: number, _split_mid_page: boolean): any;
  get_visibility_groups(_document_id: string): any;
  set_visibility_group_visible(_document_id: string, _group_id: string, _visible: boolean): boolean;
  constructor(_domain: string, _viewer_version: string);
  load(bytes: Uint8Array): string;
  init_gpu(): boolean;
  get_bytes(document_id: string): Uint8Array;
  page_info(document_id: string, page_index: number): any;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
  readonly memory: WebAssembly.Memory;
  readonly __wbg_wasm_free: (a: number, b: number) => void;
  readonly parseFontInfo: (a: number, b: number) => [number, number, number];
  readonly wasm_all_page_info: (a: number, b: number, c: number) => [number, number, number];
  readonly wasm_authenticate: (a: number, b: number, c: number, d: number, e: number) => number;
  readonly wasm_disable_telemetry: (a: number) => number;
  readonly wasm_document_format: (a: number, b: number, c: number) => [number, number];
  readonly wasm_enableGoogleFonts: (a: number) => void;
  readonly wasm_getDeclaredFonts: (a: number, b: number, c: number) => [number, number, number];
  readonly wasm_get_all_annotations: (a: number, b: number, c: number) => [number, number, number];
  readonly wasm_get_bytes: (a: number, b: number, c: number) => [number, number, number, number];
  readonly wasm_get_font_usage: (a: number, b: number, c: number) => [number, number, number];
  readonly wasm_get_layout_page: (a: number, b: number, c: number, d: number) => [number, number, number];
  readonly wasm_get_outline: (a: number, b: number, c: number) => [number, number, number];
  readonly wasm_get_page_annotations: (a: number, b: number, c: number, d: number) => [number, number, number];
  readonly wasm_has_document: (a: number, b: number, c: number) => number;
  readonly wasm_init_gpu: (a: number) => number;
  readonly wasm_license_status: (a: number) => [number, number, number];
  readonly wasm_load: (a: number, b: number, c: number) => [number, number, number, number];
  readonly wasm_needs_password: (a: number, b: number, c: number) => number;
  readonly wasm_new: (a: number, b: number, c: number, d: number) => number;
  readonly wasm_page_count: (a: number, b: number, c: number) => number;
  readonly wasm_page_groups: (a: number, b: number, c: number) => [number, number, number];
  readonly wasm_page_info: (a: number, b: number, c: number, d: number) => [number, number, number];
  readonly wasm_pdf_compose: (a: number, b: any, c: any) => [number, number, number];
  readonly wasm_pdf_compress: (a: number, b: number, c: number) => [number, number, number, number];
  readonly wasm_pdf_extract_fonts: (a: number, b: number, c: number) => [number, number, number];
  readonly wasm_pdf_extract_images: (a: number, b: number, c: number, d: number) => [number, number, number];
  readonly wasm_pdf_save_annotations: (a: number, b: number, c: number, d: any) => [number, number, number, number];
  readonly wasm_pdf_split_by_outline: (a: number, b: number, c: number, d: number, e: number) => [number, number, number];
  readonly wasm_registerFontData: (a: number, b: number, c: number) => void;
  readonly wasm_registerFonts: (a: number, b: any) => void;
  readonly wasm_remove_document: (a: number, b: number, c: number) => number;
  readonly wasm_render_page_gpu: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
  readonly wasm_set_license: (a: number, b: number, c: number) => [number, number, number];
  readonly wasm_set_visibility_group_visible: (a: number, b: number, c: number, d: number, e: number, f: number) => number;
  readonly wasm_setup_telemetry: (a: number, b: number, c: number) => void;
  readonly wasm_pdf_decompress: (a: number, b: number, c: number) => [number, number, number, number];
  readonly wasm_render_page_to_rgba: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
  readonly wasm_get_visibility_groups: (a: number, b: number, c: number) => [number, number, number];
  readonly __wbindgen_malloc: (a: number, b: number) => number;
  readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
  readonly __wbindgen_export_2: WebAssembly.Table;
  readonly __externref_table_dealloc: (a: number) => void;
  readonly __wbindgen_free: (a: number, b: number, c: number) => void;
  readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;
/**
* Instantiates the given `module`, which can either be bytes or
* a precompiled `WebAssembly.Module`.
*
* @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
*
* @returns {InitOutput}
*/
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
* If `module_or_path` is {RequestInfo} or {URL}, makes a request and
* for everything else, calls `WebAssembly.instantiate` directly.
*
* @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
*
* @returns {Promise<InitOutput>}
*/
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
