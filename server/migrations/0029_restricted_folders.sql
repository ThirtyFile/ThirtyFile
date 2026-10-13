-- A restricted folder stops grants inherited from folders above it.
-- Existing trees keep their access until an owner explicitly changes a folder.
ALTER TABLE nodes ADD COLUMN restricted INTEGER NOT NULL DEFAULT 0 CHECK(restricted IN (0, 1));
CREATE INDEX nodes_restricted ON nodes(drive_id, id) WHERE restricted = 1;
