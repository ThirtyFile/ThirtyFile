//! What the server's API sends and takes

import type { Branding } from "@/lib/branding";
import type { SORT_KEYS } from "@/api/files";
import type { Lang } from "@/lib/i18n";
import type { StyleChoice } from "@/lib/style/device";

export interface Node {
  id: string;
  parent_id: string | null;
  kind: "folder" | "file";
  name: string;
  size: number;
  mime: string;
  created_at: number;
  updated_at: number;
  trashed_at: number | null;
  /** Owning space */
  drive_id: string | null;
  /** Uploader / creator */
  owner_name: string;
  is_favorite: boolean;
  /** The signed-in person's tags on it (ids of their `Tag`s); missing when it has none of theirs */
  tags?: number[];
  /** Listings of folders only (the navigation pane): whether it has folders in it */
  has_folders?: boolean;
}

/** The colours a tag can have: the server keeps the name, each style and theme draws it in its own shade */
export type TagColor = "red" | "orange" | "yellow" | "green" | "blue" | "purple" | "gray";

/** One of the signed-in person's tags: only they see it, its name and what it is on */
export interface Tag {
  id: number;
  name: string;
  color: TagColor;
}

/** A task running on the server (`GET /jobs/:id`): a change that takes a while (see lib/jobs) */
export interface Job {
  /** Empty for a change that was done at once */
  id: string;
  kind: "compress" | "extract" | "move" | "copy" | "delete" | "empty_trash" | "scan" | "delete_user" | "remove_personal" | "webdav";
  state: "running" | "done" | "failed";
  /** Work done so far, of `total` (bytes, or items) */
  done: number;
  total: number;
  /** Why it failed (English, from the server) */
  error: string | null;
  /** The new ZIP file or folder, and its name */
  node_id: string | null;
  name: string | null;
  /** What a finished task reports besides (checking a folder: its report) */
  result?: unknown;
}

export interface SearchFilter {
  /** Folder id: that folder and below */
  in?: string;
  kind?: "file" | "folder";
  /** Extensions, comma separated */
  ext?: string;
  /** Modified from / before (Unix seconds) */
  from?: number;
  to?: number;
  min_size?: number;
  max_size?: number;
  /** Only items with all of these tags of the person's own (ids, comma separated); with tags, the search term may be empty */
  tags?: string;
}

/** Where a smart folder looks: everywhere the person has access, one space, or a folder and its subfolders */
export type SmartScope = { kind: "all" } | { kind: "space"; id: string } | { kind: "folder"; id: string };

/** What a smart folder looks for (server/src/nodes/smart.rs): what is left out doesn't narrow it down */
export interface SmartQuery {
  /** In the name */
  name?: string;
  kind?: "file" | "folder";
  /** Extensions, comma separated, lowercase, without the dot */
  ext?: string;
  /** Size in bytes (files) */
  min_size?: number;
  max_size?: number;
  /** Modified from / before (Unix seconds) */
  modified_from?: number;
  modified_to?: number;
  /** Modified in the last this many days, counted when it is opened (instead of dates) */
  modified_days?: number;
  scope: SmartScope;
  /** With all of these tags of the person's own */
  tags?: number[];
}

/** One of the signed-in person's smart folders: a saved search that shows as a folder; only they see it */
export interface SmartFolder {
  id: number;
  name: string;
  query: SmartQuery;
}

export interface Located extends Node {
  /** Where the item is, e.g. "All files/Projects/2026", in the interface's language (built by `localizeLocated`) */
  location: string;
  /** The space the item is in; null when it is only reached through something shared with the person */
  location_space: { kind: string; name: string } | null;
  /** Folders from the space root (or the shared folder) down to the item's parent */
  location_path: string[];
  /** Trash: who moved the item there; missing when unknown (deleted before this was recorded, or by a removed account) */
  deleted_by?: string;
}

/** What folders hold, at any depth (not counting the folders themselves; items in the trash are left out) */
export interface FolderContents {
  size: number;
  files: number;
  folders: number;
}

export interface Crumb {
  id: string;
  name: string;
}

export type Role = "viewer" | "editor" | "manager" | "owner";
export type DriveKind = "personal" | "company" | "team";

export interface NodeInfo {
  node: Node;
  /** From the space root (exclusive) to this node; when accessed via a shared folder, starts at the shared folder */
  path: Crumb[];
  is_root: boolean;
  drive: { id: string; name: string; kind: DriveKind; root_id: string };
  role: Role;
  via_share: boolean;
  /** The path that names it, as typed into the address bar or used over WebDAV: ["My files", "Reports"] (see lib/paths.ts); null when no path reaches it */
  location: string[] | null;
  /** Why the storage service holding the content is offline (e.g. S3 disconnected) */
  offline: string | null;
  /** A read-only space: browse, download and share only */
  read_only: boolean;
  /** The space is being moved to another storage location, and is read-only until the move finishes */
  moving: boolean;
}

