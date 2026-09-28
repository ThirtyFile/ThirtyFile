/** "Versions" in the details pane: a file's earlier content, to open, download or restore */
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { DownloadIcon, ExternalLinkIcon, HistoryIcon } from "lucide-react";
import { toast } from "sonner";
import { api, type FileVersion, type Node } from "@/api";
import { confirm } from "@/components/confirm";
import { ErrorState } from "@/components/ErrorState";
import { Button, buttonVariants } from "@/components/ui/button";
import { t } from "@/lib/i18n";
import { invalidateFiles } from "@/lib/queries";
import { useMe } from "@/lib/session";
import { formatBytes, formatWinDate } from "@/lib/utils";

export function VersionsSection({ node, canRestore }: { node: Node; canRestore: boolean }) {
  const me = useMe();
  const qc = useQueryClient();
  // Keyed by the file's version too, so saving or restoring lists the new one
  const versions = useQuery({ queryKey: ["versions", node.id, node.updated_at], queryFn: () => api.versions(node.id) });

  const restore = async (v: FileVersion) => {
    const ok = await confirm({
      title: t("Restore this version?"),
      description: t("\"{name}\" gets back the content it had on {date}. Its current content is kept as an earlier version.", {
        name: node.name,
        date: formatWinDate(v.modified_at),
      }),
      confirmText: t("Restore"),
    });
    if (!ok) return;
    try {
      await api.restoreVersion(node.id, v.id);
      toast.success(t("Version restored"));
      await invalidateFiles(qc, "versions", "node");
    } catch (e) {
      toast.error(e instanceof Error ? e.message : t("Couldn't restore"));
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
              <div className="font-medium">{formatWinDate(v.modified_at)}</div>
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
