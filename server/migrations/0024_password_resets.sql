-- Accounts whose password an administrator chose (a new account, a reset): the person sets their own at the next
-- sign-in before doing anything else
ALTER TABLE users ADD COLUMN must_change_password INTEGER NOT NULL DEFAULT 0;

-- "Forgot password": single-use links sent by email, kept by the SHA-256 of their token, valid for an hour
CREATE TABLE password_resets (
  token_hash TEXT PRIMARY KEY,
  user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL
);
CREATE INDEX password_resets_user ON password_resets (user_id);