/** What a path typed into the address bar names (`api.findPath`) */
export interface FoundPath {
  place: "spaces" | "shared" | "folder" | "file";
  id: string | null;
  /** The path as it is named (letter case as stored) */
  path: string[];
}

/** What the last scan of a folder space found */
export interface ScanReport {
  at: number;
  added: number;
  changed: number;
  moved: number;
  removed: number;
  /** Items that couldn't be indexed, with the reason; empty in someone else's personal space, which has `skipped_count` */
  skipped: string[];
  skipped_count?: number;
  error: string | null;
}

export interface Drive {
  id: string;
  name: string;
  kind: DriveKind;
  root_id: string;
  role: Role | null;
  used_bytes: number;
  quota_bytes: number;
  owner_name: string;
  member_count: number;
  disabled: boolean;
  /** The storage location the space is on (kept when the default changes); null for a folder an administrator chose */
  location_id: string | null;
  location_name: string;
  /** Why the storage service is offline: can be browsed, but not opened, downloaded or uploaded to */
  offline: string | null;
  /** "folder": the space shows a folder on the server */
  mode: "store" | "folder";
  /** Browse, download and share only (folder spaces) */
  read_only: boolean;
  /** Folder spaces, for administrators */
  source_path?: string;
  last_scan_at?: number | null;
  scan_report?: ScanReport | null;
  /** A scan running now */
  scanning?: { phase: "reading" | "indexing"; found: number; done: number; total: number; started_at: number };
}

export type StorageKind = "local" | "s3" | "sftp" | "ftp";

export interface StorageConfig {
  path?: string;
  endpoint?: string;
  region?: string;
  bucket?: string;
  prefix?: string;
  access_key_id?: string;
  secret_access_key?: string;
  path_style?: boolean;
  allow_http?: boolean;
  /** SFTP／FTP */
  host?: string;
  port?: number;
  username?: string;
  password?: string;
  private_key?: string;
  key_passphrase?: string;
  /** SFTP host key fingerprint (recorded on first connection) */
  host_key?: string;
  /** FTP: use FTPS (AUTH TLS) */
  tls?: boolean;
  tls_insecure?: boolean;
}

export interface StorageLocation {
  id: string;
  name: string;
  kind: StorageKind;
  builtin: boolean;
  is_default: boolean;
  config: StorageConfig;
  has_secret: boolean;
  /** Config loaded and the latest connection check succeeded (checked every 30 seconds) */
  connected: boolean;
  /** Why the latest connection check failed */
  health_error: string | null;
  checked_at: number | null;
  /** Number of files whose deletion failed and will be retried once the connection recovers */
  pending_deletes: number;
  /** The content store there plus the indexed size of the folder spaces on it */
  used_bytes: number;
  /** Of used_bytes, the folder spaces' part */
  folder_bytes: number;
  /** Files in the content store there */
  blob_count: number;
  /** Files in the folder spaces on it */
  folder_files: number;
  /** Spaces on this location, of every kind */
  drive_count: number;
  /** Locations on this server's disks: the disk's free and total bytes (null when unknown) */
  disk_free_bytes: number | null;
  disk_total_bytes: number | null;
}

/** A space on a storage location: what it is, not what is in it */
export interface LocationSpace {
  id: string;
  name: string;
  kind: DriveKind;
  mode: "store" | "folder";
  /** Personal spaces: the owner's user name */
  owner_name: string;
  used_bytes: number;
  /** Folder spaces: their folder on the server */
  source_path?: string;
}

/** One step of a storage location's step-by-step test */
export interface LocationTestStep {
  id: "connect" | "write" | "read" | "write_large" | "read_large" | "delete" | "cleanup";
  outcome: "ok" | "error" | "skipped";
  ms: number;
  bytes: number | null;
  /** MB (10^6 bytes) per second */
  speed: number | null;
  message: string | null;
}

/** A space as the storage location tools show it; private: someone else's personal space, whose files aren't shown */
export interface LocationItemSpace {
  id: string;
  name: string;
  kind: DriveKind;
  owner: string;
  private: boolean;
}

/** One item of a storage location, while browsing it */
export interface LocationItem {
  name: string;
  kind: "folder" | "file" | "link";
  size: number;
  modified: number | null;
  path: string;
  role: "content" | "internal" | "space" | null;
  space: LocationItemSpace | null;
  usage: {
    status: "used" | "version" | "trash" | "replica" | "pending" | "unused";
    space: LocationItemSpace | null;
    file: string | null;
    uses: number;
  } | null;
}

export interface LocationPage {
  path: string;
  items: LocationItem[];
  next: string | null;
  space: LocationItemSpace | null;
}

/** A search for unused content in a storage location, and its removal */
export interface UnusedJob {
  scan_id: string;
  phase: "scanning" | "found" | "removing" | "removed" | "failed";
  started_at: number;
  finished_at: number | null;
  scanned: number;
  count: number;
  bytes: number;
  items: { hash: string; path: string; size: number; modified: number | null }[];
  recent: number;
  error: string | null;
  removed: number;
  removed_bytes: number;
  kept: number;
  failed: number;
}

