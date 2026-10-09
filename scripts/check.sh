#!/usr/bin/env bash
# Local checks and release verification; pull requests run only `quick` (checks.yml):
#   scripts/check.sh           everything below, in this order
#   scripts/check.sh web       the interface: dependencies as locked, formatting, translations, the Office code's
#                              boundary, no fixed waits in the end-to-end tests, types, lint, unit tests
#   scripts/check.sh server    the server: migration files numbered without repeats or gaps, formatting, tests, and
#                              clippy with warnings as errors
#   scripts/check.sh migrations  only the migration files' numbers
#   scripts/check.sh e2e       the interface's size budget (scripts/check-bundle.mjs), then the end-to-end test in a
#                              real browser (sign in, upload, preview, download), against a server built from here
#                              (the first time, get the browser: cd web && pnpm exec playwright install chromium)
#   scripts/check.sh site      the website: each translated page in every language with matching language links, and
#                              links and anchors that resolve (scripts/check-site.mjs; it needs only Node)
#   scripts/check.sh quick     pull-request checks, without unit/browser tests or artifact compilation
#   scripts/check.sh web-static  the interface checks without unit tests
#   scripts/check.sh released-migrations  released migrations are unchanged (needs Git tags)
# Arguments after `web` go to the unit tests (CI passes --coverage).
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)

web_static() {
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
}

web() {
  web_static
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

released_migrations() {
  cd "$ROOT"
  local latest tag file failed=0
  latest=$(git tag -l 'v*' --sort=-v:refname | awk '!/-/ && !found { print; found=1 }')
  for tag in v0.4.0 $latest; do
    git rev-parse --verify "$tag^{commit}" > /dev/null
    while IFS= read -r file; do
      if ! git diff --quiet "$tag" HEAD -- "$file" || ! git diff --quiet HEAD -- "$file"; then
        echo "$file differs from the file released in $tag. Released migrations never change: put the change in a new migration file." >&2
        failed=1
      fi
    done < <(git ls-tree -r --name-only "$tag" -- server/migrations/)
  done
  return "$failed"
}

quick() {
  (web_static)
  migrations
  (released_migrations)
  (cd "$ROOT/server" && cargo fmt --check)
  site
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
  # What every page downloads at the start, any one chunk, and the Mac style's chunks kept out of the start
  node scripts/check-bundle.mjs
  (cd "$ROOT/server" && cargo build --locked)
  local bin="${CARGO_TARGET_DIR:-$ROOT/server/target}/debug/thirtyfile"
  if [ -f "$bin.exe" ]; then bin="$bin.exe"; fi
  THIRTYFILE_BIN="$bin" pnpm test:e2e
}

case "${1:-all}" in
  web) shift; web "$@" ;;
  web-static) web_static ;;
  quick) quick ;;
  released-migrations) released_migrations ;;
  server) server ;;
  migrations) migrations ;;
  site) site ;;
  e2e) e2e ;;
  all) (web) && (server) && (e2e) && (site) ;;
  *) echo "usage: scripts/check.sh [quick|web-static|web|server|migrations|released-migrations|site|e2e]" >&2; exit 2 ;;
esac
