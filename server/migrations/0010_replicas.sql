-- Replicas (replicas/): copies of the current content of spaces, kept on other storage locations and checked, so the
-- files can still be read when their location fails, and another location can take over as theirs (a promotion). A
-- replica isn't a backup: deleting or changing a file changes its replicas too.

-- What is replicated: the spaces of a location (its primary), to other locations (replica_targets)
CREATE TABLE replica_policies (
  id              TEXT PRIMARY KEY,
  name            TEXT NOT NULL,
  -- The location the spaces are on; a promotion makes it another
  source_location TEXT NOT NULL REFERENCES storage_locations (id),
  -- Paused policies copy nothing new; the copies they made stay
  enabled         INTEGER NOT NULL DEFAULT 1,
  -- 1: every space on the source location, those added later too; 0: the spaces of replica_policy_spaces
  all_spaces      INTEGER NOT NULL DEFAULT 1,
  -- Copies wanted besides the primary: each content is kept on the first `copies` targets (by priority) that aren't
  -- where its primary is
  copies          INTEGER NOT NULL DEFAULT 1 CHECK (copies >= 1),
  -- Files whose location can't be read are read from a checked replica of the same content
  read_fallback   INTEGER NOT NULL DEFAULT 1,
  -- Days between checks that read the replicas back (0: only when asked)
  verify_days     INTEGER NOT NULL DEFAULT 1,
  -- Hours a replica may be behind, or not working, before administrators are told (0: never)
  alert_hours     INTEGER NOT NULL DEFAULT 24,
  -- Bytes per second a sync may copy (0: no limit)
  rate_limit      INTEGER NOT NULL DEFAULT 0,
  -- Grows with every promotion: a job queued before it is refused
  epoch           INTEGER NOT NULL DEFAULT 1,
  -- What administrators were last told about, so the same trouble isn't told twice
  alerted         TEXT NOT NULL DEFAULT '',
  created_by      INTEGER,
  created_by_name TEXT NOT NULL DEFAULT '',
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL
);

-- The locations a policy keeps copies on, in order of priority
CREATE TABLE replica_targets (
  policy_id      TEXT NOT NULL REFERENCES replica_policies (id) ON DELETE CASCADE,
  location_id    TEXT NOT NULL REFERENCES storage_locations (id),
  priority       INTEGER NOT NULL,
  -- 'realtime': soon after changes; 'scheduled': on `schedule` (as in backup_policies) in `tz`
  mode           TEXT NOT NULL DEFAULT 'realtime' CHECK (mode IN ('realtime', 'scheduled')),
  schedule       TEXT NOT NULL DEFAULT '{"daily":"03:00"}',
  tz             TEXT NOT NULL DEFAULT 'UTC',
  next_run_at    INTEGER,
  -- 'active', or 'stale': the old primary after a promotion, whose copies are checked before it counts again
  state          TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'stale')),
  -- When a sync last copied everything the target should hold, and when one last ran or was asked for
  synced_at      INTEGER,
  last_run_at    INTEGER,
  last_verify_at INTEGER,
  -- Asked for while a sync ran: one more after it
  catch_up       INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (policy_id, location_id)
) WITHOUT ROWID;

-- The spaces a policy replicates when it doesn't take every space of its location
CREATE TABLE replica_policy_spaces (
  policy_id TEXT NOT NULL REFERENCES replica_policies (id) ON DELETE CASCADE,
  drive_id  TEXT NOT NULL,
  PRIMARY KEY (policy_id, drive_id)
) WITHOUT ROWID;

