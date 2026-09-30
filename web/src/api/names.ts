//! Names the server stores in English, shown in the interface's language

import { t } from "@/lib/i18n";
import type { Located, Drive } from "@/api/types";

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
export const localizeLocated = <T extends Located>(n: T): T => ({
  ...n,
  location: [n.location_space ? driveName(n.location_space) : t("Shared with me"), ...(n.location_path ?? [])].join("/"),
});

export const localizeDrive = (d: Drive): Drive => ({ ...d, name: driveName(d), location_name: locationName(d.location_id ?? "", d.location_name) });
