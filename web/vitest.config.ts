import { defineConfig } from "vitest/config";
import { fileURLToPath } from "node:url";

// Unit tests (pnpm test): plain logic, run in happy-dom for DOMParser, XMLSerializer and localStorage.
// The end-to-end test in tests/e2e runs with Playwright instead (pnpm test:e2e).
export default defineConfig({
  resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
  test: {
    environment: "happy-dom",
    include: ["tests/unit/**/*.test.ts"],
    coverage: {
      provider: "v8",
      include: ["src/**/*.{ts,tsx}"],
      exclude: ["src/lib/i18n/zh-TW/**"],
      reporter: ["text-summary", "json-summary"],
    },
  },
});
