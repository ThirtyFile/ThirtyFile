import { download, nativeDownload } from "@/downloads";
import { t, tServer } from "@/lib/i18n";
import type { Branding } from "@/lib/branding";
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
}

export interface Located extends Node {
  location: string;
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
  /** Why the storage service holding the content is offline (e.g. S3 disconnected) */
  offline: string | null;
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
  /** Storage location for new files */
  location_id: string;
  location_name: string;
  location_is_default: boolean;
  /** Why the storage service is offline: can be browsed, but not opened, downloaded or uploaded to */
  offline: string | null;
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
  used_bytes: number;
  blob_count: number;
  drive_count: number;
}

export interface Migration {
  drive_id: string;
  target: string;
  total_files: number;
  total_bytes: number;
  done_files: number;
  done_bytes: number;
  running: boolean;
  error: string | null;
  started_at: number;
  finished_at: number | null;
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
}

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
  id: "microsoft" | "google" | "github";
  label: string;
}

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
}

export interface SsoSettings {
  microsoft: SsoProviderSettings;
  google: SsoProviderSettings;
  github: SsoProviderSettings;
  allowed_domains: string[];
  domain_rules: SsoDomainRule[];
  /** Accounts one provider may create per hour */
  max_created_per_hour: number;
  /** Site URL is configured (only then is the redirect URI the real production URL) */
  public_url_set: boolean;
}

export type SsoProviderReq = SsoProviderPolicy & { enabled: boolean; client_id: string; client_secret: string; tenant: string };
export interface SsoSettingsReq {
  microsoft: SsoProviderReq;
  google: SsoProviderReq;
  github: SsoProviderReq;
  allowed_domains: string[];
  domain_rules: SsoDomainRule[];
}

export interface Page<T> {
  items: T[];
  /** `before` parameter for the next page; null when there are no more */
  next: number | null;
}

/** Share link access log */
export interface ShareAccess {
  id: number;
  at: number;
  share_id: string;
  owner_name: string | null;
  node_id: string | null;
  node_name: string;
  /** view, unlock, password_fail, preview, download, zip */
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
  /** login, bad_password, unknown_user, disabled, locked, logout, password_change, sso_denied, sso_provisioned, sso_link, sso_unlink */
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
  username: string;
  /** Shown next to the username; may be blank */
  display_name: string;
  role: "admin" | "user";
  can_write: boolean;
  can_delete: boolean;
  can_share: boolean;
  quota_bytes: number;
  root_id: string;
  /** Root folder of the "All files" company space; null when disabled */
  shared_root: string | null;
  used_bytes: number;
  can_create_drive: boolean;
  /** Public site URL (used to build share links); blank = use the browser's current URL */
  public_url: string;
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
  /** "password" (created by an administrator) or the provider that created the account automatically */
  source: string;
  /** Email of the most recently used linked sign-in */
  sso_email: string;
  display_name: string;
  used_bytes: number;
}

export interface SystemSettingsReq {
  shared_enabled?: boolean;
  allow_user_drives?: boolean;
  default_user_quota?: number;
  public_url?: string;
  default_lang?: DefaultLang;
}

/** System default interface language: "auto" follows the browser */
export type DefaultLang = "auto" | "en" | "zh-TW";

export interface SystemInfo {
  shared_enabled: boolean;
  shared_root_id: string;
  allow_user_drives: boolean;
  /** Default personal space quota for new users (bytes, 0 = unlimited) */
  default_user_quota: number;
  /** Public site URL; blank = use the browser's current URL */
  public_url: string;
  default_lang: DefaultLang;
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
  node?: Node;
}

export type SortKey = "name" | "updated" | "size" | "type";
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

const localizeDrive = (d: Drive): Drive => ({ ...d, name: driveName(d), location_name: locationName(d.location_id, d.location_name) });

export class ApiError extends Error {
  constructor(
    message: string,
    public status: number,
    public code?: string,
  ) {
    super(message);
  }
}

