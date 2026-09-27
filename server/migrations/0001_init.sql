-- ThirtyFile database schema.
-- Timestamps are Unix seconds; booleans are 0/1 integers.

-- Key/value system settings (JSON values for structured settings).
CREATE TABLE settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

-- ───────────── Accounts ─────────────

CREATE TABLE users (
  id            INTEGER PRIMARY KEY,
  username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
  -- Shown next to the username; filled in from the provider's profile on single sign-on
  display_name  TEXT NOT NULL DEFAULT '',
  password_hash TEXT NOT NULL,
  role          TEXT NOT NULL DEFAULT 'user' CHECK (role IN ('admin', 'user')),
  can_write     INTEGER NOT NULL DEFAULT 1,
  can_delete    INTEGER NOT NULL DEFAULT 1,
  can_share     INTEGER NOT NULL DEFAULT 1,
  -- Personal drive quota in bytes (0 = unlimited)
  quota_bytes   INTEGER NOT NULL DEFAULT 0,
  -- Root folder of the user's personal drive
  root_id       TEXT NOT NULL DEFAULT '',
  disabled      INTEGER NOT NULL DEFAULT 0,
  created_at    INTEGER NOT NULL,
  last_login_at INTEGER,
  -- Where the account came from: 'password' (created by an administrator) or the sign-in provider that created it automatically
  source        TEXT NOT NULL DEFAULT 'password',
  -- For automatically created accounts: the provider's stable identifier of the person (audit trail; the link itself lives in user_identities)
  provisioned_by TEXT
);
CREATE INDEX users_source_created ON users (source, created_at);

-- Login sessions; only a hash of the cookie token is stored.
CREATE TABLE sessions (
  token_hash TEXT PRIMARY KEY,
  user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL
);
CREATE INDEX sessions_user ON sessions (user_id);
CREATE INDEX sessions_expires ON sessions (expires_at);

-- External sign-in identities (Microsoft Entra ID, Google, GitHub) linked to users.
CREATE TABLE user_identities (
  provider      TEXT NOT NULL,
  -- Stable subject identifier issued by the provider
  subject       TEXT NOT NULL,
  user_id       INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  email         TEXT NOT NULL DEFAULT '',
  name          TEXT NOT NULL DEFAULT '',
  created_at    INTEGER NOT NULL,
  last_login_at INTEGER,
  PRIMARY KEY (provider, subject)
);
CREATE INDEX user_identities_user ON user_identities (user_id);

CREATE TABLE groups (
  id          INTEGER PRIMARY KEY,
  name        TEXT NOT NULL UNIQUE COLLATE NOCASE,
  description TEXT NOT NULL DEFAULT '',
  created_at  INTEGER NOT NULL
);

CREATE TABLE group_members (
  group_id INTEGER NOT NULL REFERENCES groups (id) ON DELETE CASCADE,
  user_id  INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  PRIMARY KEY (group_id, user_id)
);
CREATE INDEX group_members_user ON group_members (user_id);

-- ───────────── Storage ─────────────

-- Where file contents are stored. The built-in `local` location is the data directory's blobs/.
CREATE TABLE storage_locations (
  id         TEXT PRIMARY KEY,
  name       TEXT NOT NULL,
  kind       TEXT NOT NULL CHECK (kind IN ('local', 's3', 'sftp', 'ftp')),
  -- JSON; secrets stay on the server and are never returned by the API
  config     TEXT NOT NULL DEFAULT '{}',
  is_default INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);
INSERT INTO storage_locations (id, name, kind, config, is_default, created_at)
VALUES ('local', 'Local disk', 'local', '{}', 1, CAST(strftime('%s', 'now') AS INTEGER));

-- File contents, addressed by SHA-256 and deduplicated across the whole system.
CREATE TABLE blobs (
  hash        TEXT PRIMARY KEY,
  size        INTEGER NOT NULL,
  -- Number of nodes referencing this content; deleted from storage when it drops to 0
  refcount    INTEGER NOT NULL DEFAULT 0,
  created_at  INTEGER NOT NULL,
  location_id TEXT NOT NULL DEFAULT 'local'
);
CREATE INDEX blobs_location ON blobs (location_id);

