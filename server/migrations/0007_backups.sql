-- Copies of spaces kept on another storage location (backups/): "Copy everything to…" makes one. A set is what
-- ThirtyFile keeps in a folder of its own on the destination, `.thirtyfile-backups/<set id>/`: each content once (named
-- by its SHA-256, `objects/ab/cd/<hash>`), and snapshots (`snapshots/<id>/manifest.jsonl`, then `complete.json`) that
-- say which spaces, folders, files and earlier versions there were, and which content each file had. Nothing else in
-- ThirtyFile writes or deletes in that folder: removing unused content and deleting content nothing uses only ever
-- look at a location's content store.
CREATE TABLE backup_sets (
  id              TEXT PRIMARY KEY,
  -- 'copy': made once by "Copy everything to…"; 'policy' and 'imported' are kept for backups made on a schedule and
  -- sets found on a location
  kind            TEXT NOT NULL CHECK (kind IN ('copy', 'policy', 'imported')),
  name            TEXT NOT NULL,
  -- The location whose spaces were copied, and its name then (no foreign key: the copy outlives it)
  source_location TEXT,
  source_name     TEXT NOT NULL DEFAULT '',
  -- Where the set is kept. A location holding sets can't be deleted (locations.rs).
  dest_location   TEXT NOT NULL REFERENCES storage_locations (id),
  created_by      INTEGER,
  created_by_name TEXT NOT NULL DEFAULT '',
  created_at      INTEGER NOT NULL,
  -- Being deleted from its location (a 'remove' job): nothing is added to it, and nothing is restored from it
  removing        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX backup_sets_dest ON backup_sets (dest_location);

-- What a set holds at a point in time. 'making' until its manifest and then its completion marker are written on the
-- destination: only a 'complete' snapshot is a restore point.
CREATE TABLE backup_snapshots (
  id              TEXT PRIMARY KEY,
  set_id          TEXT NOT NULL REFERENCES backup_sets (id) ON DELETE CASCADE,
  state           TEXT NOT NULL DEFAULT 'making' CHECK (state IN ('making', 'complete')),
  -- When the spaces were read for the manifest: the snapshot shows them as they were then
  cutoff          INTEGER,
  -- The spaces it holds (JSON: [{"id", "name", "kind", "owner", "mode", "files", "bytes"}]), for listing them without
  -- reading the manifest. Personal spaces are named as elsewhere: "My files" and the owner's user name.
  space_list      TEXT NOT NULL DEFAULT '[]',
  folders         INTEGER NOT NULL DEFAULT 0,
  files           INTEGER NOT NULL DEFAULT 0,
  versions        INTEGER NOT NULL DEFAULT 0,
  -- Bytes of its files and versions, identical content counted every time
  logical_bytes   INTEGER NOT NULL DEFAULT 0,
  -- The manifest as written on the destination: its SHA-256 and size (restores check it)
  manifest_sha256 TEXT,
  manifest_size   INTEGER,
  created_at      INTEGER NOT NULL,
  completed_at    INTEGER
);
CREATE INDEX backup_snapshots_set ON backup_snapshots (set_id, state);

-- Work on sets, run in the background one at a time and kept afterwards as their history. Names are copied so the
-- history stays readable after sets, spaces and users are deleted.
CREATE TABLE backup_jobs (
  id              TEXT PRIMARY KEY,
  -- 'snapshot': copies spaces into the set and writes a snapshot; 'restore': brings a snapshot's files back into a
  -- space; 'verify': reads a snapshot's content back from the destination and checks it; 'remove': deletes the set
  -- from its destination
  kind            TEXT NOT NULL CHECK (kind IN ('snapshot', 'restore', 'verify', 'remove')),
  set_id          TEXT NOT NULL,
  snapshot_id     TEXT,
  -- queued: waiting for its turn; running; paused and failed: stopped with what was done kept, until resumed or
  -- cancelled; waiting: the destination can't be reached, tried again later; done; cancelled
  state           TEXT NOT NULL DEFAULT 'queued' CHECK (state IN ('queued', 'running', 'paused', 'waiting', 'failed', 'done', 'cancelled')),
  -- What it works on (JSON): a snapshot's spaces and whether earlier versions and the trash are included; a restore's
  -- space, folder and target
  params          TEXT NOT NULL DEFAULT '{}',
  -- The set's name, and for a restore the space's (a personal space: "My files" and its owner's user name)
  label           TEXT NOT NULL DEFAULT '',
  files_total     INTEGER NOT NULL DEFAULT 0,
  bytes_total     INTEGER NOT NULL DEFAULT 0,
  files_done      INTEGER NOT NULL DEFAULT 0,
  bytes_done      INTEGER NOT NULL DEFAULT 0,
  -- Items that couldn't be copied after a few tries: how many, and the first of them as JSON
  -- ([{"item": path or null, "error": …}]; never a name from a personal space)
  failed_items    INTEGER NOT NULL DEFAULT 0,
  failures        TEXT NOT NULL DEFAULT '[]',
  error           TEXT,
  -- What a finished job wants people to know (a restore that gave items new names, say)
  note            TEXT,
  created_by      INTEGER,
  created_by_name TEXT NOT NULL DEFAULT '',
  created_at      INTEGER NOT NULL,
  started_at      INTEGER,
  finished_at     INTEGER
);
CREATE INDEX backup_jobs_state ON backup_jobs (state, created_at);
CREATE INDEX backup_jobs_set ON backup_jobs (set_id);

-- The content a set holds on its destination, each once, written and checked (its SHA-256 while it was read, its
-- size once stored). `verified_at`: when a 'verify' job last read it back and found it whole.
CREATE TABLE backup_objects (
  set_id      TEXT NOT NULL,
  hash        TEXT NOT NULL,
  size        INTEGER NOT NULL,
  created_at  INTEGER NOT NULL,
  verified_at INTEGER,
  PRIMARY KEY (set_id, hash)
) WITHOUT ROWID;

-- Content a snapshot job still has to copy, as its manifest records it: kept from deletion until the job has copied
-- it, or has ended (tree::claim_for_deletion), so the snapshot can hold what the spaces had at its cutoff. `location`:
-- where the content was when the manifest was written.
CREATE TABLE backup_pending (
  job_id   TEXT NOT NULL,
  hash     TEXT NOT NULL,
  size     INTEGER NOT NULL,
  location TEXT NOT NULL,
  PRIMARY KEY (job_id, hash)
) WITHOUT ROWID;
CREATE INDEX backup_pending_hash ON backup_pending (hash);

-- Files of folder spaces (and the earlier versions kept in their folders) as a set last read them: their content,
-- with the path, size, modification time and identity they had then, so a file that is still the same isn't read
-- again. `item_id`: the file's node, or the version.
CREATE TABLE backup_folder_files (
  set_id   TEXT NOT NULL,
  item_id  TEXT NOT NULL,
  drive_id TEXT NOT NULL,
  path     TEXT NOT NULL,
  size     INTEGER NOT NULL,
  mtime_ns INTEGER,
  dev      INTEGER,
  ino      INTEGER,
  hash     TEXT NOT NULL,
  PRIMARY KEY (set_id, item_id)
) WITHOUT ROWID;

-- What a restore job brought back so far, by the item's id in the snapshot: a restore that stops continues without
-- making anything twice. The rows go when the job has ended.
CREATE TABLE backup_restored (
  job_id    TEXT NOT NULL,
  source_id TEXT NOT NULL,
  node_id   TEXT NOT NULL,
  PRIMARY KEY (job_id, source_id)
) WITHOUT ROWID;
