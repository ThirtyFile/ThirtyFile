//! The key of every answer the interface keeps (React Query), the queries read in several places, and what a change
//! loads again. A key starts with the name of what was asked; the parts after it narrow it down, and a key's first parts
//! name every answer under it (`keys.children(id)`: all the lists of a folder).
//! Changes to files say what they touched instead, and lib/queries.ts works out the lists they affect.

import { queryOptions, type QueryClient, type QueryKey } from "@tanstack/react-query";
import { api } from "@/api";
import type { ActivityFilter, ErrorFilter, LoginFilter, SearchFilter, ShareAccessFilter, ShareFilter, SortKey, SortOrder } from "@/api/types";

export const keys = {
  // The signed-in person
  me: () => ["me"],
  twoFactor: () => ["two-factor"],
  appPasswords: () => ["app-passwords"],
  identities: () => ["identities"],
  /** Someone's signed-in devices: the person's own ("me"), or an account's (administrators) */
  devices: (user: number | "me") => ["devices", user],
  notifications: () => ["notifications"],
  notificationSettings: () => ["notification-settings"],
  // Before signing in
  branding: () => ["branding"],
  authOptions: () => ["auth-options"],
  ssoProviders: () => ["sso-providers"],

  // Files and folders (see lib/queries.ts for how changes update them)
  node: (id: string | null | undefined) => ["node", id],
  history: (id: string | undefined) => ["node", id, "history"],
  /** Every list of a folder's items: pages, parts loaded by position, its folders */
  children: (id: string | undefined) => ["children", id],
  /** A folder's items page by page, in an order */
  childrenPages: (id: string | undefined, sort: SortKey, order: SortOrder) => ["children", id, sort, order],
  /** A folder's items a part at a time (lib/windows.ts adds where each part starts) */
  childrenAt: (id: string | undefined, sort: SortKey, order: SortOrder) => ["children", id, sort, order, "at"],
  /** The folders in a folder (the navigation pane and folder pickers) */
  folders: (id: string | null | undefined) => ["children", id, "folders"],
  versions: (id: string, version: number) => ["versions", id, version],
  recent: () => ["recent"],
  favorites: (sort: SortKey, order: SortOrder) => ["favorites", sort, order],
  /** The signed-in person's own tags */
  tags: () => ["tags"],
  /** The items with one of their tags */
  tagged: (id: number, sort: SortKey, order: SortOrder) => ["tagged", id, sort, order],
  /** The signed-in person's own smart folders (saved searches) */
  smartFolders: () => ["smart-folders"],
  /** Every list of what a smart folder holds */
  smart: (id: number) => ["smart", id],
  /** What a smart folder holds page by page, in an order */
  smartPages: (id: number, sort: SortKey, order: SortOrder) => ["smart", id, sort, order],
  /** What a smart folder holds a part at a time (lib/windows.ts adds where each part starts) */
  smartAt: (id: number, sort: SortKey, order: SortOrder) => ["smart", id, sort, order, "at"],
  search: (term: string, filter: SearchFilter) => ["search", term, filter],
  sharedWithMe: () => ["shared-with-me"],
  /** Every answer about the trash */
  trash: () => ["trash"],
  trashPages: (deletedBy: string) => ["trash", "pages", deletedBy],
  trashEmpty: () => ["trash", "empty"],
  drives: () => ["drives"],

  // Sharing
  access: (nodeId: string) => ["access", nodeId],
  directory: (q: string) => ["directory", q],
  /** Every answer about share links */
  shares: () => ["shares"],
  /** The links on an item */
  sharesOf: (nodeId: string | undefined) => ["shares", nodeId],
  shareList: (filter: ShareFilter) => ["shares", "list", filter],
  publicShare: (token: string) => ["public", token],
  publicNode: (token: string, id: string) => ["public-node", token, id],
  /** Every list of a share link's folders, or one folder's */
  publicChildren: (token: string, id?: string) => (id === undefined ? ["public-children", token] : ["public-children", token, id]),

  // Administration
  system: () => ["system"],
  adminUsers: () => ["admin-users"],
  adminUserPages: (search: string) => ["admin-users", "pages", search],
  adminDrives: () => ["admin-drives"],
  groups: () => ["groups"],
  storageLocations: () => ["storage-locations"],
  storageLocationSpaces: (id: string) => ["storage-location-spaces", id],
  /** Every page browsed of a location, or one */
  storageBrowse: (id: string, path?: string, after?: string) => (path === undefined ? ["storage-browse", id] : ["storage-browse", id, path, after]),
  storageUnused: (id: string) => ["storage-unused", id],
  usage: () => ["usage"],
  usageHistory: (location: string, range: string, work: string) => ["usage-history", location, range, work],
  moves: () => ["moves"],
  backups: () => ["backups"],
  copyPreview: (source: string, dest: string) => ["copy-preview", source, dest],
  backupNextRuns: (schedule: string, tz: string) => ["backup-next-runs", schedule, tz],
  snapshotBrowse: (snapshot: string, space: string, folder: string | null | undefined) => ["snapshot-browse", snapshot, space, folder],
  restorePreview: (snapshot: string, request: string) => ["restore-preview", snapshot, request],
  replicas: () => ["replicas"],
  promotePreflight: (id: string, target: string) => ["promote-preflight", id, target],
  emailSettings: () => ["email-settings"],
  ssoSettings: () => ["sso-settings"],
  logStatus: () => ["log-status"],
  activity: (filter?: ActivityFilter) => (filter === undefined ? ["activity"] : ["activity", filter]),
  loginLog: (filter?: LoginFilter) => (filter === undefined ? ["login-log"] : ["login-log", filter]),
  shareAccess: (filter?: ShareAccessFilter) => (filter === undefined ? ["share-access"] : ["share-access", filter]),
  errors: (filter?: ErrorFilter) => (filter === undefined ? ["errors"] : ["errors", filter]),
} satisfies Record<string, (...args: never[]) => QueryKey>;

