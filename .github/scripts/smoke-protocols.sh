#!/usr/bin/env bash
# Turns on SFTP, FTP (explicit FTPS) and S3 on a running server and makes one request with each, as people's clients
# would: run by smoke-test.sh on the new image, where a protocol that can't start (its port bound as user 1000, its host
# key kept under /data, the FTP certificate mounted read-only) shows only once it is turned on.
#   smoke-protocols.sh <base URL> <admin password> <SFTP port> <FTP port>
# SFTP and FTP are reached on 127.0.0.1 at those ports, FTP with a certificate (not checked here) and its passive ports
# published as they are; S3 is reached through the base URL's port as s3.example.com.
set -euo pipefail
BASE="$1"
PASSWORD="$2"
SFTP_PORT="$3"
FTP_PORT="$4"
DIR=$(mktemp -d)
trap 'rm -rf "$DIR"' EXIT
JAR="$DIR/cookies"

fail() {
  echo "::error::$1"
  exit 1
}

# A field of the JSON document on stdin, by its path (such as buckets.0.id)
field() {
  python3 -c 'import json, sys
value = json.load(sys.stdin)
for part in sys.argv[1].split("."):
    value = value[int(part)] if part.isdigit() else value[part]
print(value)' "$1"
}

api() { # method, path below /api, JSON body (optional)
  if [ $# -ge 3 ]; then
    curl -fsS -b "$JAR" -X "$1" -H 'Content-Type: application/json' -d "$3" "$BASE/api$2"
  else
    curl -fsS -b "$JAR" -X "$1" "$BASE/api$2"
  fi
}

# Runs a command until it succeeds, for a minute at most
retry() {
  for _ in $(seq 1 60); do
    if "$@"; then return 0; fi
    sleep 1
  done
  return 1
}

curl -fsS -c "$JAR" -H 'Content-Type: application/json' -d "{\"username\":\"admin\",\"password\":\"$PASSWORD\"}" "$BASE/api/auth/login" >/dev/null ||
  fail "signing in failed"
# Handing out credentials asks for the password again
confirm="\"password\":\"$PASSWORD\""

echo "== SFTP: listing the top folder with an app password"
api PUT /admin/sftp '{"enabled":true,"keys_enabled":false}' >/dev/null || fail "turning SFTP on failed"
token=$(api POST /auth/app-passwords "{\"name\":\"Smoke test\",\"scope\":\"read\",$confirm}" | field token) || fail "making an app password failed"
sftp_list() { curl -fsS -k --user "admin:$token" "sftp://127.0.0.1:$SFTP_PORT/" >"$DIR/sftp.txt" 2>"$DIR/sftp.err"; }
retry sftp_list || fail "SFTP didn't answer: $(cat "$DIR/sftp.err"); its settings say $(api GET /admin/sftp)"
grep -q "My files" "$DIR/sftp.txt" || fail "SFTP lists: $(cat "$DIR/sftp.txt")"

echo "== FTP: uploading to a receive folder over TLS"
api PUT /admin/ftp '{"enabled":true,"managers_publish":false,"allow_private_plaintext":false}' >/dev/null || fail "turning FTP on failed"
ftp_listening() { api GET /admin/ftp | grep -q '"listening":true'; }
retry ftp_listening || fail "FTP didn't start: $(api GET /admin/ftp)"
root=$(api GET /admin/drives | python3 -c 'import json, sys; print(next(d["root_id"] for d in json.load(sys.stdin) if d["kind"] == "company"))') ||
  fail "finding the team space failed"
folder=$(api POST /folders "{\"parent_id\":\"$root\",\"name\":\"Smoke inbox\"}" | field id) || fail "making a folder failed"
device=$(api POST /ftp/folders "{\"node_id\":\"$folder\",\"name\":\"Scanner\",\"enabled\":true,\"allow_list\":false,\"allow_read\":false,\"expires_at\":null,$confirm}") ||
  fail "making a receive folder failed"
printf 'scanned' >"$DIR/scan.txt"
curl -fsS --ssl-reqd -k --user "$(field username <<<"$device"):$(field password <<<"$device")" -T "$DIR/scan.txt" "ftp://127.0.0.1:$FTP_PORT/scan.txt" ||
  fail "uploading over FTP failed"
api GET "/nodes/$folder/children" | grep -q '"name":"scan.txt"' || fail "the file uploaded over FTP isn't in the folder"

echo "== S3: reading that file with a key of a bucket published from the folder"
api PUT /admin/s3 '{"enabled":true,"hostname":"s3.example.com","region":"us-east-1","managers_publish":false}' >/dev/null || fail "turning S3 on failed"
bucket=$(api POST /s3/buckets "{\"node_id\":\"$folder\",\"name\":\"smoke-inbox\",$confirm}" | field id) || fail "publishing a bucket failed"
key=$(api POST "/s3/buckets/$bucket/keys" "{\"description\":\"Smoke test\",\"writable\":false,\"expires_at\":null,$confirm}") || fail "making a key failed"
port=${BASE##*:}
port=${port%%/*}
got=$(curl -fsS --aws-sigv4 "aws:amz:us-east-1:s3" --user "$(field access_key <<<"$key"):$(field secret_key <<<"$key")" \
  --resolve "s3.example.com:$port:127.0.0.1" "http://s3.example.com:$port/smoke-inbox/scan.txt") || fail "reading over S3 failed"
[ "$got" = scanned ] || fail "S3 read \"$got\" instead of \"scanned\""

echo "Protocols: all good"
