-- Items the check of a folder space found on its disk, rather than ones someone added through ThirtyFile (folders.rs):
-- they have no uploader and are nobody's recent files. owner_id stays the space owner's, which usage and the clean-up
-- of a deleted user go by. Items found before this can't be told from uploaded ones, and keep the owner as uploader.
ALTER TABLE nodes ADD COLUMN found INTEGER NOT NULL DEFAULT 0;

-- Recent lists a person's own uploads newest first, without walking through the found files of a large space they own
CREATE INDEX nodes_uploads_recent ON nodes (owner_id, kind, updated_at) WHERE found = 0;
