/** What a ZIP file holds, as folders: its entries name whole paths, and a folder may be implied by them alone */
import type { ArchiveEntry } from "@/api";

/** A file or folder of the archive, in the folder it is in */
export interface ArchiveItem {
  /** Its path inside the archive ("/" between folders) */
  path: string;
  name: string;
  dir: boolean;
  size: number;
  modified: number | null;
  /** A file's number in the archive, to download it by */
  index: number | null;
  locked: ArchiveEntry["locked"];
}

const names = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

/** What each folder holds (the top one is ""), folders first, then by name. Two entries of one path are both there */
export function archiveFolders(entries: readonly ArchiveEntry[]): Map<string, ArchiveItem[]> {
  const folders = new Map<string, ArchiveItem[]>([["", []]]);
  /** The folder at `path`, made (with those around it) when it isn't there yet */
  const folder = (path: string): ArchiveItem[] => {
    const known = folders.get(path);
    if (known) return known;
    const slash = path.lastIndexOf("/");
    const inside: ArchiveItem[] = [];
    folders.set(path, inside);
    folder(path.slice(0, Math.max(0, slash))).push({ path, name: path.slice(slash + 1), dir: true, size: 0, modified: null, index: null, locked: null });
    return inside;
  };
  /** The folders' own entries, which give them their times */
  const times = new Map<string, number | null>();
  for (const e of entries) {
    if (e.dir) {
      folder(e.path);
      times.set(e.path, e.modified);
      continue;
    }
    const slash = e.path.lastIndexOf("/");
    folder(e.path.slice(0, Math.max(0, slash))).push({ path: e.path, name: e.path.slice(slash + 1), dir: false, size: e.size, modified: e.modified, index: e.index, locked: e.locked });
  }
  for (const items of folders.values()) {
    for (const item of items) if (item.dir) item.modified = times.get(item.path) ?? null;
    items.sort((a, b) => Number(b.dir) - Number(a.dir) || names.compare(a.name, b.name));
  }
  return folders;
}
