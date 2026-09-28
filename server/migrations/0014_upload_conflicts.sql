-- What an upload does when its folder already has an item with the same name: '' or 'keep' = keep both (the new file
-- gets a number), 'replace' = the existing file gets the new content and keeps its id, shares and permissions.
-- The browser asks before uploading; the answer travels with the upload so it still applies when it resumes.

ALTER TABLE uploads ADD COLUMN on_conflict TEXT NOT NULL DEFAULT '';
