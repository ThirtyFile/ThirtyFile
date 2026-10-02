-- Every trigger of space_changes (0008) gives the space the next number, MAX(seq) + 1: without an index on seq that read
-- the whole table, once for every row a change wrote. With 10,000 spaces a change of 2,000 items took 0.8 s, all of it
-- holding the write lock (9 ms with the index).
CREATE INDEX space_changes_seq ON space_changes (seq);

-- An item counts as changed when what a snapshot or a replica keeps of it changes, not when only bookkeeping is
-- written: the time it was created on disk (written for every item of a folder space by its first check after the
-- upgrade from 0.4.0), its disk identity, who deleted it or gave it its content, or whether a check found it. A write
-- that leaves the columns as they were doesn't count either.
DROP TRIGGER space_changes_node_update;
CREATE TRIGGER space_changes_node_update
AFTER UPDATE OF owner_id, parent_id, kind, name, blob_hash, size, mime, created_at, updated_at, trashed_at, trash_root, drive_id, fs_path, fs_size,
                fs_mtime_ns ON nodes
WHEN COALESCE(NEW.drive_id, OLD.drive_id) IS NOT NULL
  AND (NEW.owner_id IS NOT OLD.owner_id OR NEW.parent_id IS NOT OLD.parent_id OR NEW.kind IS NOT OLD.kind OR NEW.name IS NOT OLD.name
    OR NEW.blob_hash IS NOT OLD.blob_hash OR NEW.size IS NOT OLD.size OR NEW.mime IS NOT OLD.mime OR NEW.created_at IS NOT OLD.created_at
    OR NEW.updated_at IS NOT OLD.updated_at OR NEW.trashed_at IS NOT OLD.trashed_at OR NEW.trash_root IS NOT OLD.trash_root
    OR NEW.drive_id IS NOT OLD.drive_id OR NEW.fs_path IS NOT OLD.fs_path OR NEW.fs_size IS NOT OLD.fs_size OR NEW.fs_mtime_ns IS NOT OLD.fs_mtime_ns)
BEGIN
  INSERT INTO space_changes (drive_id, seq, changed_at)
  SELECT d, (SELECT COALESCE(MAX(seq), 0) + 1 FROM space_changes), unixepoch() FROM (SELECT NEW.drive_id AS d UNION SELECT OLD.drive_id) WHERE d IS NOT NULL
  ON CONFLICT (drive_id) DO UPDATE SET seq = excluded.seq, changed_at = excluded.changed_at;
END;

-- A folder listed in the default order (folders first, then by name, list.rs) is read from this index a page at a
-- time, instead of sorting every item of the folder for every page. It is in the order of natural_cmp (util.rs): when
-- that changes, the server rebuilds it (db.rs, NATURAL_ORDER).
CREATE INDEX nodes_listed ON nodes (parent_id, (kind = 'folder') DESC, name COLLATE natural_name, id) WHERE trashed_at IS NULL;

-- Nothing looks the error log up by person: the filter is on the name, which stays after the account is gone
DROP INDEX error_log_user;
