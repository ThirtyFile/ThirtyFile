-- Search inside files (Control panel › General › Search; contentsearch/): the text read from files, found through a
-- trigram index like the names are (so any part of a word matches, in every language).
--
-- One row per file, for its current content. `content_fts` holds the text itself; `content_index` says which file each
-- row is, which content it was read from (`stamp`) and how it went. Nothing here is filled until an administrator
-- turns the feature on.
CREATE VIRTUAL TABLE content_fts USING fts5(body, tokenize='trigram remove_diacritics 1');

CREATE TABLE content_index (
  -- No foreign key: the triggers below clear the row with its file, in the same statement
  node_id    TEXT PRIMARY KEY,
  -- The content it was read from: the stored content's hash, or the size and modification time of a folder space's file.
  -- Empty after the file was renamed (the kind of text it holds may have changed), so it is read again
  stamp      TEXT NOT NULL,
  -- indexed: its text is in content_fts; skipped: nothing to read in it (a picture, a kind of file not read, too large,
  -- no text); failed: it could not be read
  state      TEXT NOT NULL CHECK (state IN ('indexed', 'skipped', 'failed')),
  -- content_fts's row, when indexed
  fts_rowid  INTEGER,
  -- Bytes of text kept
  bytes      INTEGER NOT NULL DEFAULT 0,
  -- A short reason when skipped or failed (English, from the server)
  note       TEXT NOT NULL DEFAULT '',
  indexed_at INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX content_index_state ON content_index (state);

-- Files waiting to be read, oldest first. Filled by the triggers below while the feature is on, and by turning it on
-- or asking to index again.
CREATE TABLE content_queue (
  node_id     TEXT PRIMARY KEY,
  queued_at   INTEGER NOT NULL,
  -- Reading failed for now (a storage location that doesn't answer): tried again after this time
  not_before  INTEGER NOT NULL DEFAULT 0,
  attempts    INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;
CREATE INDEX content_queue_due ON content_queue (not_before, queued_at);

-- Whether the triggers fill the queue: one row, kept equal to the setting by the server
CREATE TABLE content_switch (
  on_ INTEGER NOT NULL DEFAULT 0 CHECK (on_ IN (0, 1))
);
INSERT INTO content_switch (on_) VALUES (0);

CREATE TRIGGER nodes_content_added AFTER INSERT ON nodes
WHEN new.kind = 'file' AND new.trashed_at IS NULL AND (SELECT on_ FROM content_switch) = 1
BEGIN
  INSERT OR IGNORE INTO content_queue (node_id, queued_at) VALUES (new.id, unixepoch());
END;

-- New content (a save, a restored version, a scan that found the file changed), or back from the trash
CREATE TRIGGER nodes_content_changed AFTER UPDATE OF blob_hash, fs_size, fs_mtime_ns, trashed_at ON nodes
WHEN new.kind = 'file' AND new.trashed_at IS NULL AND (SELECT on_ FROM content_switch) = 1
BEGIN
  INSERT OR IGNORE INTO content_queue (node_id, queued_at) VALUES (new.id, unixepoch());
END;

-- A new name may be a kind of file that is read (or no longer is): read again, the text found so far stays meanwhile
CREATE TRIGGER nodes_content_renamed AFTER UPDATE OF name ON nodes
WHEN new.kind = 'file' AND (SELECT on_ FROM content_switch) = 1
BEGIN
  UPDATE content_index SET stamp = '' WHERE node_id = new.id;
  INSERT OR IGNORE INTO content_queue (node_id, queued_at) VALUES (new.id, unixepoch());
END;

CREATE TRIGGER nodes_content_removed AFTER DELETE ON nodes
BEGIN
  DELETE FROM content_fts WHERE rowid = (SELECT fts_rowid FROM content_index WHERE node_id = old.id);
  DELETE FROM content_index WHERE node_id = old.id;
  DELETE FROM content_queue WHERE node_id = old.id;
END;
