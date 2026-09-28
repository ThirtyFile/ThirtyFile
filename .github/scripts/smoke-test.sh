#!/usr/bin/env bash
# Starts the image built for a pull request the way people run it, and upgrades the latest release to it.
#   smoke-test.sh <image>
set -euo pipefail
IMAGE="$1"
PREVIOUS="ghcr.io/thirtyfile/thirtyfile:latest"
PASSWORD="smoke-$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')"
PORT=18080
# Overridable for running the script inside a container: SMOKE_BASE=http://host.docker.internal:18080
BASE="${SMOKE_BASE:-http://127.0.0.1:$PORT}"

fail() { echo "::error::$1"; docker ps -a; for c in $(docker ps -aq); do docker logs "$c" 2>&1 | tail -30; done; exit 1; }

# Runs the image with as few privileges as it needs: read-only root, only the capabilities for taking over the folders
start() { # name image data storage
  docker run -d --name "$1" -p "$PORT:8080" --read-only \
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

echo "== The new image, locked down"
start new "$IMAGE" smoke-data smoke-storage
wait_healthy "The new image"
curl -fsS "$BASE/" | grep -q '<div id="root"' || fail "the web pages aren't served"
sign_in /tmp/new.cookies
# The image has no shell or ps: the host lists the container's processes
uids=$(docker top new -o pid,uid | awk 'NR > 1 { print $2 }' | sort -u)
[ "$uids" = "1000" ] || fail "the server doesn't run as user 1000 (but as: $uids)"
docker exec new thirtyfile health >/dev/null || fail "the health check command fails"
# A new install keeps files as ordinary files: My files of admin is /storage/users/admin
root=$(curl -fsS -b /tmp/new.cookies "$BASE/api/auth/me" | sed -n 's/.*"root_id":"\([^"]*\)".*/\1/p')
id=$(curl -fsS -D - -o /dev/null -b /tmp/new.cookies -X POST "$BASE/api/uploads" \
  -H 'Tus-Resumable: 1.0.0' -H 'Upload-Length: 0' \
  -H "Upload-Metadata: filename $(b64 hello.txt),parentId $(b64 "$root")" | tr -d '\r' | awk -F': ' 'tolower($1)=="x-node-id"{print $2}')
[ -n "$id" ] || fail "creating a file in the new image failed"
curl -fsS -b /tmp/new.cookies -X PUT --data-binary 'a plain file' "$BASE/api/files/$id/content" >/dev/null
on_disk=$(docker cp new:/storage/users/admin/hello.txt - | tar -xO) || fail "My files isn't the folder /storage/users/admin"
[ "$on_disk" = "a plain file" ] || fail "/storage/users/admin/hello.txt reads \"$on_disk\""
docker rm -f new >/dev/null

echo "== Upgrading the latest release"
docker pull -q "$PREVIOUS" >/dev/null
# Releases up to 0.3 kept the database's whole migration history, which newer versions don't upgrade from (#170)
previous_version=$(docker image inspect -f '{{ index .Config.Labels "org.opencontainers.image.version" }}' "$PREVIOUS")
case "$previous_version" in
  0.[0-3].*)
    echo "The latest release ($previous_version) predates the current database schema: nothing to upgrade from"
    echo "All good"
    exit 0
    ;;
esac
start previous "$PREVIOUS" upgrade-data upgrade-storage
wait_healthy "The latest release"
sign_in /tmp/old.cookies
root=$(curl -fsS -b /tmp/old.cookies "$BASE/api/auth/me" | sed -n 's/.*"root_id":"\([^"]*\)".*/\1/p')
id=$(curl -fsS -D - -o /dev/null -b /tmp/old.cookies -X POST "$BASE/api/uploads" \
  -H 'Tus-Resumable: 1.0.0' -H 'Upload-Length: 0' \
  -H "Upload-Metadata: filename $(b64 kept.txt),parentId $(b64 "$root")" | tr -d '\r' | awk -F': ' 'tolower($1)=="x-node-id"{print $2}')
[ -n "$id" ] || fail "creating a file in the latest release failed"
curl -fsS -b /tmp/old.cookies -X PUT --data-binary 'kept across the upgrade' "$BASE/api/files/$id/content" >/dev/null
docker rm -f previous >/dev/null

start upgraded "$IMAGE" upgrade-data upgrade-storage
wait_healthy "The upgraded image"
sign_in /tmp/up.cookies
content=$(curl -fsS -b /tmp/up.cookies "$BASE/api/files/$id/content")
[ "$content" = "kept across the upgrade" ] || fail "the file written by the latest release reads \"$content\" after upgrading"
docker rm -f upgraded >/dev/null
echo "All good"