export type MoveState = "queued" | "running" | "paused" | "failed" | "done" | "cancelled";

/** A move of a space to another storage location (Control panel › Moves) */
export interface SpaceMove {
  id: string;
  drive_id: string;
  space_name: string;
  space_kind: DriveKind;
  /** Personal spaces: the owner's user name */
  owner_name: string;
  /** null: a folder an administrator chose, on no location */
  from_location: string | null;
  from_name: string;
  from_mode: "store" | "folder";
  to_location: string;
  to_name: string;
  to_mode: "store" | "folder";
  state: MoveState;
  files_total: number;
  bytes_total: number;
  files_done: number;
  bytes_done: number;
  failed_items: number;
  /** The first items that couldn't be copied; `item` is null in personal spaces */
  failures: { item: string | null; error: string }[];
  error: string | null;
  /** What a finished move left behind, or changed */
  note: string | null;
  created_by_name: string;
  created_at: number;
  started_at: number | null;
  finished_at: number | null;
  /** Running moves: bytes copied per second lately */
  speed?: number | null;
}

export interface MovesList {
  moves: SpaceMove[];
  /** Moves that run at the same time */
  concurrency: number;
}

export type BackupJobState = "queued" | "running" | "paused" | "waiting" | "failed" | "done" | "cancelled";
export type BackupJobKind = "snapshot" | "restore" | "verify" | "remove";

/** A space held in a copy (Control panel › Backups): what it is, never what is in it */
export interface BackupSpace {
  id: string;
  name: string;
  kind: DriveKind;
  /** Personal spaces: the owner's user name */
  owner: string;
  mode: "store" | "folder";
  files: number;
  bytes: number;
}

/** What a copy held at a point in time; only a complete one can be restored from */
export interface BackupSnapshot {
  id: string;
  state: "making" | "complete";
  /** When the spaces were read: the copy shows them as they were then */
  cutoff: number | null;
  spaces: BackupSpace[];
  folders: number;
  files: number;
  versions: number;
  logical_bytes: number;
  created_at: number;
  completed_at: number | null;
}

/** A copy of a storage location's spaces, kept on another location */
export interface BackupSet {
  id: string;
  kind: "copy" | "policy" | "imported";
  name: string;
  source_location: string | null;
  source_name: string;
  dest_location: string;
  dest_name: string;
  created_by_name: string;
  created_at: number;
  /** Being deleted from its location */
  removing: boolean;
  /** Content held on the destination (each content once) */
  objects: number;
  bytes: number;
  snapshots: BackupSnapshot[];
  /** Backups made by a policy: its settings and how it is doing */
  policy: BackupPolicy | null;
}

/** When a policy makes snapshots on a schedule: every N minutes, daily at a time, or on some days of the week */
export type BackupSchedule = { every: number } | { daily: string } | { weekly: string; days: number[] };

export type BackupHealthState = "protected" | "catching_up" | "running" | "waiting" | "failing" | "overdue" | "paused" | "never";

/** A backup policy's settings */
export interface BackupPolicySettings {
  enabled: boolean;
  mode: "realtime" | "scheduled" | "both";
  schedule: BackupSchedule;
  tz: string;
  /** Every space on the location, those added later too; else `spaces` */
  all_spaces: boolean;
  spaces: string[];
  versions: boolean;
  trash: boolean;
  keep_days: number;
  keep_min: number;
  /** Bytes per second, 0: no limit */
  rate_limit: number;
  alert_hours: number;
  verify_days: number;
}

export interface BackupPolicy extends BackupPolicySettings {
  next_run_at: number | null;
  last_run_at: number | null;
  last_verify_at: number | null;
  created_at: number;
  updated_at: number;
  health: {
    state: BackupHealthState;
    /** When the newest complete snapshot read the spaces */
    protected_through: number | null;
    /** Since when changes wait for a snapshot */
    behind_since: number | null;
    changed_spaces: number;
    error: string | null;
  };
}

/** Work on a copy: making it, restoring from it, checking it, deleting it */
export interface BackupJob {
  id: string;
  kind: BackupJobKind;
  set_id: string;
  snapshot_id: string | null;
  state: BackupJobState;
  /** The copy's name; for a restore the space's */
  label: string;
  files_total: number;
  bytes_total: number;
  files_done: number;
  bytes_done: number;
  failed_items: number;
  /** The first items that couldn't be done; `item` is null in personal spaces */
  failures: { item: string | null; error: string }[];
  error: string | null;
  note: string | null;
  created_by_name: string;
  created_at: number;
  started_at: number | null;
  finished_at: number | null;
  speed?: number | null;
  /** Restores: the space restored into */
  target?: string | null;
}

export interface BackupsOverview {
  sets: BackupSet[];
  jobs: BackupJob[];
}

