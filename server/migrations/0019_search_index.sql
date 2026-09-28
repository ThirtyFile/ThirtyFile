-- Search by name with a trigram index: finds a term anywhere in a name without reading every node, ignoring letter
-- case and accents in every language (LIKE only folded A–Z). Terms shorter than three characters, which the index
-- can't look up, still use a scan.
CREATE VIRTUAL TABLE nodes_fts USING fts5(name, content='nodes', content_rowid='rowid', tokenize='trigram remove_diacritics 1');
INSERT INTO nodes_fts (nodes_fts) VALUES ('rebuild');

CREATE TRIGGER nodes_fts_insert AFTER INSERT ON nodes BEGIN
  INSERT INTO nodes_fts (rowid, name) VALUES (new.rowid, new.name);
END;
CREATE TRIGGER nodes_fts_delete AFTER DELETE ON nodes BEGIN
  INSERT INTO nodes_fts (nodes_fts, rowid, name) VALUES ('delete', old.rowid, old.name);
END;
CREATE TRIGGER nodes_fts_rename AFTER UPDATE OF name ON nodes BEGIN
  INSERT INTO nodes_fts (nodes_fts, rowid, name) VALUES ('delete', old.rowid, old.name);
  INSERT INTO nodes_fts (rowid, name) VALUES (new.rowid, new.name);
END;
