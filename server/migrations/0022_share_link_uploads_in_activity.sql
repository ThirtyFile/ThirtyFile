-- Files received through a share link were logged with the link's address. The activity log now says only that a link
-- was used: which one is in the link's own access log (share_access).
UPDATE activity SET detail = 'Through a share link' WHERE action = 'upload' AND detail LIKE 'Through share link %';
