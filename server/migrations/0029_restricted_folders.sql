-- A restricted folder stops grants inherited from folders above it.
-- Existing trees keep their access until an owner explicitly changes a folder.
ALTER TABLE nodes ADD COLUMN restricted INTEGER NOT NULL DEFAULT 0 CHECK(restricted IN (0, 1));
CREATE INDEX nodes_restricted ON nodes(drive_id, id) WHERE restricted = 1;
-- Keep access boundaries on saved names after a node is purged. These references deliberately have no foreign key.
ALTER TABLE activity ADD COLUMN restricted_scopes TEXT NOT NULL DEFAULT '[]';
ALTER TABLE share_access ADD COLUMN restricted_scopes TEXT NOT NULL DEFAULT '[]';
ALTER TABLE notifications ADD COLUMN restricted_scopes TEXT NOT NULL DEFAULT '[]';
