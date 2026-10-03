-- Coloured tags people put on files and folders (tags.rs). Each person has their own: only they see their tags, their
-- names and what they are on, also on items in spaces they share with others. Administrators don't see them either.
CREATE TABLE tags (
  id         INTEGER PRIMARY KEY,
  owner_id   INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  name       TEXT NOT NULL,
  -- From a fixed palette, which every interface style and theme draws in its own shade
  color      TEXT NOT NULL CHECK (color IN ('red', 'orange', 'yellow', 'green', 'blue', 'purple', 'gray')),
  -- A person's tag names are unique without counting letter case, in every language (unicode_lower(), db.rs)
  name_key   TEXT GENERATED ALWAYS AS (unicode_lower(name)) VIRTUAL,
  created_at INTEGER NOT NULL,
  -- For the assignments' reference, which takes the owner along
  UNIQUE (id, owner_id)
);
CREATE UNIQUE INDEX tags_name ON tags (owner_id, name_key);

-- The items each person tagged. owner_id is the tag's owner, repeated (and kept the same by the reference) so that every
-- read of assignments can say whose it wants without joining the tags. An assignment is on the item's node, so it stays with the item wherever it
-- moves, also when a scan of a folder space recognises an item renamed or moved outside ThirtyFile; it stays while the
-- item is in the trash, and goes when the item is deleted for good. Copies made in ThirtyFile get the tags of the
-- person who copies (nodes/organize.rs, fsops/across.rs).
CREATE TABLE tagged (
  node_id    TEXT NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
  tag_id     INTEGER NOT NULL,
  owner_id   INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (node_id, tag_id),
  FOREIGN KEY (tag_id, owner_id) REFERENCES tags (id, owner_id) ON DELETE CASCADE
);
-- The items with a tag, and removing a tag's assignments with it
CREATE INDEX tagged_tag ON tagged (tag_id, owner_id);
