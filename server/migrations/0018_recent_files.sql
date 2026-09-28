-- Files each person opened (content or preview, in the browser), for Recent. One row per person and file with the
-- latest time; updated at most once a minute, and only the most recent few hundred per person are kept (nodes.rs).
CREATE TABLE recent_files (
  user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  node_id TEXT NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
  at      INTEGER NOT NULL,
  PRIMARY KEY (user_id, node_id)
);
-- Deleting a node looks up its rows here
CREATE INDEX recent_files_node ON recent_files (node_id);

-- Recent also lists the files a person edited, from their newest activity entries (dropped in 0008 while unused)
CREATE INDEX activity_user ON activity (user_id);
