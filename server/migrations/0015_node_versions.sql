-- Earlier versions of files: the content a file had before it was saved over in the editor, replaced by an upload,
-- or restored to another version. In the content store a version holds a reference to its content, counted in
-- blobs.refcount like a file's, so the content stays as long as the version does. In folder spaces the old file is
-- kept in the space's folder, under `.thirtyfile-versions/<file id>/<version id>` (never indexed).
-- Versions stay with their file wherever it moves; they go when the file is deleted for good, or when the system
-- settings no longer keep them (versions.rs). They don't count toward a space's quota.
CREATE TABLE node_versions (
  id          TEXT PRIMARY KEY,
  node_id     TEXT NOT NULL,
  -- Content store: the content
  blob_hash   TEXT,
  -- Folder spaces: the space whose folder holds the version, and its path below that folder
  drive_id    TEXT,
  fs_path     TEXT,
  size        INTEGER NOT NULL,
  -- Who wrote this content (the name is copied so it stays readable after the account is deleted)
  author_id   INTEGER,
  author_name TEXT NOT NULL DEFAULT '',
  -- When the file got this content
  modified_at INTEGER NOT NULL,
  -- When it was replaced, i.e. became a version (versions are kept for a number of days from then)
  created_at  INTEGER NOT NULL
);
CREATE INDEX node_versions_node ON node_versions (node_id);
CREATE INDEX node_versions_created ON node_versions (created_at);
CREATE INDEX node_versions_blob ON node_versions (blob_hash) WHERE blob_hash IS NOT NULL;

-- Who last gave the file new content (NULL = whoever created it): the author of the version it becomes next
ALTER TABLE nodes ADD COLUMN content_by INTEGER;
