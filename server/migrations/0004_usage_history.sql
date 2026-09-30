-- Control panel › Storage usage (usage/): how much each storage location holds over time, and how its storage
-- operations went. Each sample is written into three tiers at once: a fine one (every 15 minutes for capacity, 5
-- minutes for operations), hours and days (UTC). Old rows of each tier are deleted (usage/sample.rs, RETENTION), so
-- the tables stay small whatever happens on the server.

-- How much a storage location holds. `location_id` is '' for everything together: the logical totals of all spaces
-- and the disk of the data folder. Within an hour or a day, the last sample taken stands for it.
CREATE TABLE usage_capacity (
  location_id          TEXT NOT NULL,
  -- Seconds the row covers: 900, 3600 or 86400
  span                 INTEGER NOT NULL,
  -- Start of the period (Unix seconds, a multiple of span)
  at                   INTEGER NOT NULL,
  -- When the sample was taken
  sampled_at           INTEGER NOT NULL,
  -- Files of the spaces on the location as people see them (not in the trash), the trash, and earlier versions:
  -- identical content counts every time here
  live_bytes           INTEGER NOT NULL,
  trash_bytes          INTEGER NOT NULL,
  version_bytes        INTEGER NOT NULL,
  -- Physical content ThirtyFile keeps there: the content store (identical content once), the files of folder spaces
  -- (with their trash) and the earlier versions kept in their folders
  store_bytes          INTEGER NOT NULL,
  folder_bytes         INTEGER NOT NULL,
  folder_version_bytes INTEGER NOT NULL,
  -- Contents waiting to be deleted from the location (their size isn't known any more)
  pending_deletes      INTEGER NOT NULL,
  -- Uploads in progress, kept in the data folder ('' only)
  temp_bytes           INTEGER NOT NULL,
  -- Reserved for backups and replicas; NULL until ThirtyFile makes them
  backup_bytes         INTEGER,
  replica_bytes        INTEGER,
  -- The disk as the system reports it (all of it, whatever uses it); NULL when it can't be measured (S3, SFTP, FTP,
  -- or no answer in time)
  disk_free            INTEGER,
  disk_total           INTEGER,
  -- Locations with the same value are on the same disk, which is counted once
  disk_id              TEXT,
  -- Whether the location could be used when the sample was taken
  online               INTEGER NOT NULL,
  -- How long taking the sample took (milliseconds)
  took_ms              INTEGER NOT NULL,
  PRIMARY KEY (location_id, span, at)
) WITHOUT ROWID;

-- Operations on a storage location: calls to its storage backend (content store) and the files of folder spaces read
-- and stored. `op`: read, write, delete, list or check; `work`: foreground (people using files), background (moves,
-- compressing and extracting, clean-ups, scans) or probe (health checks and tests). Only periods with operations have
-- a row. `location_id` '' holds folder spaces that are on no storage location.
CREATE TABLE usage_ops (
  location_id TEXT NOT NULL,
  span        INTEGER NOT NULL,
  at          INTEGER NOT NULL,
  op          TEXT NOT NULL,
  work        TEXT NOT NULL,
  count       INTEGER NOT NULL,
  -- Of `count`: failed, and timed out or abandoned
  errors      INTEGER NOT NULL,
  timeouts    INTEGER NOT NULL,
  bytes       INTEGER NOT NULL,
  -- Durations (microseconds): their sum and the longest, and a histogram for percentiles (usage/meter.rs, `Hist`)
  total_us    INTEGER NOT NULL,
  max_us      INTEGER NOT NULL,
  hist        TEXT NOT NULL,
  PRIMARY KEY (location_id, span, at, op, work)
) WITHOUT ROWID;
-- Deleting old rows goes by tier and time
CREATE INDEX usage_ops_span_at ON usage_ops (span, at);
CREATE INDEX usage_capacity_span_at ON usage_capacity (span, at);