/** What "Copy everything to…" would copy, and whether the destination can take it */
export interface CopyPreview {
  source_name: string;
  dest_name: string;
  dest_kind: StorageKind;
  spaces: { id: string; name: string; kind: DriveKind; owner_name: string; mode: "store" | "folder"; used_bytes: number }[];
  files: number;
  trash_files: number;
  versions: number;
  /** Files and versions, identical content counted every time */
  bytes: number;
  /** What goes to the destination: each content once */
  content_bytes: number;
  /** On a disk of this server; null when it can't be told */
  free_bytes: number | null;
  /** On the same disk or storage service as the source */
  shared: boolean;
  /** FTP without TLS */
  unencrypted: boolean;
  /** Why the destination can't be used now */
  problem: string | null;
}

/** What restoring from a copy or backup would do */
export interface RestorePreview {
  space: BackupSpace;
  target_drive: string | null;
  target_name: string | null;
  folder_name: string;
  targets: { id: string; name: string; kind: DriveKind }[];
  /** It can go back into its original place: the space is still there */
  original: boolean;
  files: number;
  bytes: number;
  /** Into the original place: files with an item where they go, of the first `checked` */
  conflicts: number | null;
  checked: number;
  problem: string | null;
}

/** What to restore, and how */
export interface RestoreRequest {
  space: string;
  folder?: string | null;
  items?: string[] | null;
  target_drive?: string | null;
  mode?: "new_folder" | "original";
  on_conflict?: "skip" | "keep" | "replace";
  trash?: boolean;
  tz?: number;
  folder_name?: string;
}

/** An item of a snapshot, while choosing what to restore */
export interface SnapshotItem {
  id: string;
  name: string;
  kind: "folder" | "file";
  size: number;
  modified: number;
  trashed: number | null;
}

export type ReplicaTargetState = "current" | "behind" | "syncing" | "initializing" | "offline" | "failed" | "corrupt" | "stale" | "paused";

/** A location a replica policy keeps copies on */
export interface ReplicaTarget {
  location_id: string;
  name: string;
  priority: number;
  mode: "realtime" | "scheduled";
  schedule: BackupSchedule;
  tz: string;
  next_run_at: number | null;
  /** active, or stale: the old primary after a promotion, checked before it counts again */
  state: "active" | "stale";
  synced_at: number | null;
  last_run_at: number | null;
  last_verify_at: number | null;
}

/** A replica policy: the spaces of a location, copied to other locations */
export interface ReplicaPolicy {
  id: string;
  name: string;
  source_location: string;
  source_name: string;
  enabled: boolean;
  all_spaces: boolean;
  spaces: string[];
  /** Copies wanted besides the primary */
  copies: number;
  read_fallback: boolean;
  verify_days: number;
  alert_hours: number;
  rate_limit: number;
  epoch: number;
  created_by_name: string;
  created_at: number;
  updated_at: number;
  targets: ReplicaTarget[];
  /** Spaces replicated now */
  replicated: number;
  health: {
    targets: {
      location_id: string;
      state: ReplicaTargetState;
      behind_since: number | null;
      synced_at: number | null;
      last_verify_at: number | null;
      /** Contents it holds of those it should, as last worked out (by its syncs); null until then */
      held: number | null;
      wanted: number | null;
      damaged: number;
      error: string | null;
    }[];
    wanted: number;
    current: number;
    shortfall: boolean;
    /** The spaces' own location can't be reached now (a server message) */
    source_offline: string | null;
    state: "ok" | "behind" | "degraded" | "paused";
  };
}

/** A job of replicas: a sync or a check of a target */
export interface ReplicaJob {
  id: string;
  kind: "sync" | "verify";
  policy_id: string;
  location_id: string | null;
  state: BackupJobState;
  label: string;
  files_total: number;
  bytes_total: number;
  files_done: number;
  bytes_done: number;
  failed_items: number;
  failures: { item: string | null; error: string }[];
  error: string | null;
  note: string | null;
  created_by_name: string;
  created_at: number;
  started_at: number | null;
  finished_at: number | null;
  speed?: number | null;
}

export interface ReplicasOverview {
  policies: ReplicaPolicy[];
  jobs: ReplicaJob[];
  /** Copies no policy wants any more: location, its name, how many, bytes */
  unneeded: [string, string, number, number][];
}

/** A target as the settings of a replica policy send it */
export interface ReplicaTargetRequest {
  location: string;
  mode: "realtime" | "scheduled";
  schedule?: BackupSchedule;
  tz?: string;
}

export interface ReplicaPolicyRequest {
  name?: string;
  source?: string;
  targets?: ReplicaTargetRequest[];
  copies?: number;
  enabled?: boolean;
  all_spaces?: boolean;
  spaces?: string[];
  read_fallback?: boolean;
  verify_days?: number;
  alert_hours?: number;
  rate_limit?: number;
}

/** What promoting a target would do */
export interface PromotePreflight {
  source: string;
  source_name: string;
  target_name: string;
  source_reachable: boolean;
  target_reachable: boolean;
  target_state: ReplicaTargetState | "unknown";
  behind_since: number | null;
  spaces: string[];
  moved: number;
  moved_bytes: number;
  missing: number;
  missing_bytes: number;
  /** Folder spaces not wholly on the target: they stay where they are */
  folder_spaces: string[];
  needs_accept: boolean;
  problem: string | null;
}