/** Queries read in several places, with the same key, request and options everywhere */
export const queries = {
  /** The signed-in person: a 401 means signing in again, not trying again */
  me: queryOptions({ queryKey: keys.me(), queryFn: api.me, retry: false, staleTime: 30_000 }),
  twoFactor: queryOptions({ queryKey: keys.twoFactor(), queryFn: api.twoFactor }),
  sharedWithMe: queryOptions({ queryKey: keys.sharedWithMe(), queryFn: api.sharedWithMe }),
  /** Read by every row that shows tag dots: changed only by the person, so kept until a change says otherwise */
  tags: queryOptions({ queryKey: keys.tags(), queryFn: api.tags, staleTime: 5 * 60_000 }),
  /** The navigation pane's smart folders: changed only by the person, so kept until a change says otherwise */
  smartFolders: queryOptions({ queryKey: keys.smartFolders(), queryFn: api.smartFolders, staleTime: 5 * 60_000 }),
  system: queryOptions({ queryKey: keys.system(), queryFn: api.systemSettings }),
  adminUsers: queryOptions({ queryKey: keys.adminUsers(), queryFn: api.users }),
  adminDrives: queryOptions({ queryKey: keys.adminDrives(), queryFn: api.adminDrives }),
  groups: queryOptions({ queryKey: keys.groups(), queryFn: api.groups }),
  storageLocations: queryOptions({ queryKey: keys.storageLocations(), queryFn: api.storageLocations }),
  storageLocationSpaces: (id: string) => queryOptions({ queryKey: keys.storageLocationSpaces(id), queryFn: () => api.storageLocationSpaces(id) }),
  moves: queryOptions({ queryKey: keys.moves(), queryFn: api.moves }),
};

/** What a change loads again when it reaches past its own list */
export const affected = {
  /** Access given to the signed-in person changed: the spaces they see, and "Shared with me" */
  myAccess: () => [keys.drives(), keys.sharedWithMe()],
  /** A storage location changed: the locations, and the spaces on them */
  storage: () => [keys.storageLocations(), keys.adminDrives()],
  /** A space changed (administrators): their list of spaces, and the spaces everyone sees */
  space: () => [keys.adminDrives(), keys.drives()],
  /** An account created or changed: the accounts, and the administrator's own account (it may be theirs) */
  account: () => [keys.adminUsers(), keys.me()],
  /** An account deleted: the accounts, and the spaces (its "My files" went with it) */
  accountDeleted: () => [keys.adminUsers(), keys.adminDrives()],
  /**
   * Someone's "My files" created or removed: the accounts and spaces, and the administrator's own account and spaces
   * (the navigation pane), in case it was theirs
   */
  personalSpace: () => [keys.adminUsers(), keys.adminDrives(), keys.me(), keys.drives()],
  /** Logs moved to archives: the activity and share link logs shown */
  logsArchived: () => [keys.activity(), keys.shareAccess()],
};

/** Marks the answers under these keys out of date: the ones shown load again, the others when they next show */
export function invalidate(qc: QueryClient, ...keys: QueryKey[]): Promise<void> {
  return Promise.all(keys.map((queryKey) => qc.invalidateQueries({ queryKey }))).then(() => undefined);
}
