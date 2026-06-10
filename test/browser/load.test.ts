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

describe("AniccaViewer document loading", () => {
    it("opens a PDF and reports page count", async () => {
        client = await AniccaClient.create({
            googleFonts: false,
            disableUpdateCheck: true,
        });
        viewer = await client.createViewer({ container });

        const loaded = new Promise<{ pageCount: number }>((resolve) => {
            viewer!.on("document:load", resolve);
        });

        await viewer.load(sampleUrl);
        const event = await loaded;

        expect(event.pageCount).toBeGreaterThan(0);
        expect(viewer.pageCount).toBe(event.pageCount);
    });

    it("renders viewer DOM into the container", async () => {
        client = await AniccaClient.create({
            googleFonts: false,
            disableUpdateCheck: true,
        });
        viewer = await client.createViewer({ container });

        const loaded = new Promise<void>((resolve) => {
            viewer!.on("document:load", () => resolve());
        });
        await viewer.load(sampleUrl);
        await loaded;

        // The viewer should mount some UI inside the container.
        expect(container.childElementCount).toBeGreaterThan(0);
    });
});
