/**
 * Quick look (the Mac style): a floating window over the list with a preview of the item selected, shown by the
 * previews every style shares (components/FileViewer). The style's key (Space) opens and closes it; the arrows go to
 * the item before or after, which is selected in the list as it shows. With several items selected, it goes through
 * those, and leaves the selection as it is.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { ChevronLeftIcon, ChevronRightIcon, ExternalLinkIcon, XIcon } from "lucide-react";
import { privateSource, type Node } from "@/api";
import { Button } from "@/components/ui/button";
import { FileIcon, typeLabel } from "@/components/FileIcon";
import { FileViewer } from "@/components/FileViewer";
import type { ExplorerProps } from "@/components/Explorer";
import type { ExplorerActions } from "@/components/explorer/actions";
import type { ExplorerState } from "@/components/explorer/state";
import { isTyping } from "@/components/explorer/types";
import type { Item } from "@/components/fileList/layout";
import { useOverlayFocus } from "@/lib/focus";
import { t } from "@/lib/i18n";
import { shortcut } from "@/lib/keys";
import { pressed, pressedKey } from "@/lib/style/keymap";
import { createStore, useStore } from "@/lib/store";
import { formatBytes, formatDateTime } from "@/lib/utils";

const shown = createStore(false);

/** Opens Quick look on the item selected (the explorer shows it) */
export function openQuickLook() {
  shown.set(true);
}

/** The items Quick look goes through: those selected when there are several, else every item loaded, in the list's order */
export function lookItems(list: readonly (Item | undefined)[], selected: ReadonlySet<string>): Item[] {
  const loaded = list.filter((n): n is Item => !!n && !n.id.startsWith("new:"));
  const picked = selected.size > 1 ? loaded.filter((n) => selected.has(n.id)) : [];
  return picked.length > 1 ? picked : loaded;
}

/** The explorer's part of Quick look: its key, and the window while it is open */
export function QuickLook({ s, a }: { p: ExplorerProps; s: ExplorerState; a: ExplorerActions }) {
  const open = useStore(shown);
  const k = s.kit.keys;
  // The style's key opens it on what is selected, from the list (not while typing, in a menu or a dialog)
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || !pressed(e, k.quickLook) || isTyping(e.target) || document.querySelector("[role=dialog]")) return;
      if ((e.target as HTMLElement | null)?.closest?.("button, a, select, [role=menu], [role=menuitem], [role=tab], [role=separator]")) return;
      if (!s.count) return;
      e.preventDefault();
      shown.set(true);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });
  if (!open || !s.count) return null;
  return <QuickLookWindow s={s} a={a} onClose={() => shown.set(false)} />;
}

function QuickLookWindow({ s, a, onClose }: { s: ExplorerState; a: ExplorerActions; onClose(): void }) {
  const k = s.kit.keys;
  const items = useMemo(() => lookItems(s.shown, s.selected), [s.shown, s.selected]);
  // The item shown: at first the one with the focus among those selected (else the first selected)
  const [id, setId] = useState(() => {
    const focused = (document.activeElement as HTMLElement | null)?.closest?.<HTMLElement>("[data-node-id]")?.dataset.nodeId;
    if (focused && s.selected.has(focused)) return focused;
    return (s.anchor && s.selected.has(s.anchor) ? s.anchor : s.selectedNodes[0]?.id) ?? items[0]?.id;
  });
  const at = items.findIndex((n) => n.id === id);
  const node = items[at];
  /** Browsing the whole list: the item shown is the one selected */
  const all = !(s.selected.size > 1);
  const close = () => {
    onClose();
    // The focus goes back to the list, on the item last shown
    setTimeout(() => node && s.listNav.current?.show(node.id, true));
  };
  const go = (step: 1 | -1) => {
    const next = items[at + step];
    if (!next) return;
    setId(next.id);
    if (all) {
      s.setSelected(new Set([next.id]));
      s.setAnchor(next.id);
    }
    s.listNav.current?.show(next.id, false);
  };

  // An item that went away (deleted, moved) closes it
  useEffect(() => {
    if (!node) onClose();
  }, [node, onClose]);

  const root = useRef<HTMLDivElement>(null);
  useOverlayFocus(root, !!node, { onClose: close });
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Not while seeking in a player or typing
      const target = e.target as HTMLElement | null;
      if (target?.closest?.(".cm-editor, video, audio, input, textarea, select, [contenteditable]") || target?.closest?.('[data-slot="dialog-content"]')) return;
      if (pressed(e, k.quickLook)) close();
      else if (pressedKey(e, [...k.itemDown, ...k.itemRight]) && !e.shiftKey) go(1);
      else if (pressedKey(e, [...k.itemUp, ...k.itemLeft]) && !e.shiftKey) go(-1);
      else return;
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  });

  if (!node) return null;
  return (
    <div
      ref={root}
      role="dialog"
      aria-modal="true"
      aria-label={t("Quick look: {name}", { name: node.name })}
      className="fixed top-1/2 left-1/2 z-40 flex h-[min(580px,calc(100dvh-3rem))] w-[min(820px,calc(100vw-2rem))] -translate-x-1/2 -translate-y-1/2 flex-col overflow-hidden rounded-xl border bg-popover text-popover-foreground shadow-2xl"
    >
      <div className="flex h-11 shrink-0 items-center gap-2 border-b px-2">
        <Button variant="ghost" size="icon-sm" aria-label={t("Close")} title={`${t("Close")} (${shortcut(k.quickLook[0])})`} onClick={close}>
          <XIcon />
        </Button>
        <div className="min-w-0 flex-1 text-center">
          <div className="truncate text-[13px] font-medium" title={node.name}>
            {node.name}
          </div>
          <div className="text-[11px] text-muted-foreground">{t("{n} of {total}", { n: at + 1, total: items.length })}</div>
        </div>
        <Button variant="ghost" size="icon-sm" aria-label={t("Previous item")} title={t("Previous item")} disabled={at <= 0} onClick={() => go(-1)}>
          <ChevronLeftIcon />
        </Button>
        <Button variant="ghost" size="icon-sm" aria-label={t("Next item")} title={t("Next item")} disabled={at >= items.length - 1} onClick={() => go(1)}>
          <ChevronRightIcon />
        </Button>
        <Button
          variant="outline"
          size="sm"
          onClick={() => {
            onClose();
            a.open(node);
          }}
        >
          <ExternalLinkIcon /> {t("Open")}
        </Button>
      </div>
      <div className="relative flex min-h-0 flex-1 items-center justify-center bg-muted/30">
        {node.kind === "file" ? <FileViewer key={node.id} node={node} source={privateSource} editable={false} embedded /> : <FolderCard node={node} />}
      </div>
    </div>
  );
}

/** A folder has no preview: its icon, name, kind and dates */
function FolderCard({ node }: { node: Node }) {
  return (
    <div className="flex flex-col items-center gap-2 p-6 text-center">
      <FileIcon node={node} className="size-24 stroke-1" />
      <div className="text-sm font-medium [overflow-wrap:anywhere]">{node.name}</div>
      <div className="text-xs text-muted-foreground">
        {typeLabel(node)}
        {node.size > 0 && ` · ${formatBytes(node.size)}`}
      </div>
      <div className="text-xs text-muted-foreground">{t("Modified {date}", { date: formatDateTime(node.updated_at) })}</div>
    </div>
  );
}
