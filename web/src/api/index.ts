//! The server's HTTP API: `api.<endpoint>()`, the types it answers with, and the helpers for requests made outside it.
//! - client.ts: requests and errors; types.ts: what the server sends and takes; names.ts: names shown in the interface
//! - one module of endpoints per area, merged into `api` here; sources.ts: where file content is read from

import { authApi } from "@/api/auth";
import { filesApi } from "@/api/files";
import { sharingApi } from "@/api/sharing";
import { usersApi } from "@/api/users";
import { drivesApi } from "@/api/drives";
import { settingsApi } from "@/api/settings";
import { logsApi } from "@/api/logs";
import { notificationsApi } from "@/api/notifications";
import { storageApi } from "@/api/storage";
import { backupsApi } from "@/api/backups";
import { replicasApi } from "@/api/replicas";

export * from "@/api/types";
export { SORT_KEYS } from "@/api/files";
export { moveActive } from "@/api/storage";
export { backupJobActive } from "@/api/backups";
export { ApiError, enc, errorFromBody, fetchOffice, fetchOk, responseError } from "@/api/client";
export { driveName, locationName } from "@/api/names";
export { privateSource, shareSource, shareUploadEndpoint, type FileSource } from "@/api/sources";

export const api = {
  ...authApi,
  ...filesApi,
  ...sharingApi,
  ...usersApi,
  ...drivesApi,
  ...settingsApi,
  ...logsApi,
  ...notificationsApi,
  ...storageApi,
  ...backupsApi,
  ...replicasApi,
};
