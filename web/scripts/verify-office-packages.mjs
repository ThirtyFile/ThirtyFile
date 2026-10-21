// Check distribution provenance without accessing an Office SDK source checkout.
import { readFile, writeFile, realpath, readdir } from "node:fs/promises";
import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = fileURLToPath(new URL("../", import.meta.url));
const require = createRequire(path.join(root, "package.json"));
const manifestPath = path.join(root, "vendor/manifest.json");
const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
const sha256 = (content) => createHash("sha256").update(content).digest("hex");
for (const entry of manifest.packages) {
  const content = await readFile(path.join(root, "vendor", entry.file));
  const digest = sha256(content);
  if (process.argv.includes("--record")) {
    entry.bytes = content.length;
    entry.sha256 = digest;
  } else if (entry.sha256 !== digest) throw new Error(`Office tarball changed: ${entry.file}`);
  const resolved = await realpath(require.resolve(`${entry.name}/package.json`));
  if (!resolved.startsWith(`${path.join(root, "node_modules")}${path.sep}`)) throw new Error(`Office resolves outside this project: ${resolved}`);
  const pkg = JSON.parse(await readFile(resolved, "utf8"));
  if (pkg.version !== manifest.version || !pkg.license) throw new Error(`Office package metadata mismatch: ${entry.name}`);
  if (!process.argv.includes("--built")) continue;
  const kind = entry.name.slice("@office/".length);
  const assetKind = kind === "sdk" ? "fonts" : kind;
  const installed = path.join(path.dirname(resolved), "dist/assets", kind === "sdk" ? "fonts" : "");
  for (const name of await readdir(installed)) {
    if (kind !== "sdk" && !/\.(js|wasm|json|html)$/.test(name)) continue;
    const source = sha256(await readFile(path.join(installed, name)));
    const deployed = sha256(await readFile(path.join(root, "dist/office-assets", manifest.version, assetKind, name)));
    if (source !== deployed) throw new Error(`Office deployment differs from installed package: ${assetKind}/${name}`);
  }
}
if (process.argv.includes("--record")) {
  manifest.createdAt = new Date().toISOString();
  await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
}
console.log(`Office ${manifest.version}: four installed tarballs verified${process.argv.includes("--built") ? ", all deployed Worker/WASM/font assets match" : ""}`);
