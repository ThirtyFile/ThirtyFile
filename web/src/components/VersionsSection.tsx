/** "Versions" in the details pane: a file's earlier content, to open, download or restore */
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { DownloadIcon, ExternalLinkIcon, HistoryIcon } from "lucide-react";
import { toast } from "sonner";
import { api, type FileVersion, type Node } from "@/api";
import { keys } from "@/api/queryKeys";
import { confirm } from "@/lib/confirm";
import { getDraft, setDraft } from "@/lib/drafts";
import { ErrorState } from "@/components/ErrorState";
import { Button, buttonVariants } from "@/components/ui/button";
import { t } from "@/lib/i18n";
import { refreshFiles, saved } from "@/lib/queries";
import { useMe } from "@/lib/session";
import { formatBytes, formatDateTime, errorMessage } from "@/lib/utils";

/** `ready`: the selection has stayed on this file for a moment, so its versions are asked for */
export function VersionsSection({ node, canRestore, ready = true }: { node: Node; canRestore: boolean; ready?: boolean }) {
  const me = useMe();
  const qc = useQueryClient();
  // Keyed by the file's version too, so saving or restoring lists the new one
  const versions = useQuery({
    queryKey: keys.versions(node.id, node.updated_at),
    queryFn: ({ signal }) => api.versions(node.id, signal),
    enabled: ready,
  });

  const restore = async (v: FileVersion) => {
    // Changes not saved yet in the spreadsheet editor would be saved over the restored content
    const unsaved = !!getDraft(node.id, "sheet");
    const ok = await confirm({
      title: t("Restore this version?"),
      description: unsaved
        ? t('"{name}" gets back the content it had on {date}. Its current content is kept as an earlier version, and the changes not saved yet in the editor are discarded.', {
            name: node.name,
            date: formatDateTime(v.modified_at),
          })
        : t('"{name}" gets back the content it had on {date}. Its current content is kept as an earlier version.', {
            name: node.name,
            date: formatDateTime(v.modified_at),
          }),
      confirmText: t("Restore"),
    });
    if (!ok) return;
    try {
      const restored = await api.restoreVersion(node.id, v.id);
      // The editor then shows the restored content
      if (unsaved) setDraft(node.id, null);
      toast.success(t("Version restored"));
      // The file's row, its details and history, and the folder it is in (sorted by size or date, it can move)
      await refreshFiles(qc, { ...saved(restored), nodes: [node.id] });
    } catch (e) {
      toast.error(errorMessage(e, t("Couldn't restore")));
    }
  };

  return (
    <section aria-labelledby="versions-heading" className="grid gap-2">
      <h3 id="versions-heading" className="flex items-center gap-1.5 text-xs font-medium">
        <HistoryIcon className="size-3.5" />
        {t("Versions")}
      </h3>
      {versions.error ? (
        <ErrorState compact message={versions.error.message} onRetry={() => versions.refetch()} />
      ) : !versions.data ? (
        <div className="text-xs text-muted-foreground">…</div>
      ) : versions.data.length === 0 ? (
        <div className="text-xs text-muted-foreground">
          {me.version_keep > 0 ? t("No earlier versions. Saving over this file keeps the content it had.") : t("Earlier versions aren't kept on this server.")}
        </div>
      ) : (
        <ul className="grid gap-1.5">
          {versions.data.map((v) => (
            <li key={v.id} className="grid gap-1 rounded-md border px-2 py-1.5 text-xs">
              <div className="font-medium">{formatDateTime(v.modified_at)}</div>
              <div className="text-muted-foreground [overflow-wrap:anywhere]">{[v.author_name, formatBytes(v.size)].filter(Boolean).join(" · ")}</div>
              <div className="flex flex-wrap gap-1">
                <a className={buttonVariants({ variant: "ghost", size: "xs" })} href={api.versionUrl(node.id, v.id)} target="_blank" rel="noopener">
                  <ExternalLinkIcon /> {t("Open")}
                </a>
                <a className={buttonVariants({ variant: "ghost", size: "xs" })} href={api.versionUrl(node.id, v.id, true)} download={node.name}>
                  <DownloadIcon /> {t("Download")}
                </a>
                {canRestore && (
                  <Button variant="ghost" size="xs" onClick={() => restore(v)}>
                    <HistoryIcon /> {t("Restore")}
                  </Button>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
