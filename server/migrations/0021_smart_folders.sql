-- Smart folders: searches a person saved, which show as folders and list what matches when opened (nodes/smart.rs).
-- Each is its owner's alone. It holds no items, only the query, so deleting one deletes nothing else; which items it
-- lists, and whether the person may still see them, is worked out each time it is listed.
CREATE TABLE smart_folders (
  id         INTEGER PRIMARY KEY,
  owner_id   INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  name       TEXT NOT NULL,
  -- A person's smart folder names are unique without counting letter case, in every language (unicode_lower(), db.rs)
  name_key   TEXT GENERATED ALWAYS AS (unicode_lower(name)) VIRTUAL,
  -- What it looks for, as JSON: the name, kind, size and modified ranges, where to look and the owner's tags
  query      TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX smart_folders_name ON smart_folders (owner_id, name_key);
