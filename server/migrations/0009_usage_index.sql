-- Space totals (at startup, daily and after moves) sum the files of each space: with the size in the index they are
-- read from the index alone, instead of from every row of the nodes table
DROP INDEX nodes_drive;
CREATE INDEX nodes_drive ON nodes (drive_id, kind, size);
