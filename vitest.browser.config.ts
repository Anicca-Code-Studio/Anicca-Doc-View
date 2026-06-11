import { defineConfig } from "vitest/config";
import { playwright } from "@vitest/browser-playwright";

export default defineConfig({
    test: {
        include: ["test/browser/**/*.test.ts"],
        browser: {
            enabled: true,
            provider: playwright(),
            instances: [{ browser: "chromium" }],
            headless: true,
        },
        testTimeout: 30_000,
        hookTimeout: 30_000,
    },
    server: {
        fs: {
            allow: [new URL(".", import.meta.url).pathname],
        },
    },
});
