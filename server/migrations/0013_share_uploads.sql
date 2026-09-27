-- Share links for receiving files, and links that allow viewing only.
-- A folder link can accept uploads from its visitors (allow_upload), optionally without showing them what is in the
-- folder (drop_only: they can only upload). allow_download = 0 serves files for previews only: no download or ZIP.

ALTER TABLE shares ADD COLUMN allow_upload INTEGER NOT NULL DEFAULT 0;
ALTER TABLE shares ADD COLUMN drop_only INTEGER NOT NULL DEFAULT 0;
ALTER TABLE shares ADD COLUMN allow_download INTEGER NOT NULL DEFAULT 1;

-- Uploads made through a share link: the link (NULL for uploads by signed-in people). They belong to the link's
-- creator (owner_id), and only requests through the same link can continue them.
ALTER TABLE uploads ADD COLUMN share_id TEXT;
CREATE INDEX uploads_share ON uploads (share_id) WHERE share_id IS NOT NULL;
