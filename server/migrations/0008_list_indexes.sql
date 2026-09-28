-- Log pages filter by one column and page newest first by id (ORDER BY id DESC). An index on the column alone ends
-- in the row id, so SQLite reads matching rows already in that order, instead of sorting every match by `at` first.
DROP INDEX activity_drive;
CREATE INDEX activity_drive ON activity (drive_id);
DROP INDEX activity_action;
CREATE INDEX activity_action ON activity (action);
DROP INDEX share_access_share;
CREATE INDEX share_access_share ON share_access (share_id);
DROP INDEX share_access_owner;
CREATE INDEX share_access_owner ON share_access (owner_id);
DROP INDEX login_log_user;
CREATE INDEX login_log_user ON login_log (user_id);

-- Never used by a query; they only slowed down writes
DROP INDEX activity_user;
DROP INDEX nodes_trash;

-- The storage list sums the content of each location: a covering index answers it without reading the blobs table
DROP INDEX blobs_location;
CREATE INDEX blobs_location_size ON blobs (location_id, size);