export type PrincipalType = "user" | "group" | "everyone";

export interface Grant {
  id: number;
  node_id: string;
  principal_type: PrincipalType;
  principal_id: number;
  principal_name: string;
  role: Role;
  expires_at: number | null;
  granted_by_name: string;
  created_at: number;
  inherited_from: string | null;
}

export interface AccessInfo {
  node: Node;
  drive: { id: string; name: string; kind: DriveKind; root_id: string };
  is_drive_root: boolean;
  my_role: Role | null;
  can_manage: boolean;
  direct: Grant[];
  inherited: Grant[];
}

export interface Principal {
  principal_type: PrincipalType;
  principal_id: number;
  name: string;
  detail: string;
}

export interface Group {
  id: number;
  name: string;
  description: string;
  created_at: number;
  members: { id: number; username: string }[];
}

export interface Activity {
  id: number;
  at: number;
  username: string;
  drive_id: string | null;
  drive_name: string | null;
  node_id: string | null;
  node_name: string;
  action: string;
  detail: string;
  /** About someone else's personal space: the item and the details are left out */
  private?: boolean;
}

/** An entry of an item's history (Details pane): the item itself, or something inside the folder */
export type HistoryEntry = Omit<Activity, "drive_id" | "drive_name" | "private">;

/** Activity log filters (times are Unix seconds, start inclusive, end exclusive) */
export interface ActivityFilter {
  drive_id?: string;
  user?: string;
  /** Multiple actions separated by commas */
  action?: string;
  q?: string;
  from?: number;
  to?: number;
}

/** An entry of the error log (Control panel > Activity > Errors): one incident, counted when it happens again */
export interface ErrorEntry {
  id: number;
  /** Last and first time it happened, and how often */
  at: number;
  first_at: number;
  count: number;
  /** backend: the server answered a request with an error; frontend: the page reported it */
  source: "backend" | "frontend";
  /** error: an unexpected failure; warning: an expected refusal (validation, permission, conflict) */
  severity: "error" | "warning";
  kind: string;
  /** Null when nobody was signed in */
  user_id: number | null;
  username: string;
  operation: string;
  route: string;
  resource: string;
  status: number | null;
  code: string;
  message: string;
  detail: string;
  request_id: string | null;
  /** What the page reported about a failed request the server recorded too */
  client: string;
  version: string;
}

/** Error log filters (times are Unix seconds, start inclusive, end exclusive) */
export interface ErrorFilter {
  /** backend, frontend (comma-separated) */
  source?: string;
  /** error, warning (comma-separated) */
  severity?: string;
  user?: string;
  q?: string;
  from?: number;
  to?: number;
}

/** Editable part of the branding settings (the logo is uploaded separately) */
export type BrandingReq = Omit<Branding, "has_logo" | "has_logo_dark" | "has_login_background" | "version">;

export interface SsoProvider {
  id: "microsoft" | "google" | "github" | "oidc";
  /** The name on the button (for oidc, the one the administrator chose) */
  label: string;
}

export type NotificationKind = "shared" | "space_full" | "access_expiring" | "app_password" | "sign_in_method" | "link_upload" | "backup" | "replica";

/** What a notification shows; names are copied when it was made */
export interface NotificationData {
  /** The folder, file or space */
  name?: string;
  /** Whether a whole space was shared ("space"), or a folder or file */
  item?: "space" | "folder" | "file";
  drive_kind?: DriveKind;
  role?: Role;
  /** Who shared it */
  by?: string;
  /** When the access ends */
  expires_at?: number | null;
  /** Almost full spaces: bytes used, the space's size, and the share used */
  used?: number;
  quota?: number;
  percent?: number;
  /** Files received through a link: the last file's name, and how many arrived */
  file?: string;
  count?: number;
  /** App passwords: what it may do, and the address it was made from */
  scope?: "read" | "write";
  ip?: string;
  /** Linked sign-in methods: the provider's name, and the linked account's email (or name) */
  label?: string;
  account?: string;
  /** Backups and replicas (administrators): what happened (failing, waiting, overdue, degraded, recovered), the error,
   * when the newest complete snapshot read the spaces, and how many replicas are current of those wanted */
  state?: "failing" | "waiting" | "overdue" | "degraded" | "recovered";
  current?: number;
  wanted?: number;
  error?: string | null;
  since?: number | null;
}

/** A notification under the bell (`GET /notifications`) */
export interface AppNotification {
  id: number;
  kind: NotificationKind;
  data: NotificationData;
  /** What opening it shows (it may have been deleted since) */
  node_id: string | null;
  created_at: number;
  read: boolean;
}

export interface NotificationPrefs {
  in_app: boolean;
  email: boolean;
}

export interface NotificationSettings {
  /** Where emails go; blank = none */
  email: string;
  /** An administrator set up an email server */
  email_ready: boolean;
  kinds: Record<NotificationKind, NotificationPrefs>;
}

