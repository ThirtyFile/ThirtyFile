-- Backups made again and again (backups/policy.rs): a policy is a set (backup_sets, kind 'policy') that snapshots the
-- spaces of a location on a schedule, soon after they change, or both, and keeps its snapshots for a number of days.
CREATE TABLE backup_policies (
  set_id         TEXT PRIMARY KEY REFERENCES backup_sets (id) ON DELETE CASCADE,
  -- Paused policies make no snapshots; what they made stays
  enabled        INTEGER NOT NULL DEFAULT 1,
  -- 'realtime': soon after the spaces change; 'scheduled': on the schedule; 'both'
  mode           TEXT NOT NULL CHECK (mode IN ('realtime', 'scheduled', 'both')),
  -- JSON: {"every": minutes}, {"daily": "HH:MM"} or {"weekly": "HH:MM", "days": [1, …]} (Monday is 1), in `tz`
  schedule       TEXT NOT NULL DEFAULT '{"daily":"03:00"}',
  -- A time zone name (IANA, e.g. Asia/Taipei)
  tz             TEXT NOT NULL DEFAULT 'UTC',
  -- 1: every space on the set's source location, those added later too; 0: the spaces of backup_policy_spaces
  all_spaces     INTEGER NOT NULL DEFAULT 1,
  versions       INTEGER NOT NULL DEFAULT 1,
  trash          INTEGER NOT NULL DEFAULT 1,
  -- Complete snapshots older than this many days are deleted, except the newest `keep_min`
  keep_days      INTEGER NOT NULL DEFAULT 30,
  keep_min       INTEGER NOT NULL DEFAULT 1 CHECK (keep_min >= 1),
  -- Bytes per second a snapshot may copy (0: no limit)
  rate_limit     INTEGER NOT NULL DEFAULT 0,
  -- Hours without a new complete snapshot after which administrators are told (0: never)
  alert_hours    INTEGER NOT NULL DEFAULT 48,
  -- Days between checks that read the backup back (0: only when asked)
  verify_days    INTEGER NOT NULL DEFAULT 7,
  -- The next scheduled snapshot (Unix seconds); NULL without a schedule
  next_run_at    INTEGER,
  last_run_at    INTEGER,
  last_verify_at INTEGER,
  -- Asked for (by the schedule or a change) while a snapshot was being made: one more snapshot after it
  catch_up       INTEGER NOT NULL DEFAULT 0,
  -- What administrators were last told about (overdue, failing), so the same trouble isn't told twice
  alerted        TEXT NOT NULL DEFAULT '',
  created_at     INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL
);

-- The spaces a policy backs up when it doesn't take every space of its location
CREATE TABLE backup_policy_spaces (
  set_id   TEXT NOT NULL REFERENCES backup_sets (id) ON DELETE CASCADE,
  drive_id TEXT NOT NULL,
  PRIMARY KEY (set_id, drive_id)
) WITHOUT ROWID;

-- Changes to each space, counted by the triggers below within the transaction that makes them: one row per space,
-- whose `seq` grows with every change of its items, earlier versions, access or settings (a scan of a folder space
-- changes it too). The record can't grow with the changes, so no change is ever lost by running out of room.
CREATE TABLE space_changes (
  drive_id   TEXT PRIMARY KEY,
  seq        INTEGER NOT NULL,
  changed_at INTEGER NOT NULL
) WITHOUT ROWID;

-- The `seq` of each space a set's newest complete snapshot holds: a space whose `seq` is higher changed since
CREATE TABLE backup_captured (
  set_id   TEXT NOT NULL REFERENCES backup_sets (id) ON DELETE CASCADE,
  drive_id TEXT NOT NULL,
  seq      INTEGER NOT NULL,
  PRIMARY KEY (set_id, drive_id)
) WITHOUT ROWID;

-- When a policy first saw a change of a space that no snapshot holds yet: how far behind it is
CREATE TABLE backup_dirty (
  set_id   TEXT NOT NULL REFERENCES backup_sets (id) ON DELETE CASCADE,
  drive_id TEXT NOT NULL,
  since    INTEGER NOT NULL,
  PRIMARY KEY (set_id, drive_id)
) WITHOUT ROWID;

-- The `seq` of each space as a snapshot read it (JSON {"<space>": seq}), recorded as captured once it is complete
ALTER TABLE backup_snapshots ADD COLUMN changes TEXT NOT NULL DEFAULT '{}';