-- Contents still to be deleted from a storage location: deletions that failed (e.g. while it was offline) and old
-- copies after a move, which wait until created_at (a time in the future) so downloads in progress can finish.
CREATE TABLE pending_blob_deletes (
  hash        TEXT NOT NULL,
  location_id TEXT NOT NULL,
  created_at  INTEGER NOT NULL,
  attempts    INTEGER NOT NULL DEFAULT 0,
  last_error  TEXT NOT NULL DEFAULT '',
  PRIMARY KEY (hash, location_id)
);
CREATE INDEX pending_blob_deletes_location ON pending_blob_deletes (location_id, created_at);

-- ───────────── Files ─────────────

-- Drives: every user's personal drive, the company-wide drive and team drives.
CREATE TABLE drives (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  kind        TEXT NOT NULL CHECK (kind IN ('personal', 'company', 'team')),
  root_id     TEXT NOT NULL UNIQUE REFERENCES nodes (id),
  owner_id    INTEGER REFERENCES users (id),
  -- 0 = unlimited
  quota_bytes INTEGER NOT NULL DEFAULT 0,
  -- Bytes of all file nodes in the drive (including the trash), maintained in the same transaction as every change,
  -- so quota checks and drive lists don't sum the node table
  used_bytes  INTEGER NOT NULL DEFAULT 0,
  disabled    INTEGER NOT NULL DEFAULT 0,
  created_by  INTEGER,
  created_at  INTEGER NOT NULL,
  -- NULL = the default storage location
  location_id TEXT
);
CREATE INDEX drives_owner ON drives (owner_id, kind);

-- The virtual file tree (folders and files).
CREATE TABLE nodes (
  id         TEXT PRIMARY KEY,
  owner_id   INTEGER NOT NULL REFERENCES users (id),
  parent_id  TEXT REFERENCES nodes (id),
  kind       TEXT NOT NULL CHECK (kind IN ('folder', 'file')),
  name       TEXT NOT NULL,
  blob_hash  TEXT REFERENCES blobs (hash),
  size       INTEGER NOT NULL DEFAULT 0,
  mime       TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  -- Trash: every node of a deleted subtree shares trash_id; trash_root marks the item the user deleted
  trashed_at INTEGER,
  trash_id   TEXT,
  trash_root INTEGER NOT NULL DEFAULT 0,
  drive_id   TEXT
);
CREATE UNIQUE INDEX nodes_name_uq ON nodes (parent_id, name COLLATE NOCASE) WHERE trashed_at IS NULL;
CREATE INDEX nodes_parent ON nodes (parent_id);
CREATE INDEX nodes_owner ON nodes (owner_id, kind);
CREATE INDEX nodes_trash ON nodes (owner_id, trash_root) WHERE trash_root = 1;
CREATE INDEX nodes_trash_id ON nodes (trash_id) WHERE trash_id IS NOT NULL;
-- Trash listings and purges filter by drive or by trashed_at
CREATE INDEX nodes_trash_drive ON nodes (drive_id, trashed_at) WHERE trash_root = 1;
CREATE INDEX nodes_blob ON nodes (blob_hash);
CREATE INDEX nodes_drive ON nodes (drive_id, kind);

-- Access granted on a drive root or folder to a user, a group or everyone.
CREATE TABLE grants (
  id             INTEGER PRIMARY KEY,
  node_id        TEXT NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
  principal_type TEXT NOT NULL CHECK (principal_type IN ('user', 'group', 'everyone')),
  principal_id   INTEGER NOT NULL DEFAULT 0,
  role           TEXT NOT NULL CHECK (role IN ('viewer', 'editor', 'manager', 'owner')),
  granted_by     INTEGER,
  created_at     INTEGER NOT NULL,
  expires_at     INTEGER,
  UNIQUE (node_id, principal_type, principal_id)
);
CREATE INDEX grants_principal ON grants (principal_type, principal_id);

