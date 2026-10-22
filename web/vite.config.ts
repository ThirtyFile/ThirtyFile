import { defineConfig, lazyPlugins } from "vite-plus";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath } from "node:url";

// During development Vite serves the frontend and proxies the API to the Rust backend (127.0.0.1:8080 by default)
const backend = process.env.THIRTYFILE_BACKEND ?? "http://127.0.0.1:8080";

export default defineConfig({
  // Vite+ (`vp`): the formatter (fmt), the linter (lint), Vite and Vitest (vitest.config.ts) are all set here and in vitest.config.ts
  fmt: {
    // Close to how the code was written before a formatter was used: double quotes, semicolons, trailing commas and
    // long lines. `pnpm format` formats src, tests and scripts; `pnpm format:check` checks them. Tables laid out by
    // hand, with several entries per line, keep their layout with a `// oxfmt-ignore` line above them.
    printWidth: 190,
    // The dictionaries (src/lib/i18n/<lang>/) keep one entry per line, however long: scripts/check-i18n.mjs reads them
    // line by line. Files copied from other projects (src/vendor/) stay as they were published, and the API's shapes
    // (tests/contract/shapes.json) as the server's test writes them
    ignorePatterns: ["src/lib/i18n/*/**", "src/vendor/**", "tests/contract/shapes.json"],
  },
  lint: {
    plugins: ["react", "typescript", "unicorn", "oxc", "vitest"],
    categories: {
      correctness: "error",
    },
    rules: {
      "react/rules-of-hooks": "error",
      "react/exhaustive-deps": "error",
      // Rules for the React Compiler, which the project doesn't use: state set from effects, refs read while
      // rendering, Date.now() in render, sheets changed in place and the list virtualizer are deliberate here
      "react/set-state-in-effect": "off",
      "react/refs": "off",
      "react/purity": "off",
      "react/immutability": "off",
      "react/incompatible-library": "off",
      // new Array(n) is meant as a length throughout (chart series)
      "unicorn/no-new-array": "off",
      // Copying before a loop that removes items from the same collection (live attribute lists, maps) is deliberate
      "unicorn/no-useless-spread": "off",
      "vite-plus/prefer-vite-plus-imports": "error",
    },
    ignorePatterns: ["dist", "node_modules", "public", "test-results"],
    jsPlugins: [
      {
        name: "vite-plus",
        specifier: "vite-plus/oxlint-plugin",
      },
    ],
  },
  plugins: lazyPlugins(() => [react(), tailwindcss()]),
  resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
  server: {
    port: 5173,
    strictPort: true,
    // Keep the original Host: the backend's CSRF check compares Origin with Host (changeOrigin would rewrite Host and get requests rejected)
    proxy: { "/api": { target: backend, changeOrigin: false } },
  },
  build: {
    outDir: "dist",
    chunkSizeWarningLimit: 1500,
  },
});
