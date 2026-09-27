import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath } from "node:url";

// During development Vite serves the frontend and proxies the API to the Rust backend (127.0.0.1:8080 by default)
const backend = process.env.THIRTYFILE_BACKEND ?? "http://127.0.0.1:8080";

export default defineConfig({
  plugins: [react(), tailwindcss()],
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
