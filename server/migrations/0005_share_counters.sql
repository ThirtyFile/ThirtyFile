-- Views and the last access of a share link, kept on the share: they used to be counted from the access log, which
-- is archived and trimmed after its retention period, so the numbers went down over time.

ALTER TABLE shares ADD COLUMN views INTEGER NOT NULL DEFAULT 0;
ALTER TABLE shares ADD COLUMN last_access INTEGER;
UPDATE shares SET
  views = (SELECT COUNT(*) FROM share_access a WHERE a.share_id = shares.id AND a.event = 'view'),
  last_access = (SELECT MAX(at) FROM share_access a WHERE a.share_id = shares.id);
