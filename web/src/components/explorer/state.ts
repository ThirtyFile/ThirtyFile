/** File explorer state: selection, view mode, dialogs, clipboard and permissions in the current folder */
import { useEffect, useMemo, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router";
import type { ViewMode } from "@/components/FileList";
import { useClipboard } from "@/lib/clipboard";
import { capsOf } from "@/lib/drives";
import { usePersisted, useMe } from "@/lib/session";
import { useTabActions } from "@/tabs";
import type { DialogState } from "./types";
import type { ExplorerProps } from "../Explorer";

export function useExplorerState(p: ExplorerProps) {
  const me = useMe();
  // In a folder, available actions depend on the role; list pages (search, recent, etc.) mix several locations, so the server decides
  const caps = p.role ? capsOf(p.role, me) : { write: me.can_write, del: me.can_delete, share: me.can_share, manage: false };
  const qc = useQueryClient();
  const navigate = useNavigate();
  const tabs = useTabActions();
  const [view, setView] = usePersisted<ViewMode>("tf-view", "list");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [anchor, setAnchor] = useState<string | null>(null);
  const [dialog, setDialog] = useState<DialogState | null>(null);
  const [dragging, setDragging] = useState(false);
  const [showCheckboxes, setShowCheckboxes] = usePersisted("tf-checkboxes", false);
  const [detailsOpen, setDetailsOpen] = usePersisted("tf-details-pane", false);
  const clip = useClipboard();
  const fileInput = useRef<HTMLInputElement>(null);
  const dirInput = useRef<HTMLInputElement>(null);

  // New folder and paste only touch the database; uploading and creating files need to write to the storage service, so they're disabled while offline
  const canCreate = !!p.folderId && caps.write;
  const canUpload = canCreate && !p.offline;
  const selectedNodes = useMemo(() => p.items.filter((n) => selected.has(n.id)), [p.items, selected]);
  const selectedIds = selectedNodes.map((n) => n.id);
  const single = selectedNodes.length === 1 ? selectedNodes[0] : null;
  const allFavorite = selectedNodes.length > 0 && selectedNodes.every((n) => n.is_favorite);

  // Clear the selection when switching folders
  useEffect(() => {
    setSelected(new Set());
    setAnchor(null);
  }, [p.folderId, p.crumbs.map((c) => c.label).join("/")]);

  return {
    me,
    caps,
    qc,
    navigate,
    tabs,
    clip,
    fileInput,
    dirInput,
    canCreate,
    canUpload,
    selectedNodes,
    selectedIds,
    single,
    allFavorite,
    view,
    setView,
    selected,
    setSelected,
    anchor,
    setAnchor,
    dialog,
    setDialog,
    dragging,
    setDragging,
    showCheckboxes,
    setShowCheckboxes,
    detailsOpen,
    setDetailsOpen,
  };
}

export type ExplorerState = ReturnType<typeof useExplorerState>;
