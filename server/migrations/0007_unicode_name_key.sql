-- Names in the content store are unique regardless of letter case in every language, not only A–Z: "Été" and "été"
-- are the same name, as Rust's to_lowercase() (used to compare names in memory) and Windows see them.
-- unicode_lower() is registered by the server on every connection (db.rs).

-- Items that were allowed side by side before (only non-English letter case differs) keep their content: every one
-- but the oldest gets its id added to the name, "Été.txt" → "Été (1a2b3c4d).txt".
UPDATE nodes SET name =
    CASE WHEN kind = 'file' AND length(rtrim(name, replace(name, '.', ''))) > 1
         THEN substr(name, 1, length(rtrim(name, replace(name, '.', ''))) - 1) || ' (' || substr(id, 1, 8) || ')'
              || substr(name, length(rtrim(name, replace(name, '.', ''))))
         ELSE name || ' (' || substr(id, 1, 8) || ')' END
WHERE fs_path IS NULL AND trashed_at IS NULL AND parent_id IS NOT NULL AND EXISTS (
    SELECT 1 FROM nodes o
    WHERE o.parent_id = nodes.parent_id AND o.fs_path IS NULL AND o.trashed_at IS NULL AND o.id <> nodes.id
      AND unicode_lower(o.name) = unicode_lower(nodes.name)
      AND (o.created_at < nodes.created_at OR (o.created_at = nodes.created_at AND o.id < nodes.id))
);

DROP INDEX nodes_name_uq;
ALTER TABLE nodes DROP COLUMN name_key;
ALTER TABLE nodes ADD COLUMN name_key TEXT GENERATED ALWAYS AS (CASE WHEN fs_path IS NULL THEN unicode_lower(name) ELSE name END) VIRTUAL;
CREATE UNIQUE INDEX nodes_name_uq ON nodes (parent_id, name_key) WHERE trashed_at IS NULL;
