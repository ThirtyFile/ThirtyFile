-- App passwords: tokens a person creates for scripts, backups and file clients (and accounts that only sign in through
-- single sign-on), sent as `Authorization: Bearer <token>` or as the password of HTTP Basic sign-in. They work for file
-- operations only. The token is shown once when it is created; only its hash is kept.

CREATE TABLE app_passwords (
  -- Public part of the token (`tfa_<id>_<secret>`), used to find it
  id           TEXT PRIMARY KEY,
  user_id      INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  name         TEXT NOT NULL,
  -- SHA-256 of the whole token
  token_hash   TEXT NOT NULL,
  -- 'read': downloads and listings only; 'write': everything a file operation can do
  scope        TEXT NOT NULL CHECK (scope IN ('read', 'write')),
  created_at   INTEGER NOT NULL,
  -- NULL = never expires
  expires_at   INTEGER,
  -- Updated at most every few minutes
  last_used_at INTEGER,
  last_ip      TEXT NOT NULL DEFAULT ''
);
CREATE INDEX app_passwords_user ON app_passwords (user_id);