export type SmtpSecurity = "starttls" | "tls" | "none";

/** Control panel › Email: the server notification emails are sent through */
export interface EmailSettings {
  enabled: boolean;
  host: string;
  port: number;
  security: SmtpSecurity;
  username: string;
  /** A password is saved (it is never sent back) */
  has_password: boolean;
  from: string;
  insecure: boolean;
}

export type EmailSettingsReq = Omit<EmailSettings, "has_password" | "port"> & { port?: number; password: string };

export interface LinkedIdentity {
  provider: string;
  email: string;
  name: string;
  created_at: number;
  last_login_at: number | null;
}

/** What happens when someone signs in with an external account that isn't linked yet */
export type SsoProvisioning = "off" | "link" | "create";

/** Permissions and space size of accounts a provider creates automatically */
export interface SsoNewUserDefaults {
  can_write: boolean;
  can_delete: boolean;
  can_share: boolean;
  /** Bytes (0 = unlimited); null = the system's default for new users */
  quota_bytes: number | null;
}

export interface SsoProviderPolicy {
  provisioning: SsoProvisioning;
  /** Provider's own domain list (lowercase, without @); empty = the global list */
  allowed_domains: string[];
  defaults: SsoNewUserDefaults;
  /** Groups automatically created accounts join */
  groups: number[];
}

export interface SsoProviderSettings extends SsoProviderPolicy {
  enabled: boolean;
  client_id: string;
  has_secret: boolean;
  /** Microsoft: tenant ID or domain */
  tenant: string;
  /** OpenID Connect: the name on the sign-in button, and the issuer URL */
  name: string;
  issuer: string;
  /** Redirect URI to enter in the provider's app settings */
  redirect_uri: string;
}

/** Settings for accounts created for one email domain (wins over the provider's defaults) */
export interface SsoDomainRule {
  domain: string;
  can_write: boolean;
  can_delete: boolean;
  can_share: boolean;
  /** Bytes (0 = unlimited); null = the system's default for new users */
  quota_bytes: number | null;
  groups: number[];
  /** Give the account a personal space ("My files"); null = the system setting */
  personal_space: boolean | null;
  /** The storage location of its personal space; null = the system setting's */
  personal_location: string | null;
}

export interface SsoSettings {
  microsoft: SsoProviderSettings;
  google: SsoProviderSettings;
  github: SsoProviderSettings;
  oidc: SsoProviderSettings;
  allowed_domains: string[];
  domain_rules: SsoDomainRule[];
  /** Accounts one provider may create per hour */
  max_created_per_hour: number;
  /** Site URL is configured (only then is the redirect URI the real production URL) */
  public_url_set: boolean;
}

export type SsoProviderReq = SsoProviderPolicy & { enabled: boolean; client_id: string; client_secret: string; tenant: string; name: string; issuer: string };
export interface SsoSettingsReq {
  microsoft: SsoProviderReq;
  google: SsoProviderReq;
  github: SsoProviderReq;
  oidc: SsoProviderReq;
  allowed_domains: string[];
  domain_rules: SsoDomainRule[];
}

export interface Page<T> {
  items: T[];
  /** `before` parameter for the next page; null when there are no more */
  next: number | null;
}

/** A page of a folder or the trash (keyset paging) */
export interface CursorPage<T> {
  items: T[];
  /** `after` parameter for the next page; null when there are no more */
  next: string | null;
}

/** A part of a folder asked for by its position, with how many items the folder has */
export interface PositionedPage<T> extends CursorPage<T> {
  total: number;
}

/** Share link access log */
export interface ShareAccess {
  id: number;
  at: number;
  share_id: string;
  owner_name: string | null;
  node_id: string | null;
  node_name: string;
  /** view, unlock, password_fail, preview, download, zip, upload */
  event: string;
  ip: string;
  user_agent: string;
  /** A visit to a link in someone else's personal space, for an administrator: the link and the item aren't named */
  private?: boolean;
}

export interface ShareAccessFilter {
  share_id?: string;
  owner?: string;
  event?: string;
  ip?: string;
  q?: string;
  from?: number;
  to?: number;
}

/** Login log */
export interface LoginRecord {
  id: number;
  at: number;
  /** null when the account doesn't exist */
  user_id: number | null;
  username: string;
  /** login, bad_password, unknown_user, disabled, locked, logout, password_change, sso_denied, sso_provisioned, sso_link, sso_unlink, device_signout, signout_others, admin_signout,
   * app_password_failed, app_password_created, app_password_revoked, 2fa_failed, 2fa_enabled, 2fa_disabled, 2fa_reset,
   * recovery_code_used, recovery_codes_new */
  event: string;
  ip: string;
  user_agent: string;
  /** password, microsoft, google, github */
  method: string;
}

export interface LoginFilter {
  user_id?: number;
  user?: string;
  event?: string;
  ip?: string;
  from?: number;
  to?: number;
}

export interface LogSettings {
  activity_days: number;
  share_days: number;
  login_days: number;
  archive: boolean;
  archive_keep_days: number;
  record_visitor: boolean;
}

