#!/usr/bin/env bash
# Refresh the advisory database at each gate, and record the exact input used by Cargo Deny.
set -euo pipefail
ROOT="${THIRTYFILE_CHECK_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
cd "$ROOT"
cargo deny --version
cargo deny --manifest-path server/Cargo.toml --locked fetch db
deny_home="${CARGO_HOME:-$HOME/.cargo}"
if command -v cygpath >/dev/null 2>&1; then deny_home=$(cygpath -u "$deny_home"); fi
databases=()
for db in "$deny_home"/advisory-dbs/advisory-db-*; do
  [ -d "$db" ] || continue
  databases+=("$db")
done
[ "${#databases[@]}" -gt 0 ] || { echo "No fetched advisory database revision could be recorded" >&2; exit 1; }
if [ "$#" -eq 0 ]; then set -- all; fi
status=0
cargo deny --manifest-path server/Cargo.toml --locked check "$@" || status=$?
# `check` may refresh again: record the revisions after it, including when an advisory fails the gate.
for db in "${databases[@]}"; do
  revision=$(git -C "$db" rev-parse HEAD)
  echo "Advisory database: $db; revision: $revision"
done
exit "$status"
