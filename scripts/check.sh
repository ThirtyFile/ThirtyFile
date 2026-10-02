#!/usr/bin/env bash
# The checks of a pull request, the same ones the tests on GitHub run (.github/workflows/build.yml calls this script):
#   scripts/check.sh           everything below, in this order
#   scripts/check.sh web       the interface: dependencies as locked, formatting, translations, the Office code's
#                              boundary, no fixed waits in the end-to-end tests, types, lint, unit tests
#   scripts/check.sh server    the server: migration files numbered without repeats or gaps, formatting, tests, and
#                              clippy with warnings as errors
#   scripts/check.sh migrations  only the migration files' numbers
#   scripts/check.sh e2e       the end-to-end test in a real browser (sign in, upload, preview, download), against a
#                              server built from here (the first time, get the browser: cd web && pnpm exec
#                              playwright install chromium)
#   scripts/check.sh site      the website: each translated page in every language with matching language links, and
#                              links and anchors that resolve (scripts/check-site.mjs; it needs only Node)
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
  # The end-to-end tests run in parallel: no fixed waits, no names without a unique part (scripts/check-e2e.mjs)
  node scripts/check-e2e.mjs
  pnpm typecheck
  pnpm lint
  pnpm test "$@"
}

migrations() {
  # The migration files are numbered 0001, 0002, … with no number used twice and none skipped. Two branches that each
  # add the next number pass on their own but break main together; the later one takes the next free number
  local dir="$ROOT/server/migrations" expected=1 failed=0 file name number previous=""
  for file in "$dir"/*.sql; do
    name=$(basename "$file")
    if ! [[ $name =~ ^([0-9]{4})_[a-z0-9_]+\.sql$ ]]; then
      echo "server/migrations/$name: name it 000N_<name>.sql (four digits, then lowercase letters, digits and _)" >&2
      failed=1
      continue
    fi
    number=$((10#${BASH_REMATCH[1]}))
    if [ "$number" -eq $((expected - 1)) ] && [ -n "$previous" ]; then
      echo "server/migrations/$name: number ${BASH_REMATCH[1]} is also used by $previous; give one of them the next free number" >&2
      failed=1
    elif [ "$number" -ne "$expected" ]; then
      echo "server/migrations/$name: expected number $(printf %04d "$expected") next, found ${BASH_REMATCH[1]}; the numbers go up by one with no gaps" >&2
      failed=1
      expected=$((number + 1))
    else
      expected=$((number + 1))
    fi
    previous=$name
  done
  return "$failed"
}

site() {
  node "$ROOT/scripts/check-site.mjs"
}

server() {
  migrations
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
  migrations) migrations ;;
  site) site ;;
  e2e) e2e ;;
  all) (web) && (server) && (e2e) && (site) ;;
  *) echo "usage: scripts/check.sh [web|server|migrations|site|e2e]" >&2; exit 2 ;;
esac
