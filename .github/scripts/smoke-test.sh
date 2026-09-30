#!/usr/bin/env bash
# Starts an image the way people run it: with the compose.yaml that is shipped (read-only, few capabilities), on
# folders bind-mounted from the host, which Docker creates as root so the server has to take them over for user 1000.
# The latest release starts first and writes a file; the new image then starts on the same folders, upgrades them and
# must still serve that file, and again after a restart.
#   smoke-test.sh <image> [<previous image>]
# SMOKE_PLATFORM=linux/arm64 starts another platform's images (under QEMU). SMOKE_PORT and SMOKE_NAME (the Compose
# project and container) let it run beside other instances.
set -euo pipefail
IMAGE="$1"
PREVIOUS="${2:-ghcr.io/thirtyfile/thirtyfile:latest}"
PLATFORM="${SMOKE_PLATFORM:-}"
PORT="${SMOKE_PORT:-18080}"
NAME="${SMOKE_NAME:-smoke}"
BASE="http://127.0.0.1:$PORT"
PASSWORD="smoke-$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')"
SHIPPED="$(cd "$(dirname "$0")/../.." && pwd)/compose.yaml"
# The Compose project: compose.yaml as shipped, an override for the image, name and port, and the data and storage
# folders next to them
DIR=$(mktemp -d)

fail() {
  echo "::error::$1"
  docker ps -a --filter "name=^$NAME\$"
  docker logs "$NAME" 2>&1 | tail -40 || true
  exit 1
}

compose() { # image, docker compose arguments…
  SMOKE_IMAGE="$1" THIRTYFILE_ADMIN_PASSWORD="$PASSWORD" docker compose --project-directory "$DIR" -p "$NAME" "${@:2}"
}

cleanup() {
  compose "$IMAGE" down --timeout 30 >/dev/null 2>&1 || true
  # The folders belong to user 1000 now
  rm -rf "$DIR" 2>/dev/null || sudo -n rm -rf "$DIR" 2>/dev/null || true
}
trap cleanup EXIT

cp "$SHIPPED" "$DIR/compose.yaml"
{
  echo "services:"
  echo "  thirtyfile:"
  echo "    image: \${SMOKE_IMAGE:?}"
  echo "    container_name: $NAME"
  echo "    ports: !override"
  echo "      - \"127.0.0.1:$PORT:8080\""
  if [ -n "$PLATFORM" ]; then echo "    platform: $PLATFORM"; fi
} > "$DIR/compose.override.yaml"

# Waits for the server, and for Docker's health check (thirtyfile health) to say so too
wait_healthy() {
  for _ in $(seq 1 180); do
    if curl -fsS "$BASE/api/health" >/dev/null 2>&1 && [ "$(docker inspect -f '{{.State.Health.Status}}' "$NAME" 2>/dev/null)" = healthy ]; then
      return 0
    fi
    sleep 1
  done
  fail "$1 didn't become healthy"
}

sign_in() { # cookie file
  curl -fsS -c "$1" -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$PASSWORD\"}" "$BASE/api/auth/login" >/dev/null || fail "signing in failed"
}

b64() { printf '%s' "$1" | base64 -w0; }

# Creates a file in My files of admin with the given content; its id is left in FILE_ID
new_file() { # cookie file, name, content
  local root id
  root=$(curl -fsS -b "$1" "$BASE/api/auth/me" | sed -n 's/.*"root_id":"\([^"]*\)".*/\1/p') || fail "reading the account failed"
  id=$(curl -fsS -D - -o /dev/null -b "$1" -X POST "$BASE/api/uploads" \
    -H 'Tus-Resumable: 1.0.0' -H 'Upload-Length: 0' \
    -H "Upload-Metadata: filename $(b64 "$2"),parentId $(b64 "$root")" | tr -d '\r' | awk -F': ' 'tolower($1)=="x-node-id"{print $2}') || true
  [ -n "$id" ] || fail "creating $2 failed"
  curl -fsS -b "$1" -X PUT --data-binary "$3" "$BASE/api/files/$id/content" >/dev/null || fail "writing $2 failed"
  FILE_ID="$id"
}

expect_file() { # cookie file, id, content
  local content
  content=$(curl -fsS -b "$1" "$BASE/api/files/$2/content") || fail "file $2 is gone"
  [ "$content" = "$3" ] || fail "file $2 reads \"$content\" instead of \"$3\""
}

owner() { stat -c %u "$1"; }

echo "== The latest release ($PREVIOUS${PLATFORM:+, $PLATFORM}), writing some data"
docker pull -q ${PLATFORM:+--platform "$PLATFORM"} "$PREVIOUS" >/dev/null
compose "$PREVIOUS" up -d
wait_healthy "The latest release"
sign_in "$DIR/old.cookies"
new_file "$DIR/old.cookies" before-upgrade.txt 'written by the latest release'
kept="$FILE_ID"
compose "$PREVIOUS" down --timeout 30

echo "== The new image ($IMAGE) on that data"
compose "$IMAGE" up -d
wait_healthy "The new image"
curl -fsS "$BASE/" | grep -q '<div id="root"' || fail "the web pages aren't served"
sign_in "$DIR/new.cookies"
expect_file "$DIR/new.cookies" "$kept" 'written by the latest release'
# The image has no shell or ps: the host lists the container's processes
uids=$(docker top "$NAME" -o pid,uid | awk 'NR > 1 { print $2 }' | sort -u)
[ "$uids" = "1000" ] || fail "the server doesn't run as user 1000 (but as: $uids)"
# Docker created the bind-mounted folders as root: the server gave them to user 1000
for folder in data storage; do
  [ "$(owner "$DIR/$folder")" = 1000 ] || fail "$folder belongs to user $(owner "$DIR/$folder"), not 1000"
done
# Files are kept as ordinary files: My files of admin is storage/users/admin
new_file "$DIR/new.cookies" hello.txt 'a plain file'
added="$FILE_ID"
on_disk=$(docker cp "$NAME:/storage/users/admin/hello.txt" - | tar -xO) || fail "My files isn't the folder storage/users/admin"
[ "$on_disk" = "a plain file" ] || fail "storage/users/admin/hello.txt reads \"$on_disk\""

echo "== The new image, restarted"
compose "$IMAGE" restart --timeout 30
wait_healthy "The restarted image"
sign_in "$DIR/again.cookies"
expect_file "$DIR/again.cookies" "$kept" 'written by the latest release'
expect_file "$DIR/again.cookies" "$added" 'a plain file'

echo "All good"
