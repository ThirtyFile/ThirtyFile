-- `thirtyfile convert` (convert.rs) turns spaces of the content store on this server's disks into folder spaces. While
-- a space is being converted, its future folder is in drives.source_path (the space still has mode 'store'), and every
-- file written into that folder is recorded here, so a conversion that stopped halfway skips it when it runs again.
-- The rows of a space go when it becomes a folder space.
CREATE TABLE convert_files (
  drive_id    TEXT NOT NULL,
  -- Path below the space's folder
  fs_path     TEXT NOT NULL,
  -- The content written there ('' for a file without content)
  hash        TEXT NOT NULL,
  -- The file as it was written: when it differs now, it is written again
  fs_dev      INTEGER NOT NULL,
  fs_ino      INTEGER NOT NULL,
  fs_size     INTEGER NOT NULL,
  fs_mtime_ns INTEGER NOT NULL,
  PRIMARY KEY (drive_id, fs_path)
);