async function request<T>(method: string, path: string, body?: unknown, raw?: BodyInit, extraHeaders?: Record<string, string>): Promise<T> {
  const res = await fetch(`/api${path}`, {
    method,
    credentials: "same-origin",
    headers: {
      ...(body !== undefined ? { "Content-Type": "application/json" } : {}),
      ...extraHeaders,
    },
    body: body !== undefined ? JSON.stringify(body) : raw,
  });
  if (!res.ok) {
    let message = t("Request failed ({status})", { status: res.status });
    let code: string | undefined;
    try {
      const data = await res.json();
      // Server messages are English: translate them to the UI language
      message = data.error ? tServer(data.error) : message;
      code = data.code;
    } catch {
      // Non-JSON error
    }
    if (res.status === 401 && code === undefined && !path.startsWith("/auth/login") && !path.startsWith("/public/")) {
      window.dispatchEvent(new Event("tf:unauthorized"));
    }
    throw new ApiError(message, res.status, code);
  }
  return res.json() as Promise<T>;
}

const get = <T>(p: string) => request<T>("GET", p);
/** Convert filters to query parameters (skipping empty values) */
const toParams = (o: object) =>
  Object.fromEntries(Object.entries(o).flatMap(([k, v]) => (v === undefined || v === null || v === "" ? [] : [[k, String(v)]]))) as Record<
    string,
    string
  >;
const post = <T>(p: string, body?: unknown) => request<T>("POST", p, body ?? {});
const qs = (params: Record<string, string | undefined>) => {
  const s = new URLSearchParams(Object.entries(params).filter((e): e is [string, string] => e[1] !== undefined));
  const str = s.toString();
  return str ? `?${str}` : "";
};

