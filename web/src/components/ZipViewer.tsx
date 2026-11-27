/** A ZIP file opened: what it holds, folder by folder, without extracting it; one file or a selection can be taken out */
import { Fragment, useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronRightIcon, DownloadIcon, FolderOutputIcon, Loader2Icon } from "lucide-react";
import { api, type Node } from "@/api";
import { keys } from "@/api/queryKeys";
import { FileIcon } from "@/components/FileIcon";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { triggerDownload } from "@/downloads";
import { type ArchiveItem, archiveFolders } from "@/lib/archive";
import { capsOf } from "@/lib/drives";
import { reportShown } from "@/lib/errorReport";
import { t } from "@/lib/i18n";
import { runJob } from "@/lib/jobs";
import { useMe } from "@/lib/session";
import { errorMessage, formatBytes, formatDateTime } from "@/lib/utils";

/** Rows shown at first, and added by "Show more" */
const ROWS = 500;

export default function ZipViewer({ node }: { node: Node }) {
  const me = useMe();
  const qc = useQueryClient();
  const listing = useQuery({
    queryKey: keys.archive(node.id, node.updated_at),
    queryFn: ({ signal }) =>
      api.archiveEntries(node.id, signal).catch((e) => {
        reportShown("preview", e, node.id);
        throw e;
      }),
    retry: false,
  });
  // Extracting adds a folder next to the archive
  const parent = useQuery({ queryKey: keys.node(node.parent_id ?? ""), queryFn: () => api.node(node.parent_id!), enabled: !!node.parent_id, retry: false });
  const canExtract = !!parent.data && !parent.data.offline && capsOf(parent.data.role, me, parent.data.read_only).add;

  const folders = useMemo(() => archiveFolders(listing.data?.entries ?? []), [listing.data]);
  const [at, setAt] = useState("");
  const [picked, setPicked] = useState<ReadonlySet<string>>(new Set());
  const [limit, setLimit] = useState(ROWS);
  // A folder that is gone with a newer content: the top one
  const folder = folders.has(at) ? at : "";
  const items = folders.get(folder) ?? [];
  const chosen = items.filter((n) => picked.has(n.path) && !n.locked);
  const choosable = items.filter((n) => !n.locked);
  const open = (path: string) => {
    setAt(path);
    setPicked(new Set());
    setLimit(ROWS);
  };
  const toggle = (path: string, on: boolean) => {
    const next = new Set(picked);
    if (on) next.add(path);
    else next.delete(path);
    setPicked(next);
  };
  const extract = (paths?: string[]) =>
    void runJob(qc, () => api.extract(node.id, paths && { paths, version: listing.data!.version }), { folders: [node.parent_id], contents: true, usage: true });
  const download = (item: ArchiveItem) => triggerDownload(api.archiveEntryUrl(node.id, item.index!, listing.data!.version));

  const parts = folder ? folder.split("/") : [];
  return (
    <div className="flex size-full min-h-0 flex-col bg-background text-left text-foreground">
      <div className="flex shrink-0 flex-wrap items-center gap-2 border-b px-3 py-2">
        <nav aria-label={t("Folders in the ZIP file")} className="flex min-w-0 flex-1 flex-wrap items-center gap-0.5 text-sm">
          {[node.name, ...parts].map((name, i) => (
            <Fragment key={i}>
              {i > 0 && <ChevronRightIcon aria-hidden className="size-3.5 text-muted-foreground" />}
              {i === parts.length ? (
                <span aria-current="location" className="min-w-0 truncate px-1 font-medium">
                  {name}
                </span>
              ) : (
                <button type="button" className="min-w-0 truncate rounded px-1 hover:bg-muted" onClick={() => open(parts.slice(0, i).join("/"))}>
                  {name}
                </button>
              )}
            </Fragment>
          ))}
        </nav>
        {canExtract && listing.data && (
          <>
            <Button size="sm" variant="outline" disabled={!chosen.length} onClick={() => extract(chosen.map((n) => n.path))}>
              <FolderOutputIcon /> {t("Extract selected")}
            </Button>
            <Button size="sm" variant="outline" onClick={() => extract()}>
              <FolderOutputIcon /> {t("Extract all")}
            </Button>
          </>
        )}
      </div>
      {listing.isPending ? (
        <p role="status" className="flex flex-1 items-center justify-center gap-2 text-sm text-muted-foreground">
          <Loader2Icon className="size-5 animate-spin" /> {t("Loading…")}
        </p>
      ) : listing.error ? (
        <div className="flex flex-1 flex-col items-center justify-center gap-3 p-6 text-center">
          <FileIcon node={node} className="size-16" />
          <p role="alert" className="max-w-sm text-sm text-muted-foreground">
            {errorMessage(listing.error, t("Couldn't open the ZIP file"))}
          </p>
          <Button variant="outline" onClick={() => triggerDownload(`/api/files/${node.id}/content?download=1`)}>
            <DownloadIcon /> {t("Download")}
          </Button>
        </div>
      ) : (
        <div className="min-h-0 flex-1 overflow-auto">
          {!items.length ? (
            <p className="p-6 text-center text-sm text-muted-foreground">{t("This ZIP file is empty")}</p>
          ) : (
            <table className="w-full table-fixed text-sm">
              <thead className="sticky top-0 bg-background text-xs text-muted-foreground">
                <tr className="border-b">
                  <th className="w-9 py-1.5 pl-3">
                    <Checkbox
                      aria-label={t("Select all")}
                      disabled={!choosable.length}
                      checked={!!choosable.length && chosen.length === choosable.length}
                      onCheckedChange={(on) => setPicked(new Set(on ? choosable.map((n) => n.path) : []))}
                    />
                  </th>
                  <th className="py-1.5 pr-2 text-left font-normal">{t("Name")}</th>
                  <th className="w-40 py-1.5 pr-2 text-left font-normal max-md:hidden">{t("Date modified")}</th>
                  <th className="w-24 py-1.5 pr-2 text-right font-normal max-sm:w-20">{t("Size")}</th>
                  <th className="w-11 py-1.5 pr-2">
                    <span className="sr-only">{t("Download")}</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {items.slice(0, limit).map((item, i) => (
                  // Two entries may share a path
                  <tr key={`${item.index}:${item.path}:${i}`} className="border-b hover:bg-muted/50">
                    <td className="py-1 pl-3">
                      <Checkbox
                        aria-label={t("Select {name}", { name: item.name })}
                        disabled={!!item.locked}
                        checked={picked.has(item.path) && !item.locked}
                        onCheckedChange={(on) => toggle(item.path, on === true)}
                      />
                    </td>
                    <td className="py-1 pr-2">
                      <div className="flex min-w-0 items-center gap-2">
                        <FileIcon node={{ kind: item.dir ? "folder" : "file", name: item.name, mime: "" }} className="size-4 shrink-0" />
                        {item.dir ? (
                          <button type="button" className="min-w-0 truncate rounded text-left hover:underline" onClick={() => open(item.path)}>
                            {item.name}
                          </button>
                        ) : (
                          <span className="min-w-0 truncate" title={item.name}>
                            {item.name}
                          </span>
                        )}
                      </div>
                      {item.locked && <div className="pl-6 text-xs text-muted-foreground">{lockedText(item.locked)}</div>}
                    </td>
                    <td className="truncate py-1 pr-2 text-xs text-muted-foreground max-md:hidden">
                      {/* The clock time the archive gives, which says no time zone */}
                      {item.modified !== null && formatDateTime(item.modified, "UTC")}
                    </td>
                    <td className="py-1 pr-2 text-right text-xs text-muted-foreground">{!item.dir && formatBytes(item.size)}</td>
                    <td className="py-1 pr-2">
                      {!item.dir && !item.locked && (
                        <Button variant="ghost" size="icon-sm" aria-label={t("Download {name}", { name: item.name })} title={t("Download")} onClick={() => download(item)}>
                          <DownloadIcon />
                        </Button>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          {items.length > limit && (
            <div className="p-3 text-center">
              <Button variant="outline" size="sm" onClick={() => setLimit(limit + ROWS)}>
                {t("Show more")}
              </Button>
            </div>
          )}
          {!!listing.data.hidden && (
            <p className="p-3 text-xs text-muted-foreground">{t("{n} item with an unsafe path isn't shown|{n} items with unsafe paths aren't shown", { n: listing.data.hidden })}</p>
          )}
        </div>
      )}
    </div>
  );
}

function lockedText(why: NonNullable<ArchiveItem["locked"]>) {
  if (why === "password") return t("Protected with a password");
  return why === "method" ? t("Compressed in a way that isn't supported") : t("Too large to take out");
}
