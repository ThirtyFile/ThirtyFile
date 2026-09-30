-- A space counts as changed (space_changes, 0008) when its items, versions, access or settings change, not when only
-- its usage counter, its move state or the report of its last check for changes is written: a check for changes runs
-- every few minutes, and usage is counted again every day, which would otherwise make backups and replicas copy spaces
-- that didn't change.
DROP TRIGGER space_changes_drive_update;
CREATE TRIGGER space_changes_drive_update
AFTER UPDATE OF name, kind, root_id, owner_id, quota_bytes, disabled, location_id, mode, source_path, read_only ON drives
WHEN NEW.name IS NOT OLD.name OR NEW.kind IS NOT OLD.kind OR NEW.root_id IS NOT OLD.root_id OR NEW.owner_id IS NOT OLD.owner_id
  OR NEW.quota_bytes IS NOT OLD.quota_bytes OR NEW.disabled IS NOT OLD.disabled OR NEW.location_id IS NOT OLD.location_id
  OR NEW.mode IS NOT OLD.mode OR NEW.source_path IS NOT OLD.source_path OR NEW.read_only IS NOT OLD.read_only
BEGIN
  INSERT INTO space_changes (drive_id, seq, changed_at) VALUES (NEW.id, (SELECT COALESCE(MAX(seq), 0) + 1 FROM space_changes), unixepoch())
  ON CONFLICT (drive_id) DO UPDATE SET seq = excluded.seq, changed_at = excluded.changed_at;
END;
