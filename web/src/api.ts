import { download, nativeDownload, type DownloadSource } from "@/downloads";
import { t, tServer } from "@/lib/i18n";
import type { Branding } from "@/lib/branding";
import type { Resolution } from "@/lib/conflicts";
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
  /** Listings of folders only (the navigation pane): whether it has folders in it */
  has_folders?: boolean;
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
  skipped: string[];
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
  blob_count: number;
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
export interface LocationSpace {
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
  space: LocationSpace | null;
  usage: {
    status: "used" | "version" | "trash" | "pending" | "unused";
    space: LocationSpace | null;
    file: string | null;
    uses: number;
  } | null;
}

export interface LocationPage {
  path: string;
  items: LocationItem[];
  next: string | null;
  space: LocationSpace | null;
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

/** Whether a move isn't over: the space is being moved, or waits to be */
export const moveActive = (m: SpaceMove) => m.state === "queued" || m.state === "running" || m.state === "paused" || m.state === "failed";

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
}

/** An entry of an item's history (Details pane): the item itself, or something inside the folder */
export type HistoryEntry = Omit<Activity, "drive_id" | "drive_name">;

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

/** Editable part of the branding settings (the logo is uploaded separately) */
export type BrandingReq = Omit<Branding, "has_logo" | "has_logo_dark" | "has_login_background" | "version">;

export interface SsoProvider {
  id: "microsoft" | "google" | "github" | "oidc";
  /** The name on the button (for oidc, the one the administrator chose) */
  label: string;
}

export type NotificationKind = "shared" | "space_full" | "access_expiring" | "app_password" | "sign_in_method" | "link_upload";

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
  kind: "activity" | "share_access" | "login_log";
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
  /** Accounts that only sign in with Microsoft, Google or GitHub have no password to protect */
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
export type DefaultLang = "auto" | "en" | "zh-TW";

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

export const SORT_KEYS = ["name", "updated", "created", "size", "type"] as const;
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

/**
 * Default space names created by the system (stored in English in the database) are shown in the UI language; spaces named by users are unchanged
 */
export function driveName(d: { kind: string; name: string }) {
  if (d.kind === "personal" && d.name === "My files") return t("My files");
  if (d.kind === "company" && d.name === "All files") return t("All files");
  return d.name;
}

/** Default name of the built-in storage location (stored in English in the database) */
export function locationName(id: string, name: string) {
  return id === "local" && name === "Local disk" ? t("Local disk") : name;
}

/** The server sends the location in English; it is rebuilt from its parts with translated space names */
const localizeLocated = <T extends Located>(n: T): T => ({
  ...n,
  location: [n.location_space ? driveName(n.location_space) : t("Shared with me"), ...(n.location_path ?? [])].join("/"),
});

const localizeDrive = (d: Drive): Drive => ({ ...d, name: driveName(d), location_name: locationName(d.location_id ?? "", d.location_name) });

export class ApiError extends Error {
  constructor(
    message: string,
    public status: number,
    public code?: string,
  ) {
    super(message);
  }
}

async function request<T>(
  method: string,
  path: string,
  body?: unknown,
  raw?: BodyInit,
  extraHeaders?: Record<string, string>,
  signal?: AbortSignal,
): Promise<T> {
  const res = await fetch(`/api${path}`, {
    method,
    signal,
    credentials: "same-origin",
    headers: {
      ...(body !== undefined ? { "Content-Type": "application/json" } : {}),
      ...extraHeaders,
    },
    body: body !== undefined ? JSON.stringify(body) : raw,
  });
  if (!res.ok) throw await responseError(res, `/api${path}`, t("Request failed ({status})", { status: res.status }));
  return res.json() as Promise<T>;
}

/**
 * The error for a failed response: the server's message translated to the UI language, or `fallback`.
 * A 401 without a code means the session expired: the user is sent to sign in (except for sign-in attempts and public share links).
 */
