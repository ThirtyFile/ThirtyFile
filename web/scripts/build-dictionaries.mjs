// Builds each language's dictionary (src/lib/i18n/<lang>/) as one classic script, dist/<lang>.js. The server adds the
// script of the page's language to the page, so it downloads in parallel with the app instead of after the app's entry
// module has run. Other languages' visitors never receive it; the app also loads it on demand when the server didn't
// add it (lib/i18n.ts). English is the source text and has no dictionary.
// Usage: node scripts/build-dictionaries.mjs (part of `pnpm build`, after the app)
import { readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "vite";

const web = fileURLToPath(new URL("../", import.meta.url));
const dir = join(web, "src/lib/i18n");
const langs = readdirSync(dir).filter((name) => statSync(join(dir, name)).isDirectory());

for (const lang of langs) {
  await build({
    configFile: false,
    root: web,
    publicDir: false,
    logLevel: "warn",
    resolve: { alias: { "@": join(web, "src") } },
    build: {
      outDir: "dist",
      emptyOutDir: false,
      lib: { entry: join(dir, lang, "index.ts"), formats: ["iife"], name: "__TF_DICT__", fileName: () => `${lang}.js` },
      minify: true,
    },
  });
  console.log(`dist/${lang}.js`);
}
