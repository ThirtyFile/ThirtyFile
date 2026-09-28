-- Uploads into the content store are hashed while they arrive (upload.rs), so finishing one doesn't read the whole
-- file again. The SHA-256 state after the first `hashed` bytes is saved together with the offset: a paused upload,
-- also after a restart, continues from it. A state that doesn't match the offset (or none, for uploads started
-- before this) is rebuilt by reading the part received so far once.
ALTER TABLE uploads ADD COLUMN hash_state BLOB;
ALTER TABLE uploads ADD COLUMN hashed INTEGER NOT NULL DEFAULT 0;
