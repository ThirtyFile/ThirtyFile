-- Storage analytics (analytics/): what takes the space of a space, as one person is allowed to see it, kept as the
-- last snapshot so the page opens at once and says when it was made
CREATE TABLE analytics_snapshots (
  drive_id TEXT NOT NULL,
  user_id  INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  taken_at INTEGER NOT NULL,
  -- JSON: the figures and the lists (analytics/compute.rs)
  data     TEXT NOT NULL,
  PRIMARY KEY (drive_id, user_id)
) WITHOUT ROWID;
