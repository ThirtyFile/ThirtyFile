-- File event webhooks (file_hooks/): where news of changes to files goes, and the messages waiting to be sent there
CREATE TABLE file_webhooks (
  id            TEXT PRIMARY KEY,
  url           TEXT NOT NULL,
  -- What messages are signed with, sealed (secrets.rs)
  secret        TEXT NOT NULL,
  -- JSON: the events sent
  events        TEXT NOT NULL,
  -- JSON: the spaces whose events are sent; empty: every space but personal ones
  drives        TEXT NOT NULL DEFAULT '[]',
  allow_private INTEGER NOT NULL DEFAULT 0,
  enabled       INTEGER NOT NULL DEFAULT 1,
  created_at    INTEGER NOT NULL
);

-- Queued in the transaction of the change it tells of, so a restart loses nothing and a change that fails sends nothing
CREATE TABLE file_webhook_deliveries (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  webhook_id      TEXT NOT NULL REFERENCES file_webhooks (id) ON DELETE CASCADE,
  event           TEXT NOT NULL,
  -- JSON: the message
  body            TEXT NOT NULL,
  created_at      INTEGER NOT NULL,
  next_attempt_at INTEGER NOT NULL,
  attempts        INTEGER NOT NULL DEFAULT 0,
  done_at         INTEGER,
  failed          INTEGER NOT NULL DEFAULT 0,
  error           TEXT
);
-- What waits to be sent, oldest first
CREATE INDEX file_webhook_pending ON file_webhook_deliveries (webhook_id, id) WHERE done_at IS NULL AND failed = 0;
-- The latest messages of a webhook
CREATE INDEX file_webhook_recent ON file_webhook_deliveries (webhook_id, id);
-- Old messages, to remove
CREATE INDEX file_webhook_age ON file_webhook_deliveries (created_at);
