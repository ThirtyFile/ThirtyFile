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
  provisioned_by TEXT,
  -- Two-factor sign-in: the shared secret of the authenticator app (TOTP, RFC 6238, base32), encrypted like the other
  -- saved secrets (secrets.rs); NULL = not set up. Unlike passwords it has to stay readable to check codes against it.
  totp_secret   TEXT,
  -- The 30-second step of the last accepted code: a code (or an older one) can't be used again
  totp_last_step INTEGER NOT NULL DEFAULT 0,
  -- Where email notifications go. Accounts made by an administrator have none until the person enters one; signing in
  -- with single sign-on fills it in from the provider's verified email when it is still blank
  email         TEXT NOT NULL DEFAULT '',
  -- The interface language and time zone the person last used (for the text and times of emails): '' = the system
  -- default language; the offset in minutes as the browser gives it (UTC − local time, so UTC+8 is -480)
  lang          TEXT NOT NULL DEFAULT '',
  tz_offset     INTEGER NOT NULL DEFAULT 0,
  -- The password was chosen by an administrator (a new account, a reset): the person sets their own at the next
  -- sign-in before doing anything else
  must_change_password INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX users_source_created ON users (source, created_at);

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

-- The highest id ever given to a user or group. SQLite gives a new row the highest existing id plus one, so after the
-- newest user or group was deleted, the next one would get its id, and with it the log entries and sign-in settings
-- that still point at the deleted one. New ids are taken from here and never repeat.
CREATE TABLE id_counters (
  name TEXT PRIMARY KEY,
  last INTEGER NOT NULL
);
INSERT INTO id_counters (name, last) VALUES ('users', 0), ('groups', 0);

-- Signed-in devices: login sessions, which people see (and can sign out) under "My account › Devices", and
-- administrators per user. Only a hash of the cookie token is stored.
CREATE TABLE sessions (
  token_hash   TEXT PRIMARY KEY,
  user_id      INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  created_at   INTEGER NOT NULL,
  expires_at   INTEGER NOT NULL,
  -- A public id for each session: the token hash stays on the server
  id           TEXT,
  -- The browser that signed in (User-Agent, at most 300 characters)
  user_agent   TEXT NOT NULL DEFAULT '',
  -- The address the device used most recently
  ip           TEXT NOT NULL DEFAULT '',
  -- How it signed in: password or the single sign-on provider
  method       TEXT NOT NULL DEFAULT 'password',
  -- Updated at most every few minutes, not on every request
  last_used_at INTEGER
);
CREATE INDEX sessions_user ON sessions (user_id);
CREATE INDEX sessions_expires ON sessions (expires_at);
CREATE UNIQUE INDEX sessions_id ON sessions (id);

-- App passwords: tokens a person creates for scripts, backups and file clients (and accounts that only sign in through
-- single sign-on), sent as `Authorization: Bearer <token>` or as the password of HTTP Basic sign-in. They work for file
-- operations only. The token is shown once when it is created; only its hash is kept.
CREATE TABLE app_passwords (
  -- Public part of the token (`tfa_<id>_<secret>`), used to find it
  id           TEXT PRIMARY KEY,
  user_id      INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  name         TEXT NOT NULL,
  -- SHA-256 of the whole token
  token_hash   TEXT NOT NULL,
  -- 'read': downloads and listings only; 'write': everything a file operation can do
  scope        TEXT NOT NULL CHECK (scope IN ('read', 'write')),
  created_at   INTEGER NOT NULL,
  -- NULL = never expires
  expires_at   INTEGER,
  -- Updated at most every few minutes
  last_used_at INTEGER,
  last_ip      TEXT NOT NULL DEFAULT ''
);
CREATE INDEX app_passwords_user ON app_passwords (user_id);

-- Two-factor sign-in: single-use recovery codes, for when the authenticator is lost; only their SHA-256 is stored.
-- Single sign-on and app passwords don't ask for a second step.
CREATE TABLE recovery_codes (
  user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  code_hash  TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  used_at    INTEGER,
  PRIMARY KEY (user_id, code_hash)
);

-- "Forgot password": single-use links sent by email, kept by the SHA-256 of their token, valid for an hour.
CREATE TABLE password_resets (
  token_hash TEXT PRIMARY KEY,
  user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL
);
CREATE INDEX password_resets_user ON password_resets (user_id);

