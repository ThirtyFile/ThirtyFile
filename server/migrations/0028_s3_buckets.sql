-- A published bucket follows the folder's identity, not its name or path.
CREATE TABLE s3_buckets (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE CHECK(length(name) BETWEEN 3 AND 63),
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    creator_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0, 1)),
    created_at INTEGER NOT NULL,
    last_used_at INTEGER
);
CREATE INDEX s3_buckets_node ON s3_buckets(node_id);
CREATE TABLE s3_keys (
    id TEXT PRIMARY KEY,
    bucket_id TEXT NOT NULL REFERENCES s3_buckets(id) ON DELETE CASCADE,
    access_key TEXT NOT NULL UNIQUE,
    secret TEXT NOT NULL,
    description TEXT NOT NULL,
    writable INTEGER NOT NULL CHECK(writable IN (0, 1)),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0, 1)),
    expires_at INTEGER,
    created_at INTEGER NOT NULL,
    last_used_at INTEGER,
    expiry_notified INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX s3_keys_bucket ON s3_keys(bucket_id);
CREATE TABLE s3_uploads (
    id TEXT PRIMARY KEY,
    bucket_id TEXT NOT NULL REFERENCES s3_buckets(id) ON DELETE CASCADE,
    key_id TEXT NOT NULL REFERENCES s3_keys(id) ON DELETE CASCADE,
    object_key TEXT NOT NULL,
    mtime TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX s3_uploads_bucket_key ON s3_uploads(bucket_id, object_key COLLATE BINARY, id);
CREATE TABLE s3_parts (
    upload_id TEXT NOT NULL REFERENCES s3_uploads(id) ON DELETE CASCADE,
    number INTEGER NOT NULL CHECK(number BETWEEN 1 AND 10000),
    file_id TEXT NOT NULL,
    size INTEGER NOT NULL,
    md5 TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(upload_id, number)
);
-- Cascades and cancelled management requests cannot lose the names of files awaiting removal.
CREATE TABLE s3_part_gc (file_id TEXT PRIMARY KEY);
CREATE TRIGGER s3_part_deleted AFTER DELETE ON s3_parts BEGIN
    INSERT OR IGNORE INTO s3_part_gc(file_id) VALUES(OLD.file_id);
END;
CREATE TRIGGER s3_part_replaced AFTER UPDATE OF file_id ON s3_parts
WHEN NEW.file_id != OLD.file_id BEGIN
    INSERT OR IGNORE INTO s3_part_gc(file_id) VALUES(OLD.file_id);
END;
ALTER TABLE nodes ADD COLUMN content_md5 TEXT;
ALTER TABLE nodes ADD COLUMN md5_version TEXT;
ALTER TABLE nodes ADD COLUMN s3_mtime TEXT;
ALTER TABLE nodes ADD COLUMN s3_mtime_version TEXT;
-- Changes made by scans, copies, restores or future adapters must never retain a stale validator.
CREATE TRIGGER nodes_md5_invalidated AFTER UPDATE OF blob_hash, size, updated_at ON nodes
WHEN NEW.blob_hash IS NOT OLD.blob_hash OR NEW.size != OLD.size OR NEW.updated_at != OLD.updated_at
BEGIN
    UPDATE nodes SET content_md5 = NULL, md5_version = NULL, s3_mtime = NULL, s3_mtime_version = NULL WHERE id = NEW.id;
END;
