-- The highest id ever given to a user or group. SQLite gives a new row the highest existing id plus one, so after the
-- newest user or group was deleted, the next one got its id, and with it the log entries and sign-in settings that
-- still pointed at the deleted one. New ids are now taken from here and never repeat.

CREATE TABLE id_counters (
  name TEXT PRIMARY KEY,
  last INTEGER NOT NULL
);
INSERT INTO id_counters (name, last) SELECT 'users', COALESCE(MAX(id), 0) FROM users;
INSERT INTO id_counters (name, last) SELECT 'groups', COALESCE(MAX(id), 0) FROM groups;
