import { defineConfig, devices } from "@playwright/test";

// End-to-end smoke test (pnpm test:e2e) against the real server, started by scripts/e2e-server.mjs with an empty data
// directory. PLAYWRIGHT_CHROMIUM lets a machine use a Chromium that is already installed instead of Playwright's own.
const port = process.env.E2E_PORT ?? "18080";

export default defineConfig({
  testDir: "tests/e2e",
  timeout: 60_000,
  retries: process.env.CI ? 1 : 0,
  // How many tests run at once: E2E_WORKERS (CI sets 4), or Playwright's default, half the CPU cores. The tests are
  // written to run in parallel (tests/e2e/helpers.ts)
  workers: process.env.E2E_WORKERS ? Number(process.env.E2E_WORKERS) : undefined,
  reporter: process.env.CI ? [["list"], ["github"]] : "list",
  use: {
    baseURL: `http://127.0.0.1:${port}`,
    locale: "en-US",
    trace: "retain-on-failure",
  },
  projects: [
    {
      name: "chromium",
      use: {
        ...devices["Desktop Chrome"],
        launchOptions: process.env.PLAYWRIGHT_CHROMIUM ? { executablePath: process.env.PLAYWRIGHT_CHROMIUM } : {},
      },
    },
  ],
  webServer: {
    command: "node scripts/e2e-server.mjs",
    url: `http://127.0.0.1:${port}/api/branding`,
    reuseExistingServer: false,
    timeout: 60_000,
  },
});
