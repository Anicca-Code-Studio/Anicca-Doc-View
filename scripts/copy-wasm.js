/**
 * Cross-platform replacement for `rm -rf dist/src/wasm && cp -r src/wasm dist/src/wasm`.
 */

import { rmSync, cpSync } from "fs";
import { fileURLToPath } from "url";
import { dirname, join } from "path";

const __dirname = dirname(fileURLToPath(import.meta.url));
const src = join(__dirname, "../src/wasm");
const dest = join(__dirname, "../dist/src/wasm");

rmSync(dest, { recursive: true, force: true });
cpSync(src, dest, { recursive: true });
console.log("Copied src/wasm to dist/src/wasm");