export const api = {
  me: () => get<Me>("/auth/me"),
  login: (username: string, password: string) => post<Me>("/auth/login", { username, password }),
  logout: () => post("/auth/logout"),
  changePassword: (current: string, next: string) => request("PUT", "/auth/password", { current, new: next }),

  node: (id: string) => get<NodeInfo>(`/nodes/${id}`).then((n) => ({ ...n, drive: { ...n.drive, name: driveName(n.drive) } })),
  children: (id: string, sort?: SortKey, order?: SortOrder, foldersOnly?: boolean) =>
    get<Node[]>(`/nodes/${id}/children${qs({ sort, order, folders_only: foldersOnly ? "true" : undefined })}`),
  createFolder: (parent_id: string, name: string) => post<Node>("/folders", { parent_id, name }),
  rename: (id: string, name: string) => request<Node>("PATCH", `/nodes/${id}`, { name }),
  move: (ids: string[], dest_id: string) => post("/nodes/move", { ids, dest_id }),
  copy: (ids: string[], dest_id: string) => post("/nodes/copy", { ids, dest_id }),
  trash: (ids: string[]) => post("/nodes/trash", { ids }),
  listTrash: () => get<Located[]>("/trash"),
  restore: (ids: string[]) => post("/trash/restore", { ids }),
  deleteForever: (ids: string[]) => post("/trash/delete", { ids }),
  emptyTrash: () => post("/trash/empty"),
  search: (q: string) => get<Located[]>(`/search${qs({ q })}`),
  recent: () => get<Located[]>("/recent"),
  favorites: (sort?: SortKey, order?: SortOrder) => get<Located[]>(`/favorites${qs({ sort, order })}`),
  setFavorite: (ids: string[], favorite: boolean) => post("/nodes/favorite", { ids, favorite }),
  /** Save from the online editor; with baseVersion (updated_at when the file was opened), returns 409 if someone else changed the file */
  saveContent: (id: string, content: BodyInit, baseVersion?: number) =>
    request<Node>(
      "PUT",
      `/files/${id}/content`,
      undefined,
      content,
      baseVersion !== undefined ? { "X-Base-Version": String(baseVersion) } : undefined,
    ),
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
    if (!res.ok) {
      const data = await res.json().catch(() => ({}));
      throw new ApiError(data.error ? tServer(data.error) : t("Couldn't create ({status})", { status: res.status }), res.status);
    }
    return res.headers.get("X-Node-Id")!;
  },

  shares: (nodeId?: string) => get<ShareInfo[]>(`/shares${qs({ node_id: nodeId })}`),
  createShare: (req: { node_id: string; password?: string; expires_at?: number; max_downloads?: number }) => post<ShareInfo>("/shares", req),
  deleteShare: (id: string) => request("DELETE", `/shares/${id}`),

  users: () => get<UserRow[]>("/admin/users"),
  createUser: (req: Partial<UserRow> & { password: string }) => post<UserRow>("/admin/users", req),
  updateUser: (id: number, req: Partial<UserRow> & { password?: string }) => request<UserRow>("PATCH", `/admin/users/${id}`, req),
  deleteUser: (id: number) => request("DELETE", `/admin/users/${id}`),
  systemSettings: () => get<SystemInfo>("/admin/settings"),
  updateSystemSettings: (req: SystemSettingsReq) => request<SystemInfo>("PATCH", "/admin/settings", req),

  drives: () => get<Drive[]>("/drives").then((l) => l.map(localizeDrive)),
  createDrive: (name: string, quota_bytes?: number) => post<Drive>("/drives", { name, quota_bytes }),
  updateDrive: (id: string, req: { name?: string; quota_bytes?: number }) => request<Drive>("PATCH", `/drives/${id}`, req),
  deleteDrive: (id: string) => request("DELETE", `/drives/${id}`),
  adminDrives: () => get<Drive[]>("/admin/drives").then((l) => l.map(localizeDrive)),
  access: (nodeId: string) => get<AccessInfo>(`/nodes/${nodeId}/access`),
  grant: (
    nodeId: string,
    req: {
      principal_type: PrincipalType;
      principal_id: number;
      role: Role;
      expires_at?: number | null;
    },
  ) => post(`/nodes/${nodeId}/access`, req),
  revoke: (grantId: number) => request("DELETE", `/grants/${grantId}`),
  directory: (q: string) => get<Principal[]>(`/directory${qs({ q })}`),
  sharedWithMe: () => get<SharedItem[]>("/shared-with-me"),
  activity: (f: ActivityFilter & { before?: number; limit?: number }) => get<Page<Activity>>(`/activity${qs(toParams(f))}`),
  /** CSV export URL (tz: browser time zone; exported times are shown in local time) */
  activityExportUrl: (f: ActivityFilter) => `/api/activity/export${qs(toParams({ ...f, tz: new Date().getTimezoneOffset() }))}`,
  shareAccess: (f: ShareAccessFilter & { before?: number; limit?: number }) => get<Page<ShareAccess>>(`/share-access${qs(toParams(f))}`),
  loginLog: (f: LoginFilter & { before?: number; limit?: number }) => get<Page<LoginRecord>>(`/login-log${qs(toParams(f))}`),
  loginLogExportUrl: (f: LoginFilter) => `/api/login-log/export${qs(toParams({ ...f, tz: new Date().getTimezoneOffset() }))}`,
  branding: () => get<Branding>("/branding"),
  updateBranding: (b: BrandingReq) => request<Branding>("PUT", "/admin/branding", b),
  uploadLogo: (variant: "light" | "dark", file: File) => request<Branding>("PUT", `/admin/branding/logo/${variant}`, undefined, file),
  deleteLogo: (variant: "light" | "dark") => request<Branding>("DELETE", `/admin/branding/logo/${variant}`),
  uploadLoginBackground: (file: File) => request<Branding>("PUT", "/admin/branding/background", undefined, file),
  deleteLoginBackground: () => request<Branding>("DELETE", "/admin/branding/background"),
  ssoProviders: () => get<SsoProvider[]>("/auth/sso/providers"),
  /** Start a third-party login (full-page redirect); link = link to the currently signed-in account */
  ssoStartUrl: (provider: string, next: string) => `/api/auth/sso/${encodeURIComponent(provider)}/start${qs({ next })}`,
  /** Linking starts with a request from this page, which returns where to go next */
  ssoLink: (provider: string, next: string) => post<{ url: string }>(`/auth/sso/${encodeURIComponent(provider)}/link`, { next }),
  myIdentities: () => get<{ linked: LinkedIdentity[]; available: string[] }>("/auth/identities"),
  unlinkIdentity: (provider: string) => request("DELETE", `/auth/identities/${provider}`),
  ssoSettings: () => get<SsoSettings>("/admin/sso"),
  updateSsoSettings: (s: SsoSettingsReq) => request<SsoSettings>("PUT", "/admin/sso", s),
  logStatus: () => get<LogStatus>("/admin/logs"),
  updateLogSettings: (s: LogSettings) => request<LogStatus>("PUT", "/admin/logs", s),
  archiveLogsNow: () => post<LogStatus>("/admin/logs/archive"),
  logArchiveUrl: (id: number) => `/api/admin/logs/archives/${id}`,
  deleteLogArchive: (id: number) => request("DELETE", `/admin/logs/archives/${id}`),
  storageLocations: () => get<StorageLocation[]>("/admin/storage").then((l) => l.map((x) => ({ ...x, name: locationName(x.id, x.name) }))),
  createStorage: (req: { name: string; kind: StorageKind; config: StorageConfig }) => post<{ id: string }>("/admin/storage", req),
  updateStorage: (id: string, req: { name?: string; config?: StorageConfig }) => request("PATCH", `/admin/storage/${id}`, req),
  deleteStorage: (id: string) => request("DELETE", `/admin/storage/${id}`),
  testStorage: (req: { id?: string; kind: StorageKind; config: StorageConfig }) =>
    post<{ ok: boolean; region?: string; host_key?: string }>("/admin/storage/test", req),
  testExistingStorage: (id: string) => post(`/admin/storage/${id}/test`),
  setDefaultStorage: (id: string) => post(`/admin/storage/${id}/default`),
  setDriveLocation: (driveId: string, location_id: string | null, migrate: boolean) =>
    request("PUT", `/admin/drives/${driveId}/location`, {
      location_id,
      migrate,
    }),
  migrateDrive: (driveId: string) => post(`/admin/drives/${driveId}/migrate`),
  migrations: () => get<Migration[]>("/admin/migrations"),
  groups: () => get<Group[]>("/admin/groups"),
  createGroup: (req: { name: string; description?: string; members?: number[] }) => post<{ id: number }>("/admin/groups", req),
  updateGroup: (id: number, req: { name?: string; description?: string; members?: number[] }) => request("PATCH", `/admin/groups/${id}`, req),
  deleteGroup: (id: number) => request("DELETE", `/admin/groups/${id}`),

  publicShare: (token: string) => get<PublicShare>(`/public/shares/${token}`),
  unlockShare: (token: string, password: string) => post(`/public/shares/${token}/unlock`, { password }),
  publicNode: (token: string, id: string) => get<{ node: Node; path: Crumb[] }>(`/public/shares/${token}/nodes/${id}`),
  publicChildren: (token: string, id: string, sort?: SortKey, order?: SortOrder) =>
    get<Node[]>(`/public/shares/${token}/nodes/${id}/children${qs({ sort, order })}`),
};

