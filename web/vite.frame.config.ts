import { defineConfig } from "vite";
import { fileURLToPath } from "node:url";

// Renderer for Word/PowerPoint previews, bundled into a single file (no code splitting or dynamic imports).
// The host page inlines it into the sandboxed iframe's srcdoc, so the iframe needs no network access at all.
export default defineConfig({
  publicDir: false,
  resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
  // Library mode doesn't replace this automatically, and some packages (JSZip) read process.env.NODE_ENV in the browser
  define: { "process.env.NODE_ENV": JSON.stringify("production") },
  build: {
    outDir: "dist",
    emptyOutDir: false,
    lib: {
      entry: "src/office-frame.ts",
      formats: ["iife"],
      name: "OfficeFrame",
      fileName: () => "office-frame.js",
    },
    minify: true,
    chunkSizeWarningLimit: 3000,
  },
});
