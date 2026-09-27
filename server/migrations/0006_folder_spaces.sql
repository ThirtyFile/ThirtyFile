-- Folder spaces: a space whose files are an ordinary folder on the server. The folder is the source of truth, and
-- the nodes are an index of it, kept up to date by scanning, so changes made outside ThirtyFile (SMB, rsync, a
-- scanner) show up too. Spaces of the content store (`store`) keep working as before.

-- 'store': files in a storage location, named by their content; 'folder': the files in `source_path` as they are
ALTER TABLE drives ADD COLUMN mode TEXT NOT NULL DEFAULT 'store' CHECK (mode IN ('store', 'folder'));
-- Absolute path of the folder (folder spaces)
ALTER TABLE drives ADD COLUMN source_path TEXT;
-- Browse, download and share only
ALTER TABLE drives ADD COLUMN read_only INTEGER NOT NULL DEFAULT 0;
ALTER TABLE drives ADD COLUMN last_scan_at INTEGER;
-- What the last scan found and skipped (JSON), shown in the Control panel
ALTER TABLE drives ADD COLUMN scan_report TEXT;

-- Where a node of a folder space is: its path below the space's folder ('' for the root, 'a/b.txt' below it)
ALTER TABLE nodes ADD COLUMN fs_path TEXT;
-- The file system's identity of the item: a path that disappears while the same identity shows up elsewhere was moved
ALTER TABLE nodes ADD COLUMN fs_dev INTEGER;
ALTER TABLE nodes ADD COLUMN fs_ino INTEGER;
-- Size and modification time (nanoseconds) when last indexed: a change means the file changed
ALTER TABLE nodes ADD COLUMN fs_size INTEGER;
ALTER TABLE nodes ADD COLUMN fs_mtime_ns INTEGER;

-- Names must be unique in a folder regardless of letter case in the content store, but a folder on disk can hold
-- both "A.txt" and "a.txt". The unique index uses this key: lower case in the content store, the name itself in
-- folder spaces. (lower() folds A–Z, like the COLLATE NOCASE it replaces.)
ALTER TABLE nodes ADD COLUMN name_key TEXT GENERATED ALWAYS AS (CASE WHEN fs_path IS NULL THEN lower(name) ELSE name END) VIRTUAL;
DROP INDEX nodes_name_uq;
CREATE UNIQUE INDEX nodes_name_uq ON nodes (parent_id, name_key) WHERE trashed_at IS NULL;
CREATE INDEX nodes_fs_path ON nodes (drive_id, fs_path) WHERE fs_path IS NOT NULL;
CREATE INDEX nodes_fs_ino ON nodes (fs_dev, fs_ino) WHERE fs_ino IS NOT NULL;
