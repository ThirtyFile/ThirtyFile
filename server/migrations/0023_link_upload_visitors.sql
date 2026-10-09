-- An upload through a share link belongs to the browser that started it: a key it was given for that link (a cookie),
-- kept here as its SHA-256. Another visitor of the same link can't continue, look at or cancel it. NULL for people's
-- own uploads, which belong to their account.
ALTER TABLE uploads ADD COLUMN visitor TEXT;
