-- Recent lists a person's own files newest first: without this index SQLite sorts all of them on every request, which
-- for the owner of a large space (scanned files belong to the space root's owner) is every file in it.
CREATE INDEX nodes_owner_recent ON nodes (owner_id, kind, updated_at);