export interface LogArchive {
  id: number;
  kind: "activity" | "share_access" | "login_log" | "error_log";
  from_at: number;
  to_at: number;
  rows: number;
  bytes: number;
  created_at: number;
}

export interface LogStatus {
  settings: LogSettings;
  activity: { rows: number; oldest: number | null };
  share_access: { rows: number; oldest: number | null };
  login_log: { rows: number; oldest: number | null };
  error_log: { rows: number; oldest: number | null };
  archives: LogArchive[];
  archive_bytes: number;
  last_run: number | null;
  summary?: string;
}

export interface SharedItem extends Located {
  role: Role;
  sharer: string;
}

export interface Me {
  id: number;
  /** An administrator chose the password: the person must choose their own before anything else */
  must_change_password: boolean;
  username: string;
  /** Shown next to the username; may be blank */
  display_name: string;
  role: "admin" | "user";
  can_write: boolean;
  can_delete: boolean;
  can_share: boolean;
  quota_bytes: number;
  /** Root folder of their personal space ("My files"); null when they don't have one: never ask for "root" then */
  root_id: string | null;
  /** Their personal space waits for its storage location to be available */
  personal_pending: boolean;
  /** Root folder of the "All files" company space; null when disabled */
  shared_root: string | null;
  /** Used in their personal space (0 without one) */
  used_bytes: number;
  can_create_drive: boolean;
  /** Public site URL (used to build share links); blank = use the browser's current URL */
  public_url: string;
  /** Days before trashed items are deleted for good; 0 = kept until the trash is emptied */
  trash_days: number;
  /** Shortest password allowed */
  min_password_length: number;
  /** The administrators' rules for public share links */
  share_policy: SharePolicy;
  /** Earlier versions kept per file; 0 = replacing a file's content keeps no version */
  version_keep: number;
  /** Largest file that can be edited and saved online (bytes) */
  max_edit_bytes: number;
  /** The release the server runs ("dev" for a local build); only people who are signed in are told */
  version: string;
  /** The language saved with the account; "" when they chose none */
  lang: Lang | "";
  /**
   * The language pages use for them in this browser when they or the system default picked it (saved with the account,
   * chosen in this browser, or the system default); null when the browser's languages decide
   */
  ui_lang: Lang | null;
  /** The interface style they chose (lib/style): `auto` follows the operating system of the device in use */
  style: StyleChoice;
}

export interface SharePolicy {
  password_required: boolean;
  /** Links must expire within this many days; 0 = no limit */
  max_days: number;
  /** Off: no new links, and existing ones don't work */
  public_links: boolean;
}

/** A correct password of an account with two-factor sign-in: the ticket for the second step */
export interface TwoFactorPending {
  /** code: ask for a code; setup: the administrator requires it and it isn't set up yet */
  two_factor: "code" | "setup";
  ticket: string;
}

/** What an authenticator app needs to be set up */
export interface TwoFactorSetup {
  /** The secret in base32, for typing it in */
  secret: string;
  /** otpauth:// link */
  uri: string;
  /** QR code of the link (SVG) */
  qr_svg: string;
}

export interface TwoFactorStatus {
  enabled: boolean;
  recovery_codes_left: number;
  /** The administrator requires it for password accounts */
  required: boolean;
  /** Accounts that only sign in with single sign-on have no password to protect */
  has_password: boolean;
}

/** A signed-in device (sign-in session) */
export interface Device {
  id: string;
  user_agent: string;
  /** The address the device used most recently */
  ip: string;
  /** password, microsoft, google or github */
  method: string;
  created_at: number;
  last_used_at: number | null;
  /** The device this page is open on */
  current: boolean;
}

/** An app password (the token itself is only returned once, when it is created) */
export interface AppPassword {
  id: string;
  name: string;
  /** read: downloads and listings only; write: can change files too */
  scope: "read" | "write";
  created_at: number;
  expires_at: number | null;
  last_used_at: number | null;
  last_ip: string;
}

export interface ShareInfo {
  id: string;
  node_id: string;
  has_password: boolean;
  expires_at: number | null;
  max_downloads: number | null;
  downloads: number;
  created_at: number;
  node_name: string;
  node_kind: "folder" | "file";
  /** Number of times the share page was opened */
  views: number;
  /** Time of the last access */
  last_access: number | null;
  /** Who created the link */
  owner_id: number;
  owner_name: string;
  /** The space the item is in */
  drive_id: string | null;
  drive_name: string;
  drive_kind: string;
  /** Owner of a personal space */
  drive_owner: string;
  /** Folder links: visitors may upload files */
  allow_upload: boolean;
  /** Visitors can only upload, not see what is in the folder */
  drop_only: boolean;
  /** false: previews only, no download or ZIP */
  allow_download: boolean;
  /** In someone else's personal space, for an administrator who may only delete it: `id` is a handle for deleting
   * it, not the link's address, and the item isn't named */
  private?: boolean;
}

