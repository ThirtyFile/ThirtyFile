import { defineConfig } from "vite";
import { fileURLToPath } from "node:url";

// Traditional Chinese dictionary as one classic script (dist/zh-TW.js). The server adds it to the page for Chinese
// visitors, so it downloads in parallel with the app instead of after the app's entry module has run.
// English visitors never receive it; it's also the fallback loaded on demand when the server didn't add it.
export default defineConfig({
  publicDir: false,
  resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
  build: {
    outDir: "dist",
    emptyOutDir: false,
    lib: {
      entry: "src/lib/i18n/zh-TW/index.ts",
      formats: ["iife"],
      name: "__TF_ZH__",
      fileName: () => "zh-TW.js",
    },
    minify: true,
  },
});