-- Copies of content ThirtyFile keeps on a location besides where the content is (blobs.location_id), where that
-- location keeps content (its content store): background deletion and removing unused content leave them alone while
-- the row is here (tree::claim_for_deletion); a copy no longer needed loses its row first, then is deleted like any
-- content nothing uses. 'verified': stored or found, and checked; 'stale': an old primary's copy after a promotion,
-- not checked since; 'corrupt': a check found it missing or damaged (repaired from a checked copy).
CREATE TABLE replica_copies (
  hash        TEXT NOT NULL,
  location_id TEXT NOT NULL,
  size        INTEGER NOT NULL,
  state       TEXT NOT NULL DEFAULT 'verified' CHECK (state IN ('verified', 'stale', 'corrupt')),
  created_at  INTEGER NOT NULL,
  verified_at INTEGER,
  PRIMARY KEY (hash, location_id)
) WITHOUT ROWID;
CREATE INDEX replica_copies_location ON replica_copies (location_id, hash);

-- The `seq` of each space (space_changes) a target's last complete sync held, and when a change not held yet was first
-- seen: how far behind it is
CREATE TABLE replica_captured (
  policy_id   TEXT NOT NULL REFERENCES replica_policies (id) ON DELETE CASCADE,
  location_id TEXT NOT NULL,
  drive_id    TEXT NOT NULL,
  seq         INTEGER NOT NULL,
  PRIMARY KEY (policy_id, location_id, drive_id)
) WITHOUT ROWID;
CREATE TABLE replica_dirty (
  policy_id   TEXT NOT NULL REFERENCES replica_policies (id) ON DELETE CASCADE,
  location_id TEXT NOT NULL,
  drive_id    TEXT NOT NULL,
  since       INTEGER NOT NULL,
  PRIMARY KEY (policy_id, location_id, drive_id)
) WITHOUT ROWID;

-- Work on replicas, run in the background one at a time and kept afterwards as their history (as backup_jobs)
CREATE TABLE replica_jobs (
  id              TEXT PRIMARY KEY,
  -- 'sync': copies what the target should hold and lets go of what it no longer should; 'verify': reads the target's
  -- copies back and checks them
  kind            TEXT NOT NULL CHECK (kind IN ('sync', 'verify')),
  policy_id       TEXT NOT NULL,
  location_id     TEXT,
  state           TEXT NOT NULL DEFAULT 'queued' CHECK (state IN ('queued', 'running', 'paused', 'waiting', 'failed', 'done', 'cancelled')),
  -- JSON: the policy's epoch when it was asked for (a job from before a promotion is refused), and how it was asked for
  params          TEXT NOT NULL DEFAULT '{}',
  label           TEXT NOT NULL DEFAULT '',
  files_total     INTEGER NOT NULL DEFAULT 0,
  bytes_total     INTEGER NOT NULL DEFAULT 0,
  files_done      INTEGER NOT NULL DEFAULT 0,
  bytes_done      INTEGER NOT NULL DEFAULT 0,
  failed_items    INTEGER NOT NULL DEFAULT 0,
  failures        TEXT NOT NULL DEFAULT '[]',
  error           TEXT,
  note            TEXT,
  created_by      INTEGER,
  created_by_name TEXT NOT NULL DEFAULT '',
  created_at      INTEGER NOT NULL,
  started_at      INTEGER,
  finished_at     INTEGER
);
CREATE INDEX replica_jobs_state ON replica_jobs (state, created_at);
CREATE INDEX replica_jobs_policy ON replica_jobs (policy_id);

-- Files and earlier versions of folder spaces as a sync last read them for replicas: where, their size, modification
-- time and identity then, and the SHA-256 of their content, which is kept on the targets like any content. A row counts
-- only while the index still has the item as it was read; a file another program changed is read again once the check
-- for changes has seen it. Rows of items that are gone are removed by the next sync.
CREATE TABLE replica_folder_files (
  item_id  TEXT PRIMARY KEY,
  drive_id TEXT NOT NULL,
  path     TEXT NOT NULL,
  size     INTEGER NOT NULL,
  mtime_ns INTEGER,
  dev      INTEGER,
  ino      INTEGER,
  hash     TEXT NOT NULL,
  read_at  INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX replica_folder_files_drive ON replica_folder_files (drive_id, hash);
CREATE INDEX replica_folder_files_hash ON replica_folder_files (hash);
