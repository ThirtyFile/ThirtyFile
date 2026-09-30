#!/usr/bin/env bash
# Starts the image built for a release the way people run it, on data made by the latest release: the release writes
# a file, then the new image starts on the same folders, upgrades them and must still serve that file
#   smoke-test.sh <image> [<previous image>]
set -euo pipefail
IMAGE="$1"
PREVIOUS="${2:-ghcr.io/thirtyfile/thirtyfile:latest}"
PASSWORD="smoke-$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')"
# Overridable for running it beside other instances: SMOKE_PORT, SMOKE_NAME (containers and volumes), and SMOKE_BASE
# for running the script inside a container (http://host.docker.internal:18080)
PORT="${SMOKE_PORT:-18080}"
NAME="${SMOKE_NAME:-smoke}"
BASE="${SMOKE_BASE:-http://127.0.0.1:$PORT}"
TMP=$(mktemp -d)

fail() {
  echo "::error::$1"
  docker ps -a --filter "name=^$NAME-"
  for c in $(docker ps -aq --filter "name=^$NAME-"); do docker logs "$c" 2>&1 | tail -30; done
  exit 1
}

cleanup() {
  docker rm -f "$NAME-old" "$NAME-new" >/dev/null 2>&1 || true
  docker volume rm "$NAME-data" "$NAME-storage" >/dev/null 2>&1 || true
  rm -rf "$TMP"
}
cleanup
mkdir -p "$TMP"
trap cleanup EXIT

# Runs the image with as few privileges as it needs: read-only root, only the capabilities for taking over the folders
start() { # name image data storage
  docker run -d --name "$NAME-$1" -p "$PORT:8080" --read-only \
    --cap-drop ALL --cap-add CHOWN --cap-add SETUID --cap-add SETGID \
    --security-opt no-new-privileges \
    -e THIRTYFILE_ADMIN_PASSWORD="$PASSWORD" -v "$3:/data" -v "$4:/storage" "$2" >/dev/null
}

wait_healthy() {
  for _ in $(seq 1 60); do
    curl -fsS "$BASE/api/health" >/dev/null 2>&1 && return 0
    sleep 1
  done
  fail "$1 didn't answer on /api/health"
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

echo "== The latest release ($PREVIOUS), writing some data"
docker pull -q "$PREVIOUS" >/dev/null
start old "$PREVIOUS" "$NAME-data" "$NAME-storage"
wait_healthy "The latest release"
sign_in "$TMP/old.cookies"
new_file "$TMP/old.cookies" before-upgrade.txt 'written by the latest release'
kept="$FILE_ID"
docker stop -t 30 "$NAME-old" >/dev/null
docker rm "$NAME-old" >/dev/null

echo "== The new image, locked down, on that data"
start new "$IMAGE" "$NAME-data" "$NAME-storage"
wait_healthy "The new image"
curl -fsS "$BASE/" | grep -q '<div id="root"' || fail "the web pages aren't served"
sign_in "$TMP/new.cookies"
content=$(curl -fsS -b "$TMP/new.cookies" "$BASE/api/files/$kept/content") || fail "the file written by the latest release is gone"
[ "$content" = "written by the latest release" ] || fail "the file written by the latest release reads \"$content\""
# The image has no shell or ps: the host lists the container's processes
uids=$(docker top "$NAME-new" -o pid,uid | awk 'NR > 1 { print $2 }' | sort -u)
[ "$uids" = "1000" ] || fail "the server doesn't run as user 1000 (but as: $uids)"
docker exec "$NAME-new" thirtyfile health >/dev/null || fail "the health check command fails"
# Files are kept as ordinary files: My files of admin is /storage/users/admin
new_file "$TMP/new.cookies" hello.txt 'a plain file'
on_disk=$(docker cp "$NAME-new":/storage/users/admin/hello.txt - | tar -xO) || fail "My files isn't the folder /storage/users/admin"
[ "$on_disk" = "a plain file" ] || fail "/storage/users/admin/hello.txt reads \"$on_disk\""
docker rm -f "$NAME-new" >/dev/null

echo "All good"
