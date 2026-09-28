/**
 * Where a person starts. Most people have a personal space, "My files" (the alias "root"); an administrator may
 * give someone none, and then nothing may offer it or ask the server for "root" (it answers 404).
 */
import type { Drive, Me } from "@/api";

/** Whether the person has "My files" */
export const hasPersonal = (me: Pick<Me, "root_id">) => !!me.root_id;

/**
 * The folder to open when no other is given (`/files`, the start of the folder picker): "root" for "My files",
 * else "shared" for the company space "All files", else the first other space's root folder. null when the person
 * has no space at all; undefined while their spaces are still loading.
 */
export function homeFolder(me: Pick<Me, "root_id" | "shared_root">, drives: readonly Pick<Drive, "kind" | "root_id" | "disabled">[] | undefined): string | null | undefined {
  if (me.root_id) return "root";
  if (me.shared_root) return "shared";
  if (!drives) return undefined;
  return drives.find((d) => d.kind !== "personal" && !d.disabled)?.root_id ?? null;
}

/**
 * The folder a page shows, from its path: `/files` (the person's home: "My files" when they have it), `/files/shared`
 * (All files) or `/files/<id>`. null for other pages, and for "My files" when the person has none.
 */
export function folderOfPath(path: string | undefined, personal = true): string | null {
  const p = path?.split(/[?#]/)[0];
  if (p === "/files") return personal ? "root" : null;
  const m = p && /^\/files\/([^/]+)$/.exec(p);
  const id = m ? decodeURIComponent(m[1]) : null;
  return id === "root" && !personal ? null : id;
}