CREATE TRIGGER space_changes_node_insert AFTER INSERT ON nodes WHEN NEW.drive_id IS NOT NULL BEGIN
  INSERT INTO space_changes (drive_id, seq, changed_at) VALUES (NEW.drive_id, (SELECT COALESCE(MAX(seq), 0) + 1 FROM space_changes), unixepoch())
  ON CONFLICT (drive_id) DO UPDATE SET seq = excluded.seq, changed_at = excluded.changed_at;
END;
CREATE TRIGGER space_changes_node_update AFTER UPDATE ON nodes WHEN COALESCE(NEW.drive_id, OLD.drive_id) IS NOT NULL BEGIN
  INSERT INTO space_changes (drive_id, seq, changed_at)
  SELECT d, (SELECT COALESCE(MAX(seq), 0) + 1 FROM space_changes), unixepoch() FROM (SELECT NEW.drive_id AS d UNION SELECT OLD.drive_id) WHERE d IS NOT NULL
  ON CONFLICT (drive_id) DO UPDATE SET seq = excluded.seq, changed_at = excluded.changed_at;
END;
CREATE TRIGGER space_changes_node_delete AFTER DELETE ON nodes WHEN OLD.drive_id IS NOT NULL BEGIN
  INSERT INTO space_changes (drive_id, seq, changed_at) VALUES (OLD.drive_id, (SELECT COALESCE(MAX(seq), 0) + 1 FROM space_changes), unixepoch())
  ON CONFLICT (drive_id) DO UPDATE SET seq = excluded.seq, changed_at = excluded.changed_at;
END;
CREATE TRIGGER space_changes_version_insert AFTER INSERT ON node_versions BEGIN
  INSERT INTO space_changes (drive_id, seq, changed_at)
  SELECT d, (SELECT COALESCE(MAX(seq), 0) + 1 FROM space_changes), unixepoch()
  FROM (SELECT COALESCE(NEW.drive_id, (SELECT drive_id FROM nodes WHERE id = NEW.node_id)) AS d) WHERE d IS NOT NULL
  ON CONFLICT (drive_id) DO UPDATE SET seq = excluded.seq, changed_at = excluded.changed_at;
END;
CREATE TRIGGER space_changes_version_delete AFTER DELETE ON node_versions BEGIN
  INSERT INTO space_changes (drive_id, seq, changed_at)
  SELECT d, (SELECT COALESCE(MAX(seq), 0) + 1 FROM space_changes), unixepoch()
  FROM (SELECT COALESCE(OLD.drive_id, (SELECT drive_id FROM nodes WHERE id = OLD.node_id)) AS d) WHERE d IS NOT NULL
  ON CONFLICT (drive_id) DO UPDATE SET seq = excluded.seq, changed_at = excluded.changed_at;
END;
CREATE TRIGGER space_changes_grant_insert AFTER INSERT ON grants BEGIN
  INSERT INTO space_changes (drive_id, seq, changed_at)
  SELECT d, (SELECT COALESCE(MAX(seq), 0) + 1 FROM space_changes), unixepoch() FROM (SELECT drive_id AS d FROM nodes WHERE id = NEW.node_id) WHERE d IS NOT NULL
  ON CONFLICT (drive_id) DO UPDATE SET seq = excluded.seq, changed_at = excluded.changed_at;
END;
CREATE TRIGGER space_changes_grant_update AFTER UPDATE ON grants BEGIN
  INSERT INTO space_changes (drive_id, seq, changed_at)
  SELECT d, (SELECT COALESCE(MAX(seq), 0) + 1 FROM space_changes), unixepoch() FROM (SELECT drive_id AS d FROM nodes WHERE id = NEW.node_id) WHERE d IS NOT NULL
  ON CONFLICT (drive_id) DO UPDATE SET seq = excluded.seq, changed_at = excluded.changed_at;
END;
CREATE TRIGGER space_changes_grant_delete AFTER DELETE ON grants BEGIN
  INSERT INTO space_changes (drive_id, seq, changed_at)
  SELECT d, (SELECT COALESCE(MAX(seq), 0) + 1 FROM space_changes), unixepoch() FROM (SELECT drive_id AS d FROM nodes WHERE id = OLD.node_id) WHERE d IS NOT NULL
  ON CONFLICT (drive_id) DO UPDATE SET seq = excluded.seq, changed_at = excluded.changed_at;
END;
CREATE TRIGGER space_changes_drive_update AFTER UPDATE ON drives BEGIN
  INSERT INTO space_changes (drive_id, seq, changed_at) VALUES (NEW.id, (SELECT COALESCE(MAX(seq), 0) + 1 FROM space_changes), unixepoch())
  ON CONFLICT (drive_id) DO UPDATE SET seq = excluded.seq, changed_at = excluded.changed_at;
END;
