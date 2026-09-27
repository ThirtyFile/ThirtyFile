-- Two-factor sign-in: after a correct password, a code from an authenticator app (TOTP, RFC 6238) or a single-use
-- recovery code. Single sign-on and app passwords don't ask for it.

-- The shared secret (base32); NULL = not set up. Unlike passwords it has to stay readable to check codes against it.
ALTER TABLE users ADD COLUMN totp_secret TEXT;
-- The 30-second step of the last accepted code: a code (or an older one) can't be used again
ALTER TABLE users ADD COLUMN totp_last_step INTEGER NOT NULL DEFAULT 0;

-- Single-use recovery codes, for when the authenticator is lost; only their SHA-256 is stored.
CREATE TABLE recovery_codes (
  user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  code_hash  TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  used_at    INTEGER,
  PRIMARY KEY (user_id, code_hash)
);
