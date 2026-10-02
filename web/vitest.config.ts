import { defineConfig } from "vitest/config";
import { fileURLToPath } from "node:url";

// Unit tests (pnpm test): plain logic and components, run in happy-dom for DOMParser, XMLSerializer and localStorage.
// The end-to-end tests in tests/e2e run with Playwright instead (pnpm test:e2e).
export default defineConfig({
  resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
  test: {
    environment: "happy-dom",
    include: ["tests/unit/**/*.test.{ts,tsx}"],
    coverage: {
      provider: "v8",
      include: ["src/**/*.{ts,tsx}"],
      exclude: ["src/lib/i18n/*/**"],
      reporter: ["text-summary", "json-summary"],
      // A floor under what the unit tests cover (pnpm test --coverage, as CI runs them fails below it): raised as tests
      // are added, never lowered to let a change through
      thresholds: { statements: 34, branches: 23, functions: 26, lines: 35 },
    },
  },
});
