-- Changes to an item and everything in it that are made a batch at a time, so the write lock is never held for long
-- (tree/changes.rs): deleting for good, moving to the trash, restoring, and renaming or moving in a folder space. The
-- first batch is made with the change itself, together with this row; the rest follows in the background, a
-- transaction per batch, and at the next start when the server stopped meanwhile. The row goes with the last batch.
CREATE TABLE tree_changes (
  id         TEXT PRIMARY KEY,
  -- 'purge': delete node_id and everything in it for good (the trash no longer lists it meanwhile)
  -- 'trash': mark everything below node_id as in the trash with it (trash_id, trashed_at)
  -- 'restore': take everything below node_id out of the trash (its trash_id)
  -- 'repath': items of drive_id at old_path, or below it, are at new_path now
  kind       TEXT NOT NULL CHECK (kind IN ('purge', 'trash', 'restore', 'repath')),
  node_id    TEXT NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
  drive_id   TEXT,
  trash_id   TEXT,
  trashed_at INTEGER,
  old_path   TEXT,
  new_path   TEXT,
  created_at INTEGER NOT NULL
);
CREATE INDEX tree_changes_node ON tree_changes (node_id);
