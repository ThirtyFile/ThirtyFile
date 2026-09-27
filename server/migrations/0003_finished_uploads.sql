-- Uploads are kept after they finish, with the file they created, until they expire.
-- When the response to the last request is lost (a proxy timeout, a dropped connection), the client asks again: it
-- now learns that the upload finished and which file it became, instead of starting the whole upload over.

ALTER TABLE uploads ADD COLUMN node_id TEXT;
