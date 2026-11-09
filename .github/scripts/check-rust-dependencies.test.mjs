import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, copyFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const helper = fileURLToPath(new URL("../../scripts/check-rust-dependencies.sh", import.meta.url));
const bash = process.platform === "win32" ? (process.env.THIRTYFILE_TEST_BASH ?? "C:/Program Files/Git/bin/bash.exe") : "bash";

function runGate({ fetchExit = 0, checkExit = 0, database = true, checks = [] } = {}) {
  const dir = mkdtempSync(join(tmpdir(), "thirtyfile-advisory-gate-"));
  try {
    const home = join(dir, "cargo-home"),
      bin = join(dir, "bin"),
      project = join(dir, "released-source");
    mkdirSync(bin);
    mkdirSync(project);
    if (database) {
      const db = join(home, "advisory-dbs", "advisory-db-fixture");
      mkdirSync(db, { recursive: true });
      for (const args of [
        ["init", "--quiet", "--initial-branch=main", db],
        ["-C", db, "-c", "user.name=Example", "-c", "user.email=developer@example.com", "-c", "commit.gpgsign=false", "commit", "--allow-empty", "--quiet", "-m", "Fixture"],
      ]) {
        const git = spawnSync("git", args, { encoding: "utf8" });
        assert.equal(git.status, 0, git.stderr);
      }
    }
    const trace = join(dir, "calls.txt"),
      copied = join(dir, "current-workflow-gate.sh");
    copyFileSync(helper, copied);
    writeFileSync(
      join(bin, "cargo"),
      `#!/usr/bin/env bash
printf '%s\\n' "$*" >> "$GATE_TRACE"
case "$*" in
  'deny --version') echo 'cargo-deny fixture'; exit 0 ;;
  *'fetch db') exit "$FETCH_EXIT" ;;
  *'check '*) exit "$CHECK_EXIT" ;;
  *) exit 99 ;;
esac
`,
      { mode: 0o755 },
    );
    const result = spawnSync(
      bash,
      [
        "-c",
        'mock_bin="$MOCK_BIN"; if command -v cygpath >/dev/null 2>&1; then mock_bin=$(cygpath -u "$mock_bin"); fi; export PATH="$mock_bin:$PATH"; exec bash "$COPIED_GATE" "$@"',
        "gate-test",
        ...checks,
      ],
      {
        encoding: "utf8",
        env: {
          ...process.env,
          CARGO_HOME: home,
          THIRTYFILE_CHECK_ROOT: project,
          MOCK_BIN: bin,
          COPIED_GATE: copied,
          GATE_TRACE: trace,
          FETCH_EXIT: String(fetchExit),
          CHECK_EXIT: String(checkExit),
        },
      },
    );
    return { ...result, calls: readFileSync(trace, "utf8").trim().split("\n") };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test("the preserved workflow gate refreshes the database and logs its revision against an older source checkout", () => {
  const result = runGate({ checks: ["advisories"] });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /revision: [a-f0-9]{40}/);
  assert.deepEqual(result.calls, ["deny --version", "deny --manifest-path server/Cargo.toml --locked fetch db", "deny --manifest-path server/Cargo.toml --locked check advisories"]);
});

test("a failed refresh stops before checking a cached database", () => {
  const result = runGate({ fetchExit: 37 });
  assert.equal(result.status, 37);
  assert.equal(result.calls.length, 2);
});

test("a dependency failure propagates without lowering the check", () => {
  const result = runGate({ checkExit: 42 });
  assert.equal(result.status, 42);
  assert.match(result.stdout, /revision: [a-f0-9]{40}/);
  assert.equal(result.calls.at(-1), "deny --manifest-path server/Cargo.toml --locked check all");
});

test("an unrecorded database revision cannot silently pass", () => {
  const result = runGate({ database: false });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /No fetched advisory database revision/);
  assert.equal(result.calls.length, 2);
});

test("both fresh publication gates run against the selected image source before version publication", () => {
  const workflow = readFileSync(new URL("../workflows/release.yml", import.meta.url), "utf8");
  const checkout = workflow.indexOf("ref: ${{ steps.image.outputs.commit }}");
  const rust = workflow.indexOf("name: Refresh Rust advisories before publication");
  const npm = workflow.indexOf("name: Refresh frontend advisories before publication");
  const draft = workflow.indexOf("name: Draft release");
  assert.ok(checkout > 0 && checkout < rust && rust < npm && npm < draft);
  assert.ok(workflow.indexOf('cp scripts/check-rust-dependencies.sh "$RUNNER_TEMP/check-rust-dependencies.sh"') < checkout);
  assert.match(workflow.slice(rust, npm), /THIRTYFILE_CHECK_ROOT: \$\{\{ github.workspace \}\}/);
});
