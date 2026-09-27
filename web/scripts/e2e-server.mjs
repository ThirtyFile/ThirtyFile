// Starts the server for the end-to-end tests (pnpm test:e2e) with an empty data directory, removed again when it stops.
// Needs a server built after the frontend (release builds embed web/dist):
//   pnpm build && (cd ../server && cargo build --release)
// Settings: THIRTYFILE_BIN (default ../server/target/release/thirtyfile), E2E_PORT (default 18080).
import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const bin = process.env.THIRTYFILE_BIN ?? fileURLToPath(new URL("../../server/target/release/thirtyfile", import.meta.url));
if (!existsSync(bin)) {
  console.error(`No server at ${bin}: build it with "cargo build --release" in server/ (after "pnpm build"), or set THIRTYFILE_BIN.`);
  process.exit(1);
}

const dir = mkdtempSync(join(tmpdir(), "thirtyfile-e2e-"));
const server = spawn(bin, [], {
  stdio: "inherit",
  env: {
    ...process.env,
    THIRTYFILE_ADDR: `127.0.0.1:${process.env.E2E_PORT ?? "18080"}`,
    THIRTYFILE_DATA: join(dir, "data"),
    THIRTYFILE_STORAGE: join(dir, "storage"),
    THIRTYFILE_ADMIN_PASSWORD: process.env.E2E_ADMIN_PASSWORD ?? "e2e-admin-password",
  },
});

const cleanup = () => rmSync(dir, { recursive: true, force: true });
server.on("exit", (code) => {
  cleanup();
  process.exit(code ?? 0);
});
for (const signal of ["SIGINT", "SIGTERM"]) process.on(signal, () => server.kill(signal));
