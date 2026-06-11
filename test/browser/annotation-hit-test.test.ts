/**
 * Regression test for a customer report against 0.6.41: with
 * `pointer-events: painted` set inline on the annotation <svg> root, the
 * full SVG (100% × 100% of the page slot) acted as a hit target — so
 * `annotation:hover` fired on every pointer move across the page, not just
 * over the painted shape.
 *
 * The fix moves hit-testing to package CSS — `<svg>` root gets
 * `pointer-events: none`, painted children get `pointer-events:
 * visiblePainted`. This test pins that behavior via
 * `document.elementFromPoint`:
 *
 *   - over the painted rect → hit must land on the annotation SVG
 *   - over empty SVG space  → hit must pass through to the layer below
 */
import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { AniccaClient } from "../../src/index.js";
import type { AniccaViewer } from "../../src/index.js";
import sampleUrl from "../fixtures/sample.pdf?url";

let container: HTMLDivElement;
let client: AniccaClient | null = null;
let viewer: AniccaViewer | null = null;

beforeEach(() => {
    container = document.createElement("div");
    container.style.width = "800px";
    container.style.height = "600px";
    document.body.appendChild(container);
});

afterEach(() => {
    viewer?.destroy();
    viewer = null;
    client?.destroy();
    client = null;
    container.remove();
});

async function waitFor<T>(fn: () => T | null | undefined, timeoutMs = 5000, intervalMs = 25): Promise<T> {
    const start = performance.now();
    while (performance.now() - start < timeoutMs) {
        const result = fn();
        if (result !== null && result !== undefined && result !== false) return result as T;
        await new Promise((r) => setTimeout(r, intervalMs));
    }
    throw new Error("waitFor: timed out");
}

async function loadFixture(): Promise<void> {
    client = await AniccaClient.create({ googleFonts: false, disableUpdateCheck: true });
    viewer = await client.createViewer({ container });
    const loaded = new Promise<void>((resolve) => viewer!.on("document:load", () => resolve()));
    await viewer.load(sampleUrl);
    await loaded;
}

describe("annotation SVG hit-testing", () => {
    it("does not register hits on empty SVG space outside the painted shape", async () => {
        await loadFixture();

        const pageSlot = await waitFor(() => container.querySelector<HTMLElement>('[data-page="1"]'));
        await waitFor(() => pageSlot.querySelector("canvas") ?? null);

        // Small filled square anchored near the top-left of the page. Filled
        // (interiorColor set) so the rect is painted across its bbox, not
        // only along its stroke — eliminates ambiguity about which child
        // pixels count as "painted" for hit-testing.
        await viewer!.addPageAnnotation(0, {
            type: "square",
            name: "hit-test-square",
            bounds: { x: 50, y: 50, width: 60, height: 40 },
            color: { r: 1, g: 0, b: 0 },
            interiorColor: { r: 1, g: 1, b: 0 },
            borderWidth: 2,
        });

        const annotationSvg = await waitFor(() => pageSlot.querySelector<SVGSVGElement>("svg[data-annotation-index]"));
        const rectEl = annotationSvg.querySelector("rect");
        expect(rectEl).not.toBeNull();

        const svgRect = annotationSvg.getBoundingClientRect();
        const shapeRect = rectEl!.getBoundingClientRect();
        const viewportWidth = document.documentElement.clientWidth;
        const viewportHeight = document.documentElement.clientHeight;

        // SVG covers the page slot; painted shape sits in a small corner.
        expect(svgRect.width).toBeGreaterThan(shapeRect.width + 50);
        expect(svgRect.height).toBeGreaterThan(shapeRect.height + 50);

        // Empty point: inside the SVG bbox, clearly outside the painted shape,
        // and inside the browser viewport so elementFromPoint can hit-test it.
        const emptyX = Math.min(shapeRect.right + 60, svgRect.right - 5, viewportWidth - 5);
        const emptyY = Math.min(shapeRect.bottom + 60, svgRect.bottom - 5, viewportHeight - 5);
        expect(emptyX).toBeGreaterThan(shapeRect.right + 10);
        expect(emptyY).toBeGreaterThan(shapeRect.bottom + 10);
        expect(emptyX).toBeLessThan(svgRect.right);
        expect(emptyY).toBeLessThan(svgRect.bottom);

        const hitOnShape = document.elementFromPoint(
            shapeRect.left + shapeRect.width / 2,
            shapeRect.top + shapeRect.height / 2,
        );
        const hitEmpty = document.elementFromPoint(emptyX, emptyY);

        expect(hitOnShape).not.toBeNull();
        expect(hitEmpty).not.toBeNull();

        // Inside the painted shape: hit must reach the annotation. If this
        // fails, annotation:hover/click would never fire on a real pointer.
        expect(hitOnShape!.closest("[data-annotation-index]")).toBe(annotationSvg);

        // Outside the painted shape (empty SVG space): hit must NOT land on
        // the annotation. Otherwise every move anywhere on the page would
        // fire annotation:hover (the 0.6.41 regression).
        expect(hitEmpty!.closest("[data-annotation-index]")).toBeNull();
    });
});