-- External sign-in identities (single sign-on providers) linked to users.
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

-- ───────────── Storage ─────────────

-- Where file contents are stored. The built-in `local` location is the storage folder.
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

-- The content store: file contents addressed by SHA-256 and deduplicated across the whole system.
CREATE TABLE blobs (
  hash        TEXT PRIMARY KEY,
  size        INTEGER NOT NULL,
  -- Number of nodes and versions referencing this content; deleted from storage when it drops to 0
  refcount    INTEGER NOT NULL DEFAULT 0,
  created_at  INTEGER NOT NULL,
  location_id TEXT NOT NULL DEFAULT 'local'
);
-- The storage list sums the content of each location: a covering index answers it without reading the blobs table
CREATE INDEX blobs_location_size ON blobs (location_id, size);

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

-- Spaces: every user's personal drive, the company-wide drive and team drives.
-- A space keeps its files either in the content store of a storage location (`store`), or as an ordinary folder on the
-- server (`folder`, folders.rs). In a folder space the folder is the source of truth and the nodes are an index of it,
-- kept up to date by scanning, so changes made outside ThirtyFile (SMB, rsync, a scanner) show up too.
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
  -- The storage location the space was created on, or an administrator moved it to (locations.rs): a content-store
  -- space keeps its files there, a folder space made on the built-in storage or a Local folder location has its folder
  -- in that location's folder. Changing the default location doesn't change it: the default is only where new spaces
  -- go. NULL only for a folder space showing a folder an administrator chose, which is on no location.
  location_id TEXT REFERENCES storage_locations (id),
  -- 'store': files in a storage location, named by their content; 'folder': the files in `source_path` as they are
  mode        TEXT NOT NULL DEFAULT 'store' CHECK (mode IN ('store', 'folder')),
  -- Absolute path of the folder (folder spaces)
  source_path TEXT,
  -- Browse, download and share only
  read_only   INTEGER NOT NULL DEFAULT 0,
  last_scan_at INTEGER,
  -- What the last scan found and skipped (JSON), shown in the Control panel
  scan_report TEXT,
  CHECK (location_id IS NOT NULL OR mode = 'folder')
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
  drive_id   TEXT,
  -- Where a node of a folder space is: its path below the space's folder ('' for the root, 'a/b.txt' below it)
  fs_path    TEXT,
  -- The file system's identity of the item: a path that disappears while the same identity shows up elsewhere was moved
  fs_dev     INTEGER,
  fs_ino     INTEGER,
  -- Size and modification time (nanoseconds) when last indexed: a change means the file changed
  fs_size    INTEGER,
  fs_mtime_ns INTEGER,
  -- Names are unique in a folder by this key. In the content store letter case doesn't count, in every language
  -- ("Été" and "été" are the same name, as Rust's to_lowercase() and Windows see them); a folder on disk can hold both
  -- "A.txt" and "a.txt", so in folder spaces it is the name itself. unicode_lower() is registered by the server on
  -- every connection (db.rs).
  name_key   TEXT GENERATED ALWAYS AS (CASE WHEN fs_path IS NULL THEN unicode_lower(name) ELSE name END) VIRTUAL,
  -- Who last gave the file new content (NULL = whoever created it): the author of the version it becomes next
  content_by INTEGER,
  -- Who moved an item to the trash, shown in the trash's Deleted by column. Set on the trash root only (the item the
  -- person deleted). No foreign key: deleting a user clears it instead (admin.rs), like other references to deleted users.
  trashed_by INTEGER
);
CREATE UNIQUE INDEX nodes_name_uq ON nodes (parent_id, name_key) WHERE trashed_at IS NULL;
CREATE INDEX nodes_parent ON nodes (parent_id);
CREATE INDEX nodes_owner ON nodes (owner_id, kind);
-- Recent lists a person's own files newest first: without this index SQLite sorts all of them on every request, which
-- for the owner of a large space (scanned files belong to the space root's owner) is every file in it
CREATE INDEX nodes_owner_recent ON nodes (owner_id, kind, updated_at);
CREATE INDEX nodes_trash_id ON nodes (trash_id) WHERE trash_id IS NOT NULL;
-- Trash listings and purges filter by drive or by trashed_at
CREATE INDEX nodes_trash_drive ON nodes (drive_id, trashed_at) WHERE trash_root = 1;
CREATE INDEX nodes_blob ON nodes (blob_hash);
-- Space totals (at startup, daily and after moves) sum the files of each space: with the size in the index they are
-- read from the index alone, instead of from every row of the nodes table
CREATE INDEX nodes_drive ON nodes (drive_id, kind, size);
CREATE INDEX nodes_fs_path ON nodes (drive_id, fs_path) WHERE fs_path IS NOT NULL;
CREATE INDEX nodes_fs_ino ON nodes (fs_dev, fs_ino) WHERE fs_ino IS NOT NULL;

-- Search by name with a trigram index: finds a term anywhere in a name without reading every node, ignoring letter
-- case and accents in every language. Terms shorter than three characters, which the index can't look up, use a scan.
CREATE VIRTUAL TABLE nodes_fts USING fts5(name, content='nodes', content_rowid='rowid', tokenize='trigram remove_diacritics 1');

CREATE TRIGGER nodes_fts_insert AFTER INSERT ON nodes BEGIN
  INSERT INTO nodes_fts (rowid, name) VALUES (new.rowid, new.name);
END;
CREATE TRIGGER nodes_fts_delete AFTER DELETE ON nodes BEGIN
  INSERT INTO nodes_fts (nodes_fts, rowid, name) VALUES ('delete', old.rowid, old.name);
END;
CREATE TRIGGER nodes_fts_rename AFTER UPDATE OF name ON nodes BEGIN
  INSERT INTO nodes_fts (nodes_fts, rowid, name) VALUES ('delete', old.rowid, old.name);
  INSERT INTO nodes_fts (rowid, name) VALUES (new.rowid, new.name);
END;

-- Earlier versions of files: the content a file had before it was saved over in the editor, replaced by an upload,
-- or restored to another version. In the content store a version holds a reference to its content, counted in
-- blobs.refcount like a file's, so the content stays as long as the version does. In folder spaces the old file is
-- kept in the space's folder, under `.thirtyfile-versions/<file id>/<version id>` (never indexed).
-- Versions stay with their file wherever it moves; they go when the file is deleted for good, or when the system
-- settings no longer keep them (versions.rs). They don't count toward a space's quota.
CREATE TABLE node_versions (
  id          TEXT PRIMARY KEY,
  node_id     TEXT NOT NULL,
  -- Content store: the content
  blob_hash   TEXT,
  -- Folder spaces: the space whose folder holds the version, and its path below that folder
  drive_id    TEXT,
  fs_path     TEXT,
  size        INTEGER NOT NULL,
  -- Who wrote this content (the name is copied so it stays readable after the account is deleted)
  author_id   INTEGER,
  author_name TEXT NOT NULL DEFAULT '',
  -- When the file got this content
  modified_at INTEGER NOT NULL,
  -- When it was replaced, i.e. became a version (versions are kept for a number of days from then)
  created_at  INTEGER NOT NULL
);
CREATE INDEX node_versions_node ON node_versions (node_id);
CREATE INDEX node_versions_created ON node_versions (created_at);
CREATE INDEX node_versions_blob ON node_versions (blob_hash) WHERE blob_hash IS NOT NULL;

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

-- Files each person opened (content or preview, in the browser), for Recent. One row per person and file with the
-- latest time; updated at most once a minute, and only the most recent few hundred per person are kept (nodes.rs).
CREATE TABLE recent_files (
  user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  node_id TEXT NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
  at      INTEGER NOT NULL,
  PRIMARY KEY (user_id, node_id)
);
-- Deleting a node looks up its rows here
CREATE INDEX recent_files_node ON recent_files (node_id);

-- ───────────── Uploads ─────────────

-- Resumable (tus) uploads; data lives in the data directory's tmp/. Uploads are kept after they finish, with the file
-- they created, until they expire: when the response to the last request is lost (a proxy timeout, a dropped
-- connection), the client asks again and learns that the upload finished and which file it became.
CREATE TABLE uploads (
  id          TEXT PRIMARY KEY,
  owner_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  parent_id   TEXT NOT NULL,
  -- Relative path inside an uploaded folder
  rel_path    TEXT NOT NULL DEFAULT '',
  name        TEXT NOT NULL,
  size        INTEGER NOT NULL,
  offset      INTEGER NOT NULL DEFAULT 0,
  created_at  INTEGER NOT NULL,
  expires_at  INTEGER NOT NULL,
  drive_id    TEXT,
  -- The folder upload it is part of (upload_batch_folders)
  batch       TEXT NOT NULL DEFAULT '',
  -- The file it created, once finished
  node_id     TEXT,
  -- Uploads made through a share link: the link (NULL for uploads by signed-in people). They belong to the link's
  -- creator (owner_id), and only requests through the same link can continue them.
  share_id    TEXT,
  -- What an upload does when its folder already has an item with the same name: '' or 'keep' = keep both (the new
  -- file gets a number), 'replace' = the existing file gets the new content and keeps its id, shares and permissions.
  -- The browser asks before uploading; the answer travels with the upload so it still applies when it resumes.
  on_conflict TEXT NOT NULL DEFAULT '',
  -- Uploads into the content store are hashed while they arrive (upload.rs), so finishing one doesn't read the whole
  -- file again. The SHA-256 state after the first `hashed` bytes is saved together with the offset: a paused upload,
  -- also after a restart, continues from it. A state that doesn't match the offset is rebuilt by reading the part
  -- received so far once.
  hash_state  BLOB,
  hashed      INTEGER NOT NULL DEFAULT 0
);
-- Quota checks sum the uploads of a drive on every upload and save; the hourly cleanup looks for expired ones
CREATE INDEX uploads_drive ON uploads (drive_id);
CREATE INDEX uploads_expires ON uploads (expires_at);
CREATE INDEX uploads_share ON uploads (share_id) WHERE share_id IS NOT NULL;

-- Folders created while uploading a folder, per upload batch.
-- Each file of an uploaded folder is its own upload. When a folder name on the way is already used by a file, the
-- first upload of the batch creates a numbered folder ("Photos (1)") and the others must find that same folder again.
CREATE TABLE upload_batch_folders (
  batch      TEXT NOT NULL,
  parent_id  TEXT NOT NULL,
  -- The name the uploaded folder has on the uploader's computer
  name       TEXT NOT NULL,
  folder_id  TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (batch, parent_id, name)
);
CREATE INDEX upload_batch_folders_created ON upload_batch_folders (created_at);

-- ───────────── Shares ─────────────

-- Public share links. A folder link can accept uploads from its visitors (allow_upload), optionally without showing
-- them what is in the folder (drop_only: they can only upload). allow_download = 0 serves files for previews only: no
-- download or ZIP.
CREATE TABLE shares (
  id             TEXT PRIMARY KEY,
  node_id        TEXT NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
  owner_id       INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  password_hash  TEXT,
  expires_at     INTEGER,
  max_downloads  INTEGER,
  downloads      INTEGER NOT NULL DEFAULT 0,
  created_at     INTEGER NOT NULL,
  -- Views and the last access, kept on the share: the access log is archived and trimmed after its retention period
  views          INTEGER NOT NULL DEFAULT 0,
  last_access    INTEGER,
  allow_upload   INTEGER NOT NULL DEFAULT 0,
  drop_only      INTEGER NOT NULL DEFAULT 0,
  allow_download INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX shares_owner ON shares (owner_id);
CREATE INDEX shares_node ON shares (node_id);

-- ───────────── Notifications ─────────────
-- Shown under the bell in the app and, when an administrator set up an email server, sent by email.

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

-- ───────────── Logs ─────────────
-- Rows older than the retention period are moved into compressed archives (log_archives) every day.
-- Log pages filter by one column and page newest first by id (ORDER BY id DESC). An index on the column alone ends
-- in the row id, so SQLite reads matching rows already in that order, instead of sorting every match by `at` first.

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
CREATE INDEX activity_at ON activity (at);
CREATE INDEX activity_drive ON activity (drive_id);
CREATE INDEX activity_action ON activity (action);
-- Recent also lists the files a person edited, from their newest activity entries
CREATE INDEX activity_user ON activity (user_id);
-- An item's history in the Details pane
CREATE INDEX activity_node ON activity (node_id);

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
CREATE INDEX share_access_at ON share_access (at);
CREATE INDEX share_access_share ON share_access (share_id);
CREATE INDEX share_access_owner ON share_access (owner_id);

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
  -- password or the single sign-on provider
  method     TEXT NOT NULL DEFAULT 'password'
);
CREATE INDEX login_log_at ON login_log (at);
CREATE INDEX login_log_user ON login_log (user_id);

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