export function errorFromBody(status: number, body: string, url: string, fallback: string): ApiError {
  let message = fallback;
  let code: string | undefined;
  try {
    const data = JSON.parse(body);
    // Server messages are English: translate them to the UI language
    if (data.error) message = tServer(data.error);
    code = data.code;
  } catch {
    // Non-JSON error
  }
  if (status === 401 && code === undefined && !url.startsWith("/api/auth/login") && !url.startsWith("/api/public/")) {
    window.dispatchEvent(new Event("tf:unauthorized"));
  }
  return new ApiError(message, status, code);
}

export async function responseError(res: Response, url: string, fallback: string): Promise<ApiError> {
  return errorFromBody(res.status, await res.text().catch(() => ""), url, fallback);
}

/** `fetch` for file content and other requests made outside `api`, with the same error handling: throws an ApiError when the response isn't OK */
export async function fetchOk(url: string, init?: RequestInit): Promise<Response> {
  const res = await fetch(url, { credentials: "same-origin", ...init });
  if (!res.ok) throw await responseError(res, url, t("Couldn't read the file ({status})", { status: res.status }));
  return res;
}

/** An Office file's content (.docx / .xlsx / .pptx), checked to be an Office Open XML package */
export async function fetchOffice(url: string, signal?: AbortSignal): Promise<ArrayBuffer> {
  return checkOoxml(await (await fetchOk(url, { signal })).arrayBuffer());
}

/** .docx / .xlsx / .pptx are really ZIP archives; check the header first so the preview and editor don't throw a cryptic error or show a blank page */
function checkOoxml(buf: ArrayBuffer) {
  const b = new Uint8Array(buf.slice(0, 4));
  if (b[0] === 0x50 && b[1] === 0x4b && b[2] === 0x03 && b[3] === 0x04) return buf;
  if (b[0] === 0xd0 && b[1] === 0xcf && b[2] === 0x11 && b[3] === 0xe0)
    throw new ApiError(t("This is a legacy Office file (.doc / .xls / .ppt) with a newer file extension, so it can't be opened online. Download it and open it in Office."), 0);
  if (buf.byteLength === 0) throw new ApiError(t("This file is empty."), 0);
  throw new ApiError(t("This file isn't a valid Office document (it may be damaged, or wasn't created by Office), so it can't be opened online. Download it to check."), 0);
}

/** `signal` stops the request when its answer isn't wanted any more (React Query passes one to each query) */
const get = <T>(p: string, signal?: AbortSignal) => request<T>("GET", p, undefined, undefined, undefined, signal);
/** Convert filters to query parameters (skipping empty values) */
const toParams = (o: object) =>
  Object.fromEntries(Object.entries(o).flatMap(([k, v]) => (v === undefined || v === null || v === "" ? [] : [[k, String(v)]]))) as Record<
    string,
    string
  >;
const post = <T>(p: string, body?: unknown) => request<T>("POST", p, body ?? {});
/**
 * API path with every interpolated part encoded as one path segment (or query value): ids and share tokens can come from
 * the address bar, and `/share/abc%3Fx=1` must not turn into `/public/shares/abc?x=1/unlock`. Query strings built with qs()
 * are added after it, unencoded.
 */
export const enc = (strings: TemplateStringsArray, ...parts: (string | number)[]) =>
  strings.reduce((out, s, i) => out + s + (i < parts.length ? encodeURIComponent(String(parts[i])) : ""), "");
const qs = (params: Record<string, string | undefined>) => {
  const s = new URLSearchParams(Object.entries(params).filter((e): e is [string, string] => e[1] !== undefined));
  const str = s.toString();
  return str ? `?${str}` : "";
};

