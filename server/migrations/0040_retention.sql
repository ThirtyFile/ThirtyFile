-- Retention periods and legal holds (tree/kept.rs): what must not be deleted for good, and for how long.
-- A space's retention period: its files, and their earlier versions, can't be deleted for good until this many days
-- after their last change (0 = none)
ALTER TABLE drives ADD COLUMN retention_days INTEGER NOT NULL DEFAULT 0;

-- A hold keeps everything it covers from being deleted for good, whatever people do, until an administrator releases
-- it. It covers a folder or file and what is in it ('node'), a whole space ('space') or the files a person owns
-- ('user'). No foreign keys: a hold outlives what it names, and says so, rather than vanishing with it.
CREATE TABLE holds (
  id           TEXT PRIMARY KEY,
  scope        TEXT NOT NULL CHECK (scope IN ('node', 'space', 'user')),
  node_id      TEXT,
  drive_id     TEXT,
  user_id      INTEGER,
  -- What it covered when it was placed, for the list (a name can change, an account can go)
  label        TEXT NOT NULL,
  reason       TEXT NOT NULL,
  created_by   INTEGER,
  created_by_name TEXT NOT NULL,
  created_at   INTEGER NOT NULL,
  released_at  INTEGER,
  released_by_name TEXT,
  CHECK ((scope = 'node' AND node_id IS NOT NULL) OR (scope = 'space' AND drive_id IS NOT NULL) OR (scope = 'user' AND user_id IS NOT NULL))
);
CREATE INDEX holds_node ON holds (node_id) WHERE released_at IS NULL;
CREATE INDEX holds_drive ON holds (drive_id) WHERE released_at IS NULL;
CREATE INDEX holds_user ON holds (user_id) WHERE released_at IS NULL;
