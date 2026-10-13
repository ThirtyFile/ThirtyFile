CREATE TABLE ftp_receive_folders (
    id TEXT PRIMARY KEY,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    creator_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    generation INTEGER NOT NULL DEFAULT 1 CHECK (generation > 0),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    allow_list INTEGER NOT NULL DEFAULT 0 CHECK (allow_list IN (0, 1)),
    allow_read INTEGER NOT NULL DEFAULT 0 CHECK (allow_read IN (0, 1)),
    expires_at INTEGER,
    created_at INTEGER NOT NULL,
    last_used_at INTEGER
);
CREATE INDEX ftp_receive_folders_node ON ftp_receive_folders(node_id);
