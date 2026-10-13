-- The Contributor role (between Viewer and Editor): adds items, and changes, moves and deletes only those they added.
-- `grants.role` has a CHECK listing the roles, which SQLite can't change in place: the table is made again with the
-- new role allowed, keeping every grant and its id, then its index and the triggers of 0008 that note grant changes
-- for backups. Nothing refers to grants, so nothing else changes.
CREATE TABLE grants_new (
  id             INTEGER PRIMARY KEY,
  node_id        TEXT NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
  principal_type TEXT NOT NULL CHECK (principal_type IN ('user', 'group', 'everyone')),
  principal_id   INTEGER NOT NULL DEFAULT 0,
  role           TEXT NOT NULL CHECK (role IN ('viewer', 'contributor', 'editor', 'manager', 'owner')),
  granted_by     INTEGER,
  created_at     INTEGER NOT NULL,
  expires_at     INTEGER,
  UNIQUE (node_id, principal_type, principal_id)
);
INSERT INTO grants_new (id, node_id, principal_type, principal_id, role, granted_by, created_at, expires_at)
  SELECT id, node_id, principal_type, principal_id, role, granted_by, created_at, expires_at FROM grants;
DROP TABLE grants;
ALTER TABLE grants_new RENAME TO grants;
CREATE INDEX grants_principal ON grants (principal_type, principal_id);

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
