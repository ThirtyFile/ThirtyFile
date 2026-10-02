-- Snapshots, replica syncs and moves read the files of a space a page at a time, in id order (backups/capture.rs,
-- replicas/folders.rs, moves/to_store.rs): with this index each page starts where the last one ended, instead of
-- reading or sorting every file of the space again.
CREATE INDEX nodes_drive_kind_id ON nodes (drive_id, kind, id);
-- The copies a replica sync checks again before they count (an old primary's after a promotion, and damaged ones):
-- found without reading every copy on the location.
CREATE INDEX replica_copies_recheck ON replica_copies (location_id, hash) WHERE state IN ('stale', 'corrupt');
-- Whether the spaces of a replica policy use a content is looked up by the content, for each one a sync walks: with
-- the space in the index, the files using it are found without reading their rows.
DROP INDEX nodes_blob;
CREATE INDEX nodes_blob ON nodes (blob_hash, drive_id);
