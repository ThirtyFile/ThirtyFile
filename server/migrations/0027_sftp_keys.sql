CREATE TABLE sftp_keys (
    id TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    public_key TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    last_used_at INTEGER,
    UNIQUE(user_id, fingerprint)
);
CREATE INDEX sftp_keys_user ON sftp_keys(user_id);