export interface ShareFilter {
  /** "managed": every link the caller may manage (administrators: all); default: the caller's own */
  scope?: "mine" | "managed";
  drive_id?: string;
  owner_id?: number;
  /** true: only links that expired or used up their downloads; false: only working ones */
  expired?: boolean;
}

/** Changes to a link: a field left out stays as it is; null clears it; password "" removes the password */
export interface ShareUpdate {
  password?: string;
  expires_at?: number | null;
  max_downloads?: number | null;
  allow_upload?: boolean;
  drop_only?: boolean;
  allow_download?: boolean;
}

/** What visitors of a link may do besides viewing */
export interface ShareAccessOptions {
  allow_upload: boolean;
  drop_only: boolean;
  allow_download: boolean;
}

export interface UserRow {
  id: number;
  username: string;
  role: "admin" | "user";
  can_write: boolean;
  can_delete: boolean;
  can_share: boolean;
  quota_bytes: number;
  disabled: boolean;
  created_at: number;
  last_login_at: number | null;
  /** Linked third-party logins (comma-separated) */
  sso: string;
  /** Two-factor sign-in is set up */
  two_factor: boolean;
  /** "password" (created by an administrator) or the provider that created the account automatically */
  source: string;
  /** Email of the most recently used linked sign-in */
  sso_email: string;
  display_name: string;
  used_bytes: number;
  /** The user has a personal space ("My files") */
  personal_space: boolean;
  /** The storage location of their personal space; null without one */
  personal_location: string | null;
  /** The storage location their personal space waits for, when it couldn't be created yet (the location wasn't available) */
  personal_pending: string | null;
  /** The folder their files go into when their personal space is removed and the files are kept ("Files of amy", in the
   * system default language; a number is added when the name is taken) */
  files_folder: string;
}

export interface SystemSettingsReq {
  shared_enabled?: boolean;
  allow_user_drives?: boolean;
  default_user_quota?: number;
  personal_spaces?: boolean;
  /** A storage location's id, or "" for the default location */
  personal_location?: string;
  public_url?: string;
  default_lang?: DefaultLang;
  scan_minutes?: number;
  require_two_factor?: boolean;
  min_password_length?: number;
  share_password_required?: boolean;
  share_max_days?: number;
  public_links?: boolean;
  version_keep?: number;
  version_days?: number;
}

/** System default interface language: "auto" follows the browser */
export type DefaultLang = "auto" | Lang;

export interface SystemInfo {
  shared_enabled: boolean;
  shared_root_id: string;
  allow_user_drives: boolean;
  /** Default personal space quota for new users (bytes, 0 = unlimited) */
  default_user_quota: number;
  /** New users get a personal space ("My files") */
  personal_spaces: boolean;
  /** The storage location of new personal spaces; "" = the default location */
  personal_location: string;
  /** Public site URL; blank = use the browser's current URL */
  public_url: string;
  default_lang: DefaultLang;
  /** Folder spaces are checked for changes this often (minutes, 0 = only by hand) */
  scan_minutes: number;
  /** Password sign-in needs a second factor */
  require_two_factor: boolean;
  min_password_length: number;
  /** Public share links must have a password */
  share_password_required: boolean;
  /** Public share links must expire within this many days (0 = no limit) */
  share_max_days: number;
  /** Public share links can be created and opened */
  public_links: boolean;
  /** Earlier versions kept per file (0 = none), and for how many days (0 = no limit) */
  version_keep: number;
  version_days: number;
  stats: {
    users: number;
    groups: number;
    team_drives: number;
    personal_bytes: number;
    personal_files: number;
    shared_bytes: number;
    shared_files: number;
    team_bytes: number;
    team_files: number;
    trash_bytes: number;
    /** Earlier versions of files (not counted toward the spaces' quotas) */
    version_bytes: number;
    stored_bytes: number;
    share_links: number;
  };
}

export interface PublicShare {
  token: string;
  /** Only once the share is unlocked */
  owner: string | null;
  expires_at: number | null;
  downloads_left: number | null;
  needs_password: boolean;
  /** Visitors may upload files into the folder */
  allow_upload: boolean;
  /** Visitors can only upload: the folder's contents aren't shown */
  drop_only: boolean;
  /** false: previews only */
  allow_download: boolean;
  /** Upload size limit per file in bytes; 0 = none */
  max_upload: number;
  node?: Node;
}

export type SortKey = (typeof SORT_KEYS)[number];

/** An earlier version of a file */
export interface FileVersion {
  id: string;
  size: number;
  /** Who wrote this content */
  author_name: string;
  /** When the file got this content */
  modified_at: number;
  /** When it was replaced */
  created_at: number;
}

/** An item whose name the destination already has (see `api.conflicts`) */
export interface NameConflict {
  /** The item being moved, copied or restored; null for a name about to be uploaded */
  id: string | null;
  name: string;
  kind: "file" | "folder" | null;
  size: number | null;
  updated_at: number | null;
  /** The item with the same name already there */
  existing: Node;
}
export type SortOrder = "asc" | "desc";
