-- Signed-in devices: what each sign-in session is, so people can see where they are signed in and sign a device out
-- (under "My account › Devices", and administrators per user).

-- A public id for each session: the token hash stays on the server
ALTER TABLE sessions ADD COLUMN id TEXT;
UPDATE sessions SET id = lower(hex(randomblob(16)));
CREATE UNIQUE INDEX sessions_id ON sessions (id);
-- The browser that signed in (User-Agent, at most 300 characters)
ALTER TABLE sessions ADD COLUMN user_agent TEXT NOT NULL DEFAULT '';
-- The address the device used most recently
ALTER TABLE sessions ADD COLUMN ip TEXT NOT NULL DEFAULT '';
-- How it signed in: password, microsoft, google or github
ALTER TABLE sessions ADD COLUMN method TEXT NOT NULL DEFAULT 'password';
-- Updated at most every few minutes, not on every request
ALTER TABLE sessions ADD COLUMN last_used_at INTEGER;
UPDATE sessions SET last_used_at = created_at;
