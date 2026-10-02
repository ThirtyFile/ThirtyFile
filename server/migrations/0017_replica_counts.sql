-- What Control panel › Replicas shows, kept instead of worked out each time the page is looked at (that read every
-- content of the replicated spaces). Worked out by a sync that changed something, and by the scheduler soon after a
-- policy changes, a promotion, or a space leaves a policy (replicas/policy.rs).

-- How many contents a target holds of those it should; NULL: not worked out yet
ALTER TABLE replica_targets ADD COLUMN held INTEGER;
ALTER TABLE replica_targets ADD COLUMN wanted INTEGER;

-- Copies kept on a location that no replica policy wants any more (removed on request), and their bytes. A location
-- with copies and no row here isn't worked out yet.
CREATE TABLE replica_unneeded (
  location_id TEXT PRIMARY KEY,
  copies      INTEGER NOT NULL,
  bytes       INTEGER NOT NULL
) WITHOUT ROWID;
