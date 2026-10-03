/** The Columns view's preview of the file selected: its picture, name, kind, size and dates, and the main things to do with it */
import { DownloadIcon, EyeIcon, InfoIcon, Share2Icon } from "lucide-react";
import { privateSource, type Node } from "@/api";
import type { ExplorerActions } from "@/components/explorer/actions";
import type { ExplorerState } from "@/components/explorer/state";
import { typeLabel } from "@/components/FileIcon";
import { Thumb } from "@/components/fileList/thumbs";
import { Button } from "@/components/ui/button";
import { t } from "@/lib/i18n";
import { formatBytes, formatDateTime } from "@/lib/utils";

export function PreviewColumn({ node, s, a }: { node: Node; s: ExplorerState; a: ExplorerActions }) {
  const rows: [string, string][] = [
    [t("Type"), typeLabel(node)],
    [t("Size"), formatBytes(node.size)],
    [t("Date modified"), formatDateTime(node.updated_at)],
    [t("Date created"), formatDateTime(node.created_at)],
  ];
  return (
    <section aria-label={t("Preview")} className="flex flex-col items-center gap-3 p-4 text-center">
      <div className="flex h-40 w-full items-center justify-center">
        <Thumb key={node.id} node={node} source={privateSource} className="max-h-40 max-w-full rounded shadow" iconClass="size-16" />
      </div>
      <h2 className="text-sm font-medium [overflow-wrap:anywhere]">{node.name}</h2>
      <dl className="grid w-full grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1.5 text-left text-xs">
        {rows.map(([k, v]) => (
          <div key={k} className="contents">
            <dt className="text-muted-foreground">{k}</dt>
            <dd className="[overflow-wrap:anywhere] select-text">{v}</dd>
          </div>
        ))}
      </dl>
      <div className="flex flex-wrap justify-center gap-1.5">
        <Button size="sm" variant="outline" onClick={() => a.open(node)}>
          <EyeIcon /> {t("Open")}
        </Button>
        <Button size="sm" variant="outline" onClick={() => a.download(s.picked)}>
          <DownloadIcon /> {t("Download")}
        </Button>
        {s.caps.share && (
          <Button size="sm" variant="outline" onClick={() => s.setDialog({ t: "share", node })}>
            <Share2Icon /> {t("Create share link")}
          </Button>
        )}
        <Button size="sm" variant="outline" onClick={() => s.setDetailsOpen(true)}>
          <InfoIcon /> {t("Properties")}
        </Button>
      </div>
    </section>
  );
}
