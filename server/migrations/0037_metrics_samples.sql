-- Control panel › Dashboard (metrics/dashboard.rs): a figure for each five-minute period of a fixed list of series
-- (requests, errors, how long answers took, sign-ins, storage operations, tasks, free disk), kept eight days
CREATE TABLE metrics_samples (
  -- The series, and the start of the period (Unix seconds)
  series TEXT NOT NULL,
  at     INTEGER NOT NULL,
  value  REAL NOT NULL,
  PRIMARY KEY (series, at)
) WITHOUT ROWID;
