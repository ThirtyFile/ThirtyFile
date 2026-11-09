// Keep the temporary SQLx dependency backport bounded to its reviewed manifest and license changes.
import { createHash } from "node:crypto";
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { join, relative } from "node:path";

const dir = fileURLToPath(new URL("../../server/vendor/sqlx-sqlite/", import.meta.url));
const upstream = JSON.parse(readFileSync(join(dir, "upstream.json"), "utf8"));
const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");
const files = (at) => readdirSync(at, { withFileTypes: true }).flatMap((e) => (e.isDirectory() ? files(join(at, e.name)) : [relative(dir, join(at, e.name)).replaceAll("\\", "/")]));
if (upstream.archive_sha256 !== "488e99c397a62007e4229aec669a179816339afc6d2620ca6fa420dbee2e982c") throw Error("The SQLx package provenance changed; review the upstream archive.");
const expected = [...Object.keys(upstream.files), "upstream.json"].sort();
if (JSON.stringify(files(dir).sort()) !== JSON.stringify(expected)) throw Error("The SQLx patch has unexpected or missing files.");
for (const [name, checksum] of Object.entries(upstream.files)) {
  const bytes = readFileSync(join(dir, name));
  if (hash(bytes) !== checksum) throw Error(`The reviewed SQLx patch changed: ${name}`);
  if (name in upstream.original_files) {
    const original = name === "Cargo.toml" ? Buffer.from(bytes.toString("utf8").replace('version = ">=0.30.1, <0.40.0"', 'version = ">=0.30.1, <0.38.0"')) : bytes;
    if (hash(original) !== upstream.original_files[name]) throw Error(`The SQLx backport changes published source: ${name}`);
  }
}
console.log(`SQLx SQLite backport: ${Object.keys(upstream.original_files).length} published files verified; only the SQLite upper bound and license texts differ.`);
