-- Errors people ran into, for Control panel > Activity > Errors (logs/errors.rs): failed requests the server answered
-- and errors the web page reported. One row per incident: the same error again soon after is counted on the row
-- (count, at) instead of adding another. Old rows are archived or deleted with the activity log's retention.
CREATE TABLE error_log (
  id          INTEGER PRIMARY KEY,
  -- Last time it happened; first_at the first
  at          INTEGER NOT NULL,
  first_at    INTEGER NOT NULL,
  count       INTEGER NOT NULL DEFAULT 1,
  -- 'backend' (the server answered with an error) or 'frontend' (the page reported it)
  source      TEXT NOT NULL,
  -- 'error' for unexpected failures, 'warning' for expected refusals (validation, permission, conflict)
  severity    TEXT NOT NULL,
  -- What kind of failure: server, storage, timeout, validation, permission, not_found, conflict, limit, render,
  -- uncaught, rejection, handled
  kind        TEXT NOT NULL DEFAULT '',
  -- The signed-in person, when known (the name is copied so the log stays readable after the account is removed)
  user_id     INTEGER,
  username    TEXT NOT NULL DEFAULT '',
  -- What was being done ("POST /api/nodes/{id}/rename", "upload", "preview") and where (a route, never a file name)
  operation   TEXT NOT NULL DEFAULT '',
  route       TEXT NOT NULL DEFAULT '',
  -- An id of the item concerned, when there is one
  resource    TEXT NOT NULL DEFAULT '',
  status      INTEGER,
  code        TEXT NOT NULL DEFAULT '',
  message     TEXT NOT NULL DEFAULT '',
  -- Diagnostic details: the kind of server error, or the page's stack trace
  detail      TEXT NOT NULL DEFAULT '',
  -- Ties a failed request to the page's report of it (X-Request-Id); the latest occurrence's
  request_id  TEXT,
  -- What the page reported about a failed request the server recorded too
  client      TEXT NOT NULL DEFAULT '',
  version     TEXT NOT NULL DEFAULT '',
  -- Same source, kind, person, operation, status and message: the same error
  fingerprint TEXT NOT NULL
);
CREATE INDEX error_log_at ON error_log (at);
CREATE INDEX error_log_fingerprint ON error_log (fingerprint);
CREATE INDEX error_log_request ON error_log (request_id);
CREATE INDEX error_log_user ON error_log (user_id);
