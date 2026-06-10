import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { AniccaClient } from "../../src/index.js";
import type { AniccaViewer, ViewerEventMap } from "../../src/index.js";
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

describe("AniccaViewer annotation pointer events", () => {
    it("fires annotation:hover and annotation:click with correct payload", async () => {
        await loadFixture();

        // Wait until the first page slot is mounted and has been rendered (canvas
        // populated). Adding annotations before the spread component for the
        // target page exists means the renderer skips them on its first pass.
        const pageSlot = await waitFor(() => container.querySelector<HTMLElement>('[data-page="1"]'));
        await waitFor(() => pageSlot.querySelector("canvas") ?? null);

        const annotName = "adv-test-square-1";
        await viewer!.addPageAnnotation(0, {
            type: "square",
            name: annotName,
            bounds: { x: 80, y: 80, width: 180, height: 140 },
            color: { r: 1, g: 0, b: 0 },
            borderWidth: 2,
        });

        const annotationEl = await waitFor(() => pageSlot.querySelector<HTMLElement>("[data-annotation-index]"));

        const rect = annotationEl.getBoundingClientRect();
        expect(rect.width).toBeGreaterThan(0);
        expect(rect.height).toBeGreaterThan(0);
        const centerX = rect.left + rect.width / 2;
        const centerY = rect.top + rect.height / 2;

        const hoverEvents: ViewerEventMap["annotation:hover"][] = [];
        const clickEvents: ViewerEventMap["annotation:click"][] = [];
        viewer!.on("annotation:hover", (p) => hoverEvents.push(p));
        viewer!.on("annotation:click", (p) => clickEvents.push(p));

        annotationEl.dispatchEvent(
            new PointerEvent("pointermove", {
                bubbles: true,
                clientX: centerX,
                clientY: centerY,
                pointerType: "mouse",
            }),
        );

        const hover = await waitFor(() => hoverEvents.find((p) => p !== null) ?? null);
        expect(hover).not.toBeNull();
        expect(hover!.pageIndex).toBe(0);
        expect(hover!.annotation.type).toBe("square");
        expect(hover!.annotation.name).toBe(annotName);
        expect(hover!.clientX).toBeCloseTo(centerX, 0);
        expect(hover!.clientY).toBeCloseTo(centerY, 0);

        annotationEl.dispatchEvent(
            new MouseEvent("click", {
                bubbles: true,
                clientX: centerX,
                clientY: centerY,
            }),
        );

        const click = await waitFor(() => clickEvents[0]);
        expect(click.pageIndex).toBe(0);
        expect(click.annotation.type).toBe("square");
        expect(click.annotation.name).toBe(annotName);
        expect(click.clientX).toBeCloseTo(centerX, 0);
        expect(click.clientY).toBeCloseTo(centerY, 0);
    });
});
