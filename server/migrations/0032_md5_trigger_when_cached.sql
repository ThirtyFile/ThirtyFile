-- The S3 validators (0028) are cleared only when the content changes (its blob or size) and something is cached. Every
-- read checks a cached value against the content's version anyway (md5_version, s3_mtime_version), so a new updated_at
-- alone (a rename, or tree::touch on a folder with every change inside it) no longer runs a second UPDATE or throws
-- away a digest worked out by reading the whole file.
DROP TRIGGER nodes_md5_invalidated;
CREATE TRIGGER nodes_md5_invalidated AFTER UPDATE OF blob_hash, size ON nodes
WHEN (NEW.blob_hash IS NOT OLD.blob_hash OR NEW.size != OLD.size)
  AND (OLD.content_md5 IS NOT NULL OR OLD.md5_version IS NOT NULL OR OLD.s3_mtime IS NOT NULL OR OLD.s3_mtime_version IS NOT NULL)
BEGIN
    UPDATE nodes SET content_md5 = NULL, md5_version = NULL, s3_mtime = NULL, s3_mtime_version = NULL WHERE id = NEW.id;
END;
