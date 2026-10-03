/**
 * The Gallery view (the Mac style): a large preview of the item selected, a strip of thumbnails below it to move through
 * the list, and beside them a panel with the item's details and main actions, which can be hidden.
 *
 * The strip is the explorer's own list laid out across (components/FileList, view "gallery"): its selection, the
 * style's keys (← and → go to the item before and after, Space is Quick look), file operations, dragging and loading a
 * large folder a part at a time are every view's. The preview is the one every style shares (components/FileViewer),
 * shown once the selection stays on an item; meanwhile, the item's thumbnail. Moving to another item is announced.
 */
import { useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";
import { DownloadIcon, ExternalLinkIcon, EyeIcon, InfoIcon, PanelRightIcon, PencilIcon, Share2Icon, Trash2Icon } from "lucide-react";
import { privateSource } from "@/api";
import { useSettled } from "@/components/DetailsPane";
import { FileList } from "@/components/FileList";
import { FileViewer } from "@/components/FileViewer";
import { typeLabel } from "@/components/FileIcon";
import { InlineRename } from "@/components/InlineRename";
import type { ExplorerActions } from "@/components/explorer/actions";
import type { ExplorerState } from "@/components/explorer/state";
import type { Item } from "@/components/fileList/layout";
import { Thumb } from "@/components/fileList/thumbs";
import { TagNames } from "@/components/tags";
import { Button } from "@/components/ui/button";
import { t } from "@/lib/i18n";
import { usePersisted } from "@/lib/session";
import { formatBytes, formatDateTime } from "@/lib/utils";
import type { OwnViewProps } from "../types";
import { FolderCard, openQuickLook } from "./quickLook";

/** How long the selection stays on an item before its preview loads (holding an arrow key passes many) */
const SETTLE_MS = 200;

/**
 * The item the gallery shows: of the items selected, the one with the focus (the end Shift moved), else the anchor
 * (the item clicked last), else the first. None when nothing is selected.
 */
export function galleryItem(selected: readonly Item[], focused: string | null, anchor: string | null): Item | undefined {
  const byId = (id: string | null) => (id === null ? undefined : selected.find((n) => n.id === id));
  return byId(focused) ?? byId(anchor) ?? selected[0];
}

/** Where an item is in the list (-1: not there), by the list's index when it has one (a large folder) */
export function positionOf(id: string, shown: readonly (Item | undefined)[], index?: ReadonlyMap<string, number>): number {
  return index?.get(id) ?? shown.findIndex((n) => n?.id === id);
}

export function GalleryView({ p, s, a, list, empty }: OwnViewProps) {
  const [details, setDetails] = usePersisted("tf-gallery-details", true);
  const detailsId = useId();
  /** The item of the strip with the focus */
  const [focused, setFocused] = useState<string | null>(null);
  // An item being renamed shows, with its name to type in place of its title
  const dialog = s.dialog;
  const renaming = dialog?.t === "rename" ? s.items.find((n) => n.id === dialog.node.id) : undefined;
  const node = renaming ?? galleryItem(s.selectedNodes, focused, s.anchor);
  const position = useMemo(() => (node ? positionOf(node.id, s.shown, p.list?.index) : -1), [node, s.shown, p.list]);

  // Something is shown from the start, as a Finder window does: the first item, once the list has loaded, unless
  // something else is selected (the folder came from, going up) or about to be
  const first = s.shown[0];
  const place = `${p.folderId ?? ""}|${p.crumbs.map((c) => c.label).join("/")}`;
  const started = useRef<string | null>(null);
  useEffect(() => {
    if (started.current === place) return;
    if (s.count > 0) {
      started.current = place;
      return;
    }
    if (p.loading || !first || s.arriving()) return;
    s.setSelected(new Set([first.id]));
    s.setAnchor(first.id);
  });

  // Moving to another item is announced: its name, and where it is in the list
  const [said, setSaid] = useState("");
  const last = useRef<string | null>(null);
  const id = node?.id ?? null;
  useEffect(() => {
    if (id && node && last.current !== null && last.current !== id && !renaming) setSaid(t("{name}, {n} of {total}", { name: node.name, n: position + 1, total: s.total }));
    last.current = id;
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- when another item shows
  }, [id]);

  // The preview of the item the selection stays on; its thumbnail meanwhile
  const settled = useSettled(id, SETTLE_MS);
  let preview;
  if (!node) preview = <p className="text-sm text-muted-foreground">{t("Select an item to see it here.")}</p>;
  else if (node.kind === "folder") preview = <FolderCard node={node} />;
  else if (settled === node.id) preview = <FileViewer key={node.id} node={node} source={privateSource} editable={false} embedded autoPlay={false} />;
  else preview = <Thumb key={node.id} node={node} source={privateSource} className="max-h-full max-w-full rounded object-contain" iconClass="size-24 stroke-1" />;

  if (!s.shown.length) return <>{empty}</>;
  return (
    // The strip and the preview aren't selected with a box (useMarquee)
    <div data-no-marquee className="flex h-full min-h-0">
      <div role="status" className="sr-only">
        {said}
      </div>
      <div className="flex min-w-0 flex-1 flex-col">
        <section
          aria-label={node ? t("Preview: {name}", { name: node.name }) : t("Preview")}
          // A click or right-click here leaves the item selected (its menu is the item's)
          data-keeps-selection
          className="flex min-h-0 flex-1 flex-col"
        >
          <div className="flex h-10 shrink-0 items-center gap-2 border-b px-3">
            {renaming ? (
              <div className="max-w-sm min-w-0 flex-1">
                <InlineRename
                  key={renaming.id}
                  initial={renaming.name}
                  selectAll={renaming.kind === "folder"}
                  onSubmit={(name) => a.renameItem(renaming, name)}
                  onDone={(byKey) => {
                    a.renameDone();
                    // Enter or Esc: back to the item in the strip
                    if (byKey) setTimeout(() => s.listNav.current?.show(renaming.id, true));
                  }}
                />
              </div>
            ) : (
              <h2 className="min-w-0 truncate text-[13px] font-medium" title={node?.name}>
                {node?.name}
              </h2>
            )}
            {node && position >= 0 && <span className="shrink-0 text-[11px] text-muted-foreground">{t("{n} of {total}", { n: position + 1, total: s.total })}</span>}
          </div>
          <div className="relative flex min-h-0 flex-1 items-center justify-center overflow-hidden bg-muted/30">{preview}</div>
        </section>
        <div className="flex h-[88px] shrink-0 items-center border-t">
          <div
            className="h-full min-w-0 flex-1 overflow-x-auto overflow-y-hidden"
            onFocus={(e) => {
              const at = (e.target as HTMLElement).closest<HTMLElement>("[data-node-id]")?.dataset.nodeId;
              if (at) setFocused(at);
            }}
          >
            <FileList {...list} view="gallery" groupBy="none" label={t("Thumbnails")} renamingId={null} empty={empty} />
          </div>
          <Button
            variant="ghost"
            size="icon-sm"
            className="mx-2 shrink-0"
            aria-label={details ? t("Hide details") : t("Show details")}
            title={details ? t("Hide details") : t("Show details")}
            aria-controls={details ? detailsId : undefined}
            aria-pressed={details}
            // Not a click on the list's empty space, which clears the selection
            data-keeps-selection
            onClick={() => setDetails(!details)}
          >
            <PanelRightIcon />
          </Button>
        </div>
      </div>
      {details && (
        <aside id={detailsId} aria-label={t("Details")} data-keeps-selection className="flex w-60 shrink-0 flex-col gap-3 overflow-y-auto border-l p-4 text-xs xl:w-72">
          <Details node={node} s={s} a={a} />
        </aside>
      )}
    </div>
  );
}

/** The panel beside the preview: what the item is, and what to do with it (with several selected, with them all) */
function Details({ node, s, a }: { node?: Item; s: ExplorerState; a: ExplorerActions }) {
  const { caps } = s;
  const trash = caps.del && s.count > 0 && (
    <Button size="sm" variant="outline" onClick={() => s.setDialog({ t: "trash", picked: s.picked })}>
      <Trash2Icon /> {t("Move to trash")}
    </Button>
  );
  const download = s.count > 0 && (
    <Button size="sm" variant="outline" onClick={() => a.download(s.picked)}>
      <DownloadIcon /> {t("Download")}
    </Button>
  );
  if (s.count > 1)
    return (
      <>
        <h3 className="text-sm font-medium">{t("{n} item selected|{n} items selected", { n: s.count })}</h3>
        <div className="flex flex-col items-start gap-1.5">
          {download}
          {trash}
        </div>
      </>
    );
  if (!node) return <p className="text-muted-foreground">{t("Select an item to see it here.")}</p>;
  const rows: [string, ReactNode][] = [
    [t("Type"), typeLabel(node)],
    ...(node.kind === "file" ? ([[t("Size"), formatBytes(node.size)]] as [string, string][]) : []),
    ...(node.location ? ([[t("Location"), node.location]] as [string, string][]) : []),
    [t("Date modified"), formatDateTime(node.updated_at)],
    [t("Date created"), formatDateTime(node.created_at)],
    ...(node.tags?.length ? ([[t("Tags"), <TagNames key="tags" ids={node.tags} />]] as [string, ReactNode][]) : []),
  ];
  return (
    <>
      <h3 className="text-sm font-medium [overflow-wrap:anywhere]">{node.name}</h3>
      <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1.5">
        {rows.map(([k, v]) => (
          <div key={k} className="contents">
            <dt className="text-muted-foreground">{k}</dt>
            <dd className="[overflow-wrap:anywhere] select-text">{v}</dd>
          </div>
        ))}
      </dl>
      <div className="flex flex-col items-start gap-1.5">
        <Button size="sm" variant="outline" onClick={() => a.open(node)}>
          <ExternalLinkIcon /> {t("Open")}
        </Button>
        <Button size="sm" variant="outline" onClick={openQuickLook}>
          <EyeIcon /> {t("Quick look")}
        </Button>
        {download}
        {caps.share && (
          <Button size="sm" variant="outline" onClick={() => s.setDialog({ t: "share", node })}>
            <Share2Icon /> {t("Create share link")}
          </Button>
        )}
        {caps.write && (
          <Button size="sm" variant="outline" onClick={() => s.setDialog({ t: "rename", node })}>
            <PencilIcon /> {t("Rename")}
          </Button>
        )}
        <Button size="sm" variant="outline" onClick={() => s.setDetailsOpen(true)}>
          <InfoIcon /> {t("Get info")}
        </Button>
        {trash}
      </div>
    </>
  );
}
