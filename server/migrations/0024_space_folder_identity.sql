-- The file system identity (device and inode) of a folder space's folder, as it was when the folder was chosen or last
-- found holding the space's marker. A folder without the marker (a read-only one, or one whose marker was deleted) is
-- taken for the space's only when it is that folder: another one put in its place, or a link to one, is never indexed.
-- NULL until known (spaces made by earlier versions, and systems without inodes).
ALTER TABLE drives ADD COLUMN source_dev INTEGER;
ALTER TABLE drives ADD COLUMN source_ino INTEGER;
