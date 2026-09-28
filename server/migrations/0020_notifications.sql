-- Notifications (#64): shown under the bell in the app and, when an administrator set up an email server, sent by email.

-- Where email notifications go. Accounts made by an administrator have none until the person enters one; signing in
-- with single sign-on fills it in from the provider's verified email when it is still blank
ALTER TABLE users ADD COLUMN email TEXT NOT NULL DEFAULT '';
-- The interface language and time zone the person last used (for the text and times of emails): '' = the system
-- default language; the offset in minutes as the browser gives it (UTC − local time, so UTC+8 is -480)
ALTER TABLE users ADD COLUMN lang TEXT NOT NULL DEFAULT '';
ALTER TABLE users ADD COLUMN tz_offset INTEGER NOT NULL DEFAULT 0;

CREATE TABLE notifications (
  id         INTEGER PRIMARY KEY,
  user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  -- shared, space_full or access_expiring
  kind       TEXT NOT NULL,
  -- JSON with what the message shows; names are copied so it stays readable after they change
  data       TEXT NOT NULL DEFAULT '{}',
  -- What opening the notification shows (a folder, a file or a space's root); it may have been deleted since
  node_id    TEXT,
  created_at INTEGER NOT NULL,
  read_at    INTEGER
);
CREATE INDEX notifications_user ON notifications (user_id, id);
CREATE INDEX notifications_unread ON notifications (user_id) WHERE read_at IS NULL;
CREATE INDEX notifications_created ON notifications (created_at);

-- Per person and kind: whether it shows in the app and whether it is sent by email. No row = both on
CREATE TABLE notification_prefs (
  user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  kind    TEXT NOT NULL,
  in_app  INTEGER NOT NULL DEFAULT 1,
  email   INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (user_id, kind)
);

-- What the hourly check already told people, so it doesn't say it again: "space_full:<space>" until the space has room
-- again, "access_expiring:<grant>:<expires_at>" until a while after the access ended
CREATE TABLE notification_marks (
  key TEXT PRIMARY KEY,
  at  INTEGER NOT NULL
);
