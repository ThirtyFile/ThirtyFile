-- Folder spaces: when each item was created, where the file system tells (nanoseconds since 1970; NULL where it isn't
-- known). An item found at a new path with the device and inode number of one whose path is gone is taken for that
-- item only when it was created at the same time, since file systems give the inode numbers of deleted items out
-- again (folders.rs).
ALTER TABLE nodes ADD COLUMN fs_birth_ns INTEGER;
