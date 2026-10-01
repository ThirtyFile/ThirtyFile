#!/usr/bin/env bash
# The checks of a pull request, the same ones the tests on GitHub run (.github/workflows/build.yml calls this script):
#   scripts/check.sh           everything below, in this order
#   scripts/check.sh web       the interface: dependencies as locked, formatting, translations, the Office code's
#                              boundary, types, lint, unit tests
#   scripts/check.sh server    the server: formatting, tests, and clippy with warnings as errors
#   scripts/check.sh e2e       the end-to-end test in a real browser (sign in, upload, preview, download), against a
#                              server built from here (the first time, get the browser: cd web && pnpm exec
#                              playwright install chromium)
# Arguments after `web` go to the unit tests (CI passes --coverage).
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)

web() {
  cd "$ROOT/web"
  pnpm install --frozen-lockfile
  # Formatting (oxfmt, web/.oxfmtrc.json): `pnpm format` fixes it
  pnpm format:check
  node scripts/check-i18n.mjs
  node scripts/check-boundaries.mjs
  pnpm typecheck
  pnpm lint
  pnpm test "$@"
}

server() {
  cd "$ROOT/server"
  # Formatting (rustfmt, server/rustfmt.toml): `cargo fmt` fixes it
  cargo fmt --check
  cargo test --locked
  cargo clippy --locked --all-targets -- -D warnings
}

e2e() {
  # The server embeds the interface it is built with (server/build.rs), so the interface is built first
  cd "$ROOT/web"
  pnpm build
  (cd "$ROOT/server" && cargo build --locked)
  local bin="${CARGO_TARGET_DIR:-$ROOT/server/target}/debug/thirtyfile"
  if [ -f "$bin.exe" ]; then bin="$bin.exe"; fi
  THIRTYFILE_BIN="$bin" pnpm test:e2e
}

case "${1:-all}" in
  web) shift; web "$@" ;;
  server) server ;;
  e2e) e2e ;;
  all) (web) && (server) && (e2e) ;;
  *) echo "usage: scripts/check.sh [web|server|e2e]" >&2; exit 2 ;;
esac
