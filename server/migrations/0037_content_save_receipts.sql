-- A committed online-editor save and its retry receipt share the same transaction.
CREATE TABLE content_save_receipts (
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    request_key TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    revision INTEGER NOT NULL,
    PRIMARY KEY (node_id, user_id, request_key)
);