/** Source of file content URLs; signed-in files and public shares use the same components */
export interface FileSource {
  contentUrl(n: Node, download?: boolean): string;
  thumbUrl(n: Node): string;
  downloadUrl(ids: string[]): string;
}

export const privateSource: FileSource = {
  contentUrl: (n, download) => `/api/files/${n.id}/content${download ? "?download=1" : ""}`,
  thumbUrl: (n) => `/api/files/${n.id}/thumbnail?v=${n.updated_at}`,
  downloadUrl: (ids) => `/api/download?ids=${ids.join(",")}`,
};

export function shareSource(token: string): FileSource {
  const base = `/api/public/shares/${token}`;
  return {
    contentUrl: (n, download) => `${base}/nodes/${n.id}/content${download ? "?download=1" : ""}`,
    thumbUrl: (n) => `${base}/nodes/${n.id}/thumbnail?v=${n.updated_at}`,
    downloadUrl: (ids) => `${base}/download?ids=${ids.join(",")}`,
  };
}

/** Trigger a browser download (without leaving the page) */
/**
 * Downloads: signed-in downloads show progress in the page (bottom right) and are saved when done;
 * public share links are handed to the browser to download directly (a pre-check would count an extra download)
 */
export function triggerDownload(url: string) {
  if (url.startsWith("/api/public/")) return nativeDownload(url);
  return download(url);
}
