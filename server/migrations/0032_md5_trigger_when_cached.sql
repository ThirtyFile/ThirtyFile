-- The S3 validators (0028) are cleared only when something is cached. A folder's updated_at changes with every change
-- inside it (tree::touch), and the trigger ran a second UPDATE each time, on folders that never have validators.
DROP TRIGGER nodes_md5_invalidated;
CREATE TRIGGER nodes_md5_invalidated AFTER UPDATE OF blob_hash, size, updated_at ON nodes
WHEN (NEW.blob_hash IS NOT OLD.blob_hash OR NEW.size != OLD.size OR NEW.updated_at != OLD.updated_at)
  AND (OLD.content_md5 IS NOT NULL OR OLD.md5_version IS NOT NULL OR OLD.s3_mtime IS NOT NULL OR OLD.s3_mtime_version IS NOT NULL)
BEGIN
    UPDATE nodes SET content_md5 = NULL, md5_version = NULL, s3_mtime = NULL, s3_mtime_version = NULL WHERE id = NEW.id;
END;