export const api = {
  me: () => get<Me>("/auth/me"),
  login: (username: string, password: string) => post<Me | TwoFactorPending>("/auth/login", { username, password }),
  /** The second step of signing in; after setting it up, the answer carries the recovery codes */
  loginCode: (ticket: string, code: string) => post<Me & { recovery_codes?: string[] }>("/auth/login/2fa", { ticket, code }),
  loginSetup: (ticket: string) => post<TwoFactorSetup>("/auth/login/2fa/setup", { ticket }),
  twoFactor: () => get<TwoFactorStatus>("/auth/2fa"),
  /** Once two-factor sign-in is on, changing it also takes a code from the app or a recovery code */
  startTwoFactor: (password: string, code?: string) => post<TwoFactorSetup>("/auth/2fa/setup", { password, code }),
  enableTwoFactor: (code: string) => post<{ recovery_codes: string[] }>("/auth/2fa/enable", { code }),
  disableTwoFactor: (password: string, code?: string) => post("/auth/2fa/disable", { password, code }),
  newRecoveryCodes: (password: string, code?: string) => post<{ recovery_codes: string[] }>("/auth/2fa/recovery-codes", { password, code }),
  resetTwoFactor: (userId: number) => request("DELETE", `/admin/users/${userId}/2fa`),
  logout: () => post("/auth/logout"),
  changePassword: (current: string, next: string) => request("PUT", "/auth/password", { current, new: next }),
  authOptions: () => get<{ password_reset: boolean }>("/auth/options"),
  forgotPassword: (account: string) => post("/auth/forgot", { account }),
  resetPassword: (token: string, next: string) => post("/auth/reset", { token, new: next }),
  devices: () => get<Device[]>("/auth/sessions"),
  signOutDevice: (id: string) => request("DELETE", `/auth/sessions/${encodeURIComponent(id)}`),
  signOutOtherDevices: () => post<{ removed: number }>("/auth/sessions/others"),
  userDevices: (userId: number) => get<Device[]>(`/admin/users/${userId}/sessions`),
  signOutUserDevice: (userId: number, id: string) => request("DELETE", `/admin/users/${userId}/sessions/${encodeURIComponent(id)}`),
  signOutUserDevices: (userId: number) => request<{ removed: number }>("DELETE", `/admin/users/${userId}/sessions`),
  appPasswords: () => get<AppPassword[]>("/auth/app-passwords"),
  createAppPassword: (req: { name: string; scope: "read" | "write"; expires_days?: number; password?: string; code?: string }) =>
    post<{ token: string; app_password: AppPassword }>("/auth/app-passwords", req),
  deleteAppPassword: (id: string) => request("DELETE", `/auth/app-passwords/${encodeURIComponent(id)}`),

  node: (id: string, signal?: AbortSignal) => get<NodeInfo>(enc`/nodes/${id}`, signal).then((n) => ({ ...n, drive: { ...n.drive, name: driveName(n.drive) } })),
  children: (id: string, sort?: SortKey, order?: SortOrder, foldersOnly?: boolean, signal?: AbortSignal) =>
    get<Node[]>(enc`/nodes/${id}/children` + qs({ sort, order, folders_only: foldersOnly ? "true" : undefined }), signal),
  childrenPage: (id: string, sort: SortKey, order: SortOrder, limit: number, after?: string, signal?: AbortSignal) =>
    get<CursorPage<Node>>(enc`/nodes/${id}/children` + qs({ sort, order, limit: String(limit), after }), signal),
  /** What a typed path names; `aliases` maps names as the UI language shows them to the ones paths use */
  findPath: (path: string, aliases: Record<string, string>) => post<FoundPath>("/nodes/find", { path, aliases }),
  createFolder: (parent_id: string, name: string) => post<Node>("/folders", { parent_id, name }),
  rename: (id: string, name: string) => request<Node>("PATCH", enc`/nodes/${id}`, { name }),
  /** `resolutions`: what to do with each item (by id) whose name the destination already has */
  /** Moving or copying to or from a folder on the server can take a while: follow the task (`waitForJob`) */
  move: (ids: string[], dest_id: string, resolutions?: Record<string, Resolution>) => post<Job>("/nodes/move", { ids, dest_id, resolutions }),
  copy: (ids: string[], dest_id: string, resolutions?: Record<string, Resolution>) => post<Job>("/nodes/copy", { ids, dest_id, resolutions }),
  /** Which names would clash: of `names` about to be uploaded to `dest_id`, of `ids` moved or copied there, or of `ids` restored from the trash (no `dest_id`) */
  conflicts: (req: { dest_id?: string; names?: string[]; ids?: string[] }) => post<NameConflict[]>("/nodes/conflicts", req),
  trash: (ids: string[]) => post("/nodes/trash", { ids }),
  /** The most recent entries about an item (and, for a folder, what's inside it) */
  history: (id: string, signal?: AbortSignal) => get<HistoryEntry[]>(enc`/nodes/${id}/activity`, signal),
  /** Size and number of items inside these folders (files among the ids hold nothing) */
  contents: (ids: string[], signal?: AbortSignal) => request<FolderContents>("POST", "/nodes/contents", { ids }, undefined, undefined, signal),
  /** With mine, only the items the person deleted */
  trashPage: (limit: number, after?: string, mine?: boolean, signal?: AbortSignal) =>
    get<CursorPage<Located>>(`/trash${qs({ limit: String(limit), after, mine: mine ? "true" : undefined })}`, signal).then((p) => ({ ...p, items: p.items.map(localizeLocated) })),
  restore: (ids: string[], resolutions?: Record<string, Resolution>) => post("/trash/restore", { ids, resolutions }),
  deleteForever: (ids: string[]) => post("/trash/delete", { ids }),
  emptyTrash: () => post("/trash/empty"),
  /** What Empty trash would delete: items per space */
  emptyTrashPreview: () => get<{ kind: string; name: string; items: number }[]>("/trash/empty"),
  /** Names containing `q`; at most 300 (`truncated` when there were more) */
  search: (q: string, f: SearchFilter = {}) =>
    get<{ items: Located[]; truncated: boolean }>(`/search${qs(toParams({ q, ...f }))}`).then((r) => ({ ...r, items: r.items.map(localizeLocated) })),
  recent: () => get<Located[]>("/recent").then((l) => l.map(localizeLocated)),
  favorites: (sort?: SortKey, order?: SortOrder) => get<Located[]>(`/favorites${qs({ sort, order })}`).then((l) => l.map(localizeLocated)),
  setFavorite: (ids: string[], favorite: boolean) => post("/nodes/favorite", { ids, favorite }),
  /** Packs items into a new ZIP file in `parentId`, on the server */
  compress: (ids: string[], parentId: string) => post<Job>("/archive/compress", { ids, parent_id: parentId, tz: new Date().getTimezoneOffset() }),
  /** Extracts a ZIP file into a new folder next to it, on the server */
  extract: (id: string) => post<Job>("/archive/extract", { id }),
  job: (id: string) => get<Job>(enc`/jobs/${id}`),
  /** Save from the online editor; with baseVersion (updated_at when the file was opened), returns 409 if someone else changed the file */
  saveContent: (id: string, content: BodyInit, baseVersion?: number) =>
    request<Node>(
      "PUT",
      enc`/files/${id}/content`,
      undefined,
      content,
      baseVersion !== undefined ? { "X-Base-Version": String(baseVersion) } : undefined,
    ),
  /** A file's earlier versions, newest first */
  versions: (id: string, signal?: AbortSignal) => get<FileVersion[]>(enc`/files/${id}/versions`, signal),
  /** Where to open (preview) or download an earlier version */
  versionUrl: (id: string, version: string, download?: boolean) => enc`/api/files/${id}/versions/${version}/content` + (download ? "?download=1" : ""),
  /** Make an earlier version the file's content again (the current content becomes a version too) */
  restoreVersion: (id: string, version: string) => post<Node>(enc`/files/${id}/versions/${version}/restore`),
  /** Create an empty file (a zero-length tus upload completes immediately); returns the new node id */
  createEmptyFile: async (parentId: string, name: string) => {
    const b64 = (s: string) => btoa(String.fromCharCode(...new TextEncoder().encode(s)));
    const res = await fetch("/api/uploads", {
      method: "POST",
      headers: {
        "Tus-Resumable": "1.0.0",
        "Upload-Length": "0",
        "Upload-Metadata": `filename ${b64(name)},parentId ${b64(parentId)}`,
      },
    });
    if (!res.ok) throw await responseError(res, "/api/uploads", t("Couldn't create ({status})", { status: res.status }));
    return res.headers.get("X-Node-Id")!;
  },

  /** With a node: every link on it the caller may manage; otherwise the caller's own links, or those matching the filter */
  shares: (nodeId?: string, filter: ShareFilter = {}, signal?: AbortSignal) => get<ShareInfo[]>(`/shares${qs(toParams({ ...filter, node_id: nodeId }))}`, signal),
  updateShare: (id: string, req: ShareUpdate) => request<ShareInfo>("PATCH", enc`/shares/${id}`, req),
  createShare: (req: { node_id: string; password?: string; expires_at?: number; max_downloads?: number } & Partial<ShareAccessOptions>) =>
    post<ShareInfo>("/shares", req),
  deleteShare: (id: string) => request("DELETE", enc`/shares/${id}`),

  users: () => get<UserRow[]>("/admin/users"),
  /** A page of accounts, by id */
  usersPage: (after: number, limit: number, q = "", signal?: AbortSignal) =>
    get<UserRow[]>(`/admin/users${qs({ after: String(after), limit: String(limit), q: q.trim() || undefined })}`, signal),
  /** `personal_space` / `personal_location`: "My files" and its storage location; left out, the system settings decide */
  createUser: (req: Partial<Omit<UserRow, "personal_space" | "personal_location">> & { password: string; personal_space?: boolean; personal_location?: string }) =>
    post<UserRow>("/admin/users", req),
  updateUser: (id: number, req: Partial<UserRow> & { password?: string }) => request<UserRow>("PATCH", enc`/admin/users/${id}`, req),
  /** Deletes a user: their personal space's files are moved to another space (move_to, a space id) or deleted (delete_files) */
  deleteUser: (id: number, files: { move_to?: string; delete_files?: boolean } = {}) =>
    request<Job>("DELETE", enc`/admin/users/${id}` + qs(toParams(files))),
  /** Gives a user a personal space on a storage location (the system setting's when left out) */
  addPersonalSpace: (id: number, location_id?: string) => post<UserRow>(enc`/admin/users/${id}/personal-space`, { location_id }),
  /** Removes a user's personal space: its files are moved to another space (move_to) or deleted (delete_files) */
  removePersonalSpace: (id: number, files: { move_to?: string; delete_files?: boolean }) =>
    request<Job>("DELETE", enc`/admin/users/${id}/personal-space` + qs(toParams(files))),
  systemSettings: () => get<SystemInfo>("/admin/settings"),
  updateSystemSettings: (req: SystemSettingsReq) => request<SystemInfo>("PATCH", "/admin/settings", req),

  drives: () => get<Drive[]>("/drives").then((l) => l.map(localizeDrive)),
  /** `location_id` (administrators): the storage location of the new space; left out, the default location */
  createDrive: (name: string, quota_bytes?: number, source_path?: string, read_only?: boolean, location_id?: string) =>
    post<Drive>("/drives", { name, quota_bytes, source_path, read_only, location_id }),
  /** Scans a folder space for changes made on the server's folder; the finished task's result is a `ScanReport` */
  scanDrive: (id: string) => post<Job>(enc`/admin/drives/${id}/scan`),
  updateDrive: (id: string, req: { name?: string; quota_bytes?: number; read_only?: boolean }) => request<Drive>("PATCH", enc`/drives/${id}`, req),
  deleteDrive: (id: string) => request("DELETE", enc`/drives/${id}`),
  adminDrives: () => get<Drive[]>("/admin/drives").then((l) => l.map(localizeDrive)),
  access: (nodeId: string) => get<AccessInfo>(enc`/nodes/${nodeId}/access`),
  grant: (
    nodeId: string,
    req: {
      principal_type: PrincipalType;
      principal_id: number;
      role: Role;
      expires_at?: number | null;
    },
  ) => post(enc`/nodes/${nodeId}/access`, req),
  revoke: (grantId: number) => request("DELETE", enc`/grants/${grantId}`),
  directory: (q: string) => get<Principal[]>(`/directory${qs({ q })}`),
  sharedWithMe: () => get<SharedItem[]>("/shared-with-me").then((l) => l.map(localizeLocated)),
  activity: (f: ActivityFilter & { before?: number; limit?: number }) => get<Page<Activity>>(`/activity${qs(toParams(f))}`),
  /** CSV export URL (tz: browser time zone; exported times are shown in local time) */
  activityExportUrl: (f: ActivityFilter) => `/api/activity/export${qs(toParams({ ...f, tz: new Date().getTimezoneOffset() }))}`,
  shareAccess: (f: ShareAccessFilter & { before?: number; limit?: number }) => get<Page<ShareAccess>>(`/share-access${qs(toParams(f))}`),
  loginLog: (f: LoginFilter & { before?: number; limit?: number }) => get<Page<LoginRecord>>(`/login-log${qs(toParams(f))}`),
  loginLogExportUrl: (f: LoginFilter) => `/api/login-log/export${qs(toParams({ ...f, tz: new Date().getTimezoneOffset() }))}`,
  branding: () => get<Branding>("/branding"),
  updateBranding: (b: BrandingReq) => request<Branding>("PUT", "/admin/branding", b),
  uploadLogo: (variant: "light" | "dark", file: File) => request<Branding>("PUT", enc`/admin/branding/logo/${variant}`, undefined, file),
  deleteLogo: (variant: "light" | "dark") => request<Branding>("DELETE", enc`/admin/branding/logo/${variant}`),
  uploadLoginBackground: (file: File) => request<Branding>("PUT", "/admin/branding/background", undefined, file),
  deleteLoginBackground: () => request<Branding>("DELETE", "/admin/branding/background"),
  ssoProviders: () => get<SsoProvider[]>("/auth/sso/providers"),
  /** Start a third-party login (full-page redirect); link = link to the currently signed-in account */
  ssoStartUrl: (provider: string, next: string) => enc`/api/auth/sso/${provider}/start` + qs({ next }),
  /** Linking starts with a request from this page, which returns where to go next */
  /** Linking takes the current password (and a two-factor code); accounts without a password, a recent sign-in */
  ssoLink: (provider: string, next: string, password?: string, code?: string) => post<{ url: string }>(enc`/auth/sso/${provider}/link`, { next, password, code }),
  myIdentities: () => get<{ linked: LinkedIdentity[]; available: string[] }>("/auth/identities"),
  unlinkIdentity: (provider: string) => request("DELETE", enc`/auth/identities/${provider}`),
  /** Also tells the server the time zone, for the times in emails */
  notifications: () => get<{ items: AppNotification[]; unread: number }>(`/notifications${qs({ tz: String(new Date().getTimezoneOffset()) })}`),
  /** Without ids: all of them */
  markNotificationsRead: (ids?: number[]) => post("/notifications/read", { ids }),
  deleteNotification: (id: number) => request("DELETE", enc`/notifications/${id}`),
  clearNotifications: () => request("DELETE", "/notifications"),
  notificationSettings: () => get<NotificationSettings>("/notifications/settings"),
  /** Changing the email address takes the current password (and a two-factor code when it is on) */
  updateNotificationSettings: (req: { email?: string; kinds?: Partial<Record<NotificationKind, NotificationPrefs>>; password?: string; code?: string }) =>
    request<NotificationSettings>("PUT", "/notifications/settings", req),
  emailSettings: () => get<EmailSettings>("/admin/email"),
  updateEmailSettings: (req: EmailSettingsReq) => request<EmailSettings>("PUT", "/admin/email", req),
  testEmail: (req: EmailSettingsReq & { to: string }) => post("/admin/email/test", req),
  ssoSettings: () => get<SsoSettings>("/admin/sso"),
  updateSsoSettings: (s: SsoSettingsReq) => request<SsoSettings>("PUT", "/admin/sso", s),
  logStatus: () => get<LogStatus>("/admin/logs"),
  updateLogSettings: (s: LogSettings) => request<LogStatus>("PUT", "/admin/logs", s),
  archiveLogsNow: () => post<LogStatus>("/admin/logs/archive"),
  logArchiveUrl: (id: number) => enc`/api/admin/logs/archives/${id}`,
  deleteLogArchive: (id: number) => request("DELETE", enc`/admin/logs/archives/${id}`),
  storageLocations: () => get<StorageLocation[]>("/admin/storage").then((l) => l.map((x) => ({ ...x, name: locationName(x.id, x.name) }))),
  createStorage: (req: { name: string; kind: StorageKind; config: StorageConfig }) => post<{ id: string }>("/admin/storage", req),
  updateStorage: (id: string, req: { name?: string; config?: StorageConfig }) => request("PATCH", enc`/admin/storage/${id}`, req),
  deleteStorage: (id: string) => request("DELETE", enc`/admin/storage/${id}`),
  testStorage: (req: { id?: string; kind: StorageKind; config: StorageConfig }) =>
    post<{ ok: boolean; region?: string; host_key?: string }>("/admin/storage/test", req),
  testExistingStorage: (id: string) => post(enc`/admin/storage/${id}/test`),
  testStorageSteps: (id: string) => post<{ ok: boolean; steps: LocationTestStep[] }>(enc`/admin/storage/${id}/test-steps`),
  browseStorage: (id: string, path: string, after?: string) => get<LocationPage>(enc`/admin/storage/${id}/browse` + qs({ path, after })),
  storageDownloadUrl: (id: string, path: string) => enc`/api/admin/storage/${id}/download` + qs({ path }),
  unusedContent: (id: string) => get<UnusedJob | null>(enc`/admin/storage/${id}/unused`),
  findUnusedContent: (id: string) => post<UnusedJob>(enc`/admin/storage/${id}/unused`),
  removeUnusedContent: (id: string, scanId: string) => post<UnusedJob>(enc`/admin/storage/${id}/unused/remove`, { scan_id: scanId }),
  setDefaultStorage: (id: string) => post(enc`/admin/storage/${id}/default`),
  storageLocationSpaces: (id: string) =>
    get<LocationSpace[]>(enc`/admin/storage/${id}/spaces`).then((l) => l.map((s) => ({ ...s, name: driveName(s) }))),
  moves: () =>
    get<MovesList>("/admin/moves").then((l) => ({
      ...l,
      moves: l.moves.map((m) => ({
        ...m,
        space_name: driveName({ kind: m.space_kind, name: m.space_name }),
        from_name: m.from_location ? locationName(m.from_location, m.from_name) : m.from_name,
        to_name: locationName(m.to_location, m.to_name),
      })),
    })),
  startMoves: (drive_ids: string[], location_id: string) => post<{ ids: string[] }>("/admin/moves", { drive_ids, location_id }),
  pauseMove: (id: string) => post(enc`/admin/moves/${id}/pause`),
  resumeMove: (id: string) => post(enc`/admin/moves/${id}/resume`),
  cancelMove: (id: string) => post(enc`/admin/moves/${id}/cancel`),
  setMoveConcurrency: (concurrency: number) => request("PUT", "/admin/moves/settings", { concurrency }),
  groups: () => get<Group[]>("/admin/groups"),
  createGroup: (req: { name: string; description?: string; members?: number[] }) => post<{ id: number }>("/admin/groups", req),
  updateGroup: (id: number, req: { name?: string; description?: string; members?: number[] }) => request("PATCH", enc`/admin/groups/${id}`, req),
  deleteGroup: (id: number) => request("DELETE", enc`/admin/groups/${id}`),

  publicShare: (token: string) => get<PublicShare>(enc`/public/shares/${token}`),
  unlockShare: (token: string, password: string) => post(enc`/public/shares/${token}/unlock`, { password }),
  publicNode: (token: string, id: string) => get<{ node: Node; path: Crumb[] }>(enc`/public/shares/${token}/nodes/${id}`),
  publicChildrenPage: (token: string, id: string, limit: number, after?: string, signal?: AbortSignal) =>
    get<CursorPage<Node>>(enc`/public/shares/${token}/nodes/${id}/children` + qs({ limit: String(limit), after }), signal),
};

