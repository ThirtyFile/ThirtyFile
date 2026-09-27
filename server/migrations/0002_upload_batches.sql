-- Folders created while uploading a folder, per upload batch.
-- Each file of an uploaded folder is its own upload. When a folder name on the way is already used by a file, the
-- first upload of the batch creates a numbered folder ("Photos (1)") and the others must find that same folder again.

ALTER TABLE uploads ADD COLUMN batch TEXT NOT NULL DEFAULT '';

CREATE TABLE upload_batch_folders (
  batch      TEXT NOT NULL,
  parent_id  TEXT NOT NULL,
  -- The name the uploaded folder has on the uploader's computer
  name       TEXT NOT NULL,
  folder_id  TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (batch, parent_id, name)
);
CREATE INDEX upload_batch_folders_created ON upload_batch_folders (created_at);
