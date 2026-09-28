import { driveName, type FoundPath, type NodeInfo } from "@/api";
import { t } from "@/lib/i18n";

/**
 * Paths in the address bar name items the way WebDAV does (see the server's paths.rs): `/My files/Reports`, with the
 * top level being the user's spaces ("My files" is their personal space) and "Shared with me". The names the system
 * gives ("My files", "All files", "Shared with me") are stored in English and shown in the UI language.
 */
const BUILT_IN = ["My files", "All files", "Shared with me"];

/** The path of a folder or file as shown in the address bar, e.g. "/My files/Reports"; undefined when no path reaches it */
export function pathOf(info: Pick<NodeInfo, "location" | "via_share" | "drive"> | undefined) {
  if (!info?.location?.length) return undefined;
  const [top, ...rest] = info.location;
  const shown = info.via_share ? (top === "Shared with me" ? t("Shared with me") : top) : driveName({ kind: info.drive.kind, name: top });
  return "/" + [shown, ...rest].join("/");
}

/** How the UI language shows the system's names → the names paths use, for the server to read a typed path with */
export function pathAliases() {
  const out: Record<string, string> = {};
  for (const name of BUILT_IN) if (t(name) !== name) out[t(name)] = name;
  return out;
}

/** The page a found path opens */
export function urlOf(found: FoundPath) {
  if (found.place === "spaces") return "/drives";
  if (found.place === "shared") return "/shared-with-me";
  return found.place === "folder" ? `/files/${found.id}` : `/view/${found.id}`;
}

/**
 * A link to a page of this site pasted into the address bar (e.g. a folder's address copied from another tab): the
 * page to go to. WebDAV addresses (`/dav/…`) aren't pages, the server reads them as paths
 */
export function appLink(text: string, origin: string) {
  let url: URL;
  try {
    url = new URL(text.trim());
  } catch {
    return null;
  }
  if (url.origin !== origin || url.pathname === "/dav" || url.pathname.startsWith("/dav/")) return null;
  return url.pathname + url.search + url.hash;
}