-- Per-user favourites.
CREATE TABLE favorites (
  user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  node_id    TEXT NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (user_id, node_id)
);
CREATE INDEX favorites_node ON favorites (node_id);

-- Resumable (tus) uploads in progress; data lives in the data directory's tmp/.
CREATE TABLE uploads (
  id         TEXT PRIMARY KEY,
  owner_id   INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  parent_id  TEXT NOT NULL,
  -- Relative path inside an uploaded folder
  rel_path   TEXT NOT NULL DEFAULT '',
  name       TEXT NOT NULL,
  size       INTEGER NOT NULL,
  offset     INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  drive_id   TEXT
);
-- Quota checks sum the uploads of a drive on every upload and save; the hourly cleanup looks for expired ones
CREATE INDEX uploads_drive ON uploads (drive_id);
CREATE INDEX uploads_expires ON uploads (expires_at);

-- Public share links.
CREATE TABLE shares (
  id            TEXT PRIMARY KEY,
  node_id       TEXT NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
  owner_id      INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  password_hash TEXT,
  expires_at    INTEGER,
  max_downloads INTEGER,
  downloads     INTEGER NOT NULL DEFAULT 0,
  created_at    INTEGER NOT NULL
);
CREATE INDEX shares_owner ON shares (owner_id);
CREATE INDEX shares_node ON shares (node_id);

-- ───────────── Logs ─────────────
-- Rows older than the retention period are moved into compressed archives (log_archives) every day.

CREATE TABLE activity (
  id        INTEGER PRIMARY KEY,
  at        INTEGER NOT NULL,
  user_id   INTEGER,
  -- Names are copied so the log stays readable after users or files are deleted
  username  TEXT NOT NULL DEFAULT '',
  drive_id  TEXT,
  node_id   TEXT,
  node_name TEXT NOT NULL DEFAULT '',
  action    TEXT NOT NULL,
  detail    TEXT NOT NULL DEFAULT ''
);
CREATE INDEX activity_drive ON activity (drive_id, at);
CREATE INDEX activity_at ON activity (at);
CREATE INDEX activity_user ON activity (user_id, at);
CREATE INDEX activity_action ON activity (action, at);

-- Visits to share links (open, password attempts, preview, download).
CREATE TABLE share_access (
  id         INTEGER PRIMARY KEY,
  at         INTEGER NOT NULL,
  share_id   TEXT NOT NULL,
  -- Owner of the share, so owners can list access to their own links
  owner_id   INTEGER,
  node_id    TEXT,
  node_name  TEXT NOT NULL DEFAULT '',
  event      TEXT NOT NULL,
  -- Empty when visitor details are not recorded
  ip         TEXT NOT NULL DEFAULT '',
  user_agent TEXT NOT NULL DEFAULT ''
);
CREATE INDEX share_access_share ON share_access (share_id, at);
CREATE INDEX share_access_owner ON share_access (owner_id, at);
CREATE INDEX share_access_at ON share_access (at);

CREATE TABLE login_log (
  id         INTEGER PRIMARY KEY,
  at         INTEGER NOT NULL,
  -- NULL when the username does not exist
  user_id    INTEGER,
  -- The username that was typed (also recorded for unknown users)
  username   TEXT NOT NULL,
  event      TEXT NOT NULL,
  ip         TEXT NOT NULL DEFAULT '',
  user_agent TEXT NOT NULL DEFAULT '',
  -- password, microsoft, google or github
  method     TEXT NOT NULL DEFAULT 'password'
);
CREATE INDEX login_log_user ON login_log (user_id, at);
CREATE INDEX login_log_at ON login_log (at);

-- Compressed archives of old log rows (data/archives/*.jsonl.gz).
CREATE TABLE log_archives (
  id         INTEGER PRIMARY KEY,
  kind       TEXT NOT NULL,
  from_at    INTEGER NOT NULL,
  to_at      INTEGER NOT NULL,
  rows       INTEGER NOT NULL,
  bytes      INTEGER NOT NULL,
  file       TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