/** Source of file content URLs; signed-in files and public shares use the same components */
export interface FileSource {
  contentUrl(n: Node, download?: boolean): string;
  thumbUrl(n: Node): string;
  /** Keeps a thumbnail made in the browser (PDFs, videos) on the server; missing where that isn't possible (share links) */
  saveThumb?(n: Node, image: Blob): Promise<void>;
  /**
   * Link to download the given items from: one item goes in the URL; several are sent to the server, which answers
   * with a short-lived link (a URL holding hundreds of ids is too long for many reverse proxies)
   */
  downloadLink(ids: string[]): Promise<string>;
}

/** Download link for items at `base` (`/download` of the signed-in API or of a share) */
function downloadLink(base: string, ids: string[]): Promise<string> {
  const tz = new Date().getTimezoneOffset();
  if (ids.length === 1) return Promise.resolve(`/api${base}?ids=${encodeURIComponent(ids[0])}&tz=${tz}`);
  return post<{ url: string }>(base, { ids, tz }).then((r) => r.url);
}

export const privateSource: FileSource = {
  contentUrl: (n, download) => enc`/api/files/${n.id}/content` + (download ? "?download=1" : ""),
  thumbUrl: (n) => enc`/api/files/${n.id}/thumbnail?v=${n.updated_at}`,
  saveThumb: async (n, image) => void (await fetchOk(enc`/api/files/${n.id}/thumbnail`, { method: "PUT", body: image, headers: { "Content-Type": image.type } })),
  downloadLink: (ids) => downloadLink("/download", ids),
};

/** Where visitors of a share link that accepts files upload them */
export const shareUploadEndpoint = (token: string) => enc`/api/public/shares/${token}/uploads`;

export function shareSource(token: string): FileSource {
  const base = enc`/api/public/shares/${token}`;
  return {
    contentUrl: (n, download) => base + enc`/nodes/${n.id}/content` + (download ? "?download=1" : ""),
    thumbUrl: (n) => base + enc`/nodes/${n.id}/thumbnail?v=${n.updated_at}`,
    downloadLink: (ids) => downloadLink(enc`/public/shares/${token}/download`, ids),
  };
}

/** Trigger a browser download (without leaving the page) */
/**
 * Downloads: signed-in downloads show progress in the page (bottom right) and are saved when done;
 * public share links are handed to the browser to download directly (handing a large file over to the browser after starting it in the page would count
 * the download twice).
 * A function is called for the URL when the download starts (and again when it is retried)
 */
export function triggerDownload(source: DownloadSource) {
  if (typeof source === "string" && source.startsWith("/api/public/")) return nativeDownload(source);
  return download(source);
}
