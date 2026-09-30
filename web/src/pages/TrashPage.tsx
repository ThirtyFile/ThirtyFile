import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArchiveRestoreIcon, FilterIcon, FolderOpenIcon, RefreshCwIcon, SquareCheckIcon, Trash2Icon, TrashIcon } from "lucide-react";
import { useNavigate } from "react-router";
import { toast } from "sonner";
import { api, driveName, privateSource, type Located } from "@/api";
import { keys } from "@/api/queryKeys";
import { Skeleton } from "@/components/ui/skeleton";
import { ContextMenu, ContextMenuContent, ContextMenuTrigger } from "@/components/ui/context-menu";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { ConfirmDialog } from "@/components/dialogs";
import { askBeforeTransfer } from "@/components/ConflictDialog";
import { ErrorState } from "@/components/ErrorState";
import { FileList } from "@/components/FileList";
import { Frame, ToolButton } from "@/components/Frame";
import { useMe } from "@/lib/session";
import { locale, t } from "@/lib/i18n";
import { followJob } from "@/lib/jobs";
import { useAllPages } from "@/lib/pages";
import { refreshFiles, type FileChange } from "@/lib/queries";
import { trashHint } from "@/lib/utils";

export function TrashPage() {
  const me = useMe();
  const qc = useQueryClient();
  const navigate = useNavigate();
  // Deleted by me, or by everyone (the items of every space whose trash the person sees)
  const [deletedBy, setDeletedBy] = useState<"everyone" | "me">("everyone");
  const mine = deletedBy === "me";
  const q = useAllPages(keys.trashPages(deletedBy), (limit, after, signal) => api.trashPage(limit, after, mine, signal));
  // Empty trash deletes only the spaces the person manages; the trash also lists items of spaces they can only view
  const emptyable = useQuery({ queryKey: keys.trashEmpty(), queryFn: api.emptyTrashPreview, enabled: me.can_delete });
  const emptyCount = (emptyable.data ?? []).reduce((sum, s) => sum + s.items, 0);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [anchor, setAnchor] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<"delete" | "empty" | null>(null);
  const items = q.items;
  const ids = [...selected];

  /** Restored: they leave the trash, and show again in the folders they were in */
  const restored = (sent: string[]) => {
    const wanted = new Set(sent);
    const parents = items.filter((n) => wanted.has(n.id)).map((n) => n.parent_id);
    return refreshFiles(qc, { folders: parents, nodes: sent, trash: true, contents: true, usage: true });
  };

  const restore = async () => {
    try {
      // Asks first when an item's name was taken in its folder meanwhile
      const resolutions = await askBeforeTransfer("restore", ids);
      if (!resolutions) return;
      const sent = ids.filter((id) => resolutions[id] !== "skip");
      // Every item skipped: nothing was restored, and the selection stays
      if (!sent.length) return void toast.info(t("Nothing was restored: every item was skipped"));
      try {
        await api.restore(sent, resolutions);
      } finally {
        // Also what was restored before it failed
        void restored(sent);
      }
      toast.success(t("Restored {n} item|Restored {n} items", { n: sent.length }));
      setSelected(new Set());
    } catch (e) {
      toast.error(e instanceof Error ? e.message : t("Couldn't restore"));
    }
  };

  const toolbar = (
    <>
      <ToolButton icon={ArchiveRestoreIcon} label={t("Restore")} showLabel disabled={!ids.length} onClick={restore} />
      <ToolButton icon={Trash2Icon} label={t("Delete permanently")} showLabel disabled={!ids.length || !me.can_delete} onClick={() => setConfirm("delete")} />
      <DropdownMenu>
        <DropdownMenuTrigger
          render={<ToolButton icon={FilterIcon} label={mine ? t("Deleted by me") : t("Deleted by everyone")} showLabel className={mine ? "text-brand" : undefined} />}
        />
        <DropdownMenuContent className="w-48">
          <DropdownMenuRadioGroup
            value={deletedBy}
            onValueChange={(v) => {
              setDeletedBy(v as "everyone" | "me");
              setSelected(new Set());
            }}
          >
            <DropdownMenuRadioItem value="everyone">{t("Deleted by everyone")}</DropdownMenuRadioItem>
            <DropdownMenuRadioItem value="me">{t("Deleted by me")}</DropdownMenuRadioItem>
          </DropdownMenuRadioGroup>
        </DropdownMenuContent>
      </DropdownMenu>
      <span className="flex-1" />
      <ToolButton icon={TrashIcon} label={t("Empty trash")} showLabel disabled={!emptyCount || !me.can_delete} onClick={() => setConfirm("empty")} />
    </>
  );

  return (
    <Frame
      toolbar={toolbar}
      crumbs={[{ label: t("Files") }, { label: t("Trash") }]}
      icon={Trash2Icon}
      footer={
        <span>
          {t("{n} item|{n} items", { n: items.length })}
          {q.loadingMore && ` · ${t("Loading more items…")}`}
          {selected.size > 0 && ` · ${t("{n} selected", { n: selected.size })}`} ·{" "}
          {me.trash_days > 0
            ? t("Items are permanently deleted after {n} day|Items are permanently deleted after {n} days", { n: me.trash_days })
            : t("Items stay until the trash is emptied")}
        </span>
      }
    >
      <ContextMenu>
        <ContextMenuTrigger
          className="min-h-0 flex-1 overflow-auto"
          onClick={(e) => !(e.target as HTMLElement).closest("[data-node-id]") && setSelected(new Set())}
          onContextMenuCapture={(e) => !(e.target as HTMLElement).closest("[data-node-id]") && setSelected(new Set())}
        >
          {q.isLoading ? (
            <div className="grid gap-1.5 p-3">
              {[0, 1, 2, 3].map((i) => (
                <Skeleton key={i} className="h-6" />
              ))}
            </div>
          ) : q.error ? (
            <ErrorState message={q.error.message} onRetry={() => q.refetch()} />
          ) : (
            <FileList
              items={items}
              view="list"
              source={privateSource}
              selected={selected}
              anchor={anchor}
              onSelect={(s, a) => {
                setSelected(s);
                if (a !== undefined) setAnchor(a);
              }}
              onOpen={(n) => setSelected(new Set([n.id]))}
              showLocation
              dateLabel={t("Date deleted")}
              dateOf={(n) => n.trashed_at ?? n.updated_at}
              extraColumn={{ label: t("Deleted by"), value: (n) => (n as Located).deleted_by ?? "—" }}
              empty={
                <div className="flex min-h-52 flex-col items-center justify-center gap-2 py-10 text-muted-foreground">
                  <Trash2Icon className="size-9 stroke-[1.4]" />
                  <p>{mine ? t("You haven't deleted anything that's in the trash") : t("Trash is empty")}</p>
                  <p className="text-xs">{trashHint(me.trash_days)}</p>
                </div>
              }
            />
          )}
        </ContextMenuTrigger>
        <ContextMenuContent>
          {ids.length > 0 ? (
            <>
              <DropdownMenuItem onClick={restore}>
                <ArchiveRestoreIcon /> {ids.length > 1 ? t("Restore {n} item|Restore {n} items", { n: ids.length }) : t("Restore")}
              </DropdownMenuItem>
              <DropdownMenuItem
                onClick={() => {
                  const n = items.find((i) => i.id === ids[0]);
                  if (n?.parent_id) navigate(`/files/${n.parent_id}`);
                }}
                disabled={ids.length !== 1}
              >
                <FolderOpenIcon /> {t("Open original location")}
              </DropdownMenuItem>
              {me.can_delete && (
                <>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem variant="destructive" onClick={() => setConfirm("delete")}>
                    <Trash2Icon /> {t("Delete permanently")}
                  </DropdownMenuItem>
                </>
              )}
            </>
          ) : (
            <>
              <DropdownMenuItem onClick={() => setSelected(new Set(items.map((i) => i.id)))} disabled={!items.length}>
                <SquareCheckIcon /> {t("Select all")}
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => qc.invalidateQueries({ queryKey: keys.trash() })}>
                <RefreshCwIcon /> {t("Refresh")}
              </DropdownMenuItem>
              {me.can_delete && (
                <>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem variant="destructive" onClick={() => setConfirm("empty")} disabled={!emptyCount}>
                    <TrashIcon /> {t("Empty trash")}
                  </DropdownMenuItem>
                </>
              )}
            </>
          )}
        </ContextMenuContent>
      </ContextMenu>
      {confirm && (
        <ConfirmDialog
          title={
            confirm === "empty"
              ? t("Permanently delete {n} item?|Permanently delete {n} items?", { n: emptyCount })
              : t("Permanently delete {n} item?|Permanently delete {n} items?", { n: ids.length })
          }
          description={
            confirm === "empty"
              ? t("Empties the trash of: {spaces}. Permanently deleted items can't be recovered.", {
                  spaces: (emptyable.data ?? []).map((s) => `${driveName(s)} (${s.items.toLocaleString(locale)})`).join(", "),
                })
              : t("Permanently deleted items can't be recovered.")
          }
          confirmText={t("Delete permanently")}
          destructive
          irreversible
          onClose={() => setConfirm(null)}
          onConfirm={async () => {
            const job = confirm === "empty" ? await api.emptyTrash() : await api.deleteForever(ids);
            setConfirm(null);
            // The items have left the trash; deleting a large folder goes on, followed by a message at the bottom
            setSelected(new Set());
            // Items in the trash aren't in any folder's list: only the trash and the space used change
            const change: FileChange = confirm === "empty" ? { trash: true, usage: true } : { removed: ids, usage: true };
            void refreshFiles(qc, change);
            void followJob(job, t("Permanently deleted"), () => refreshFiles(qc, change));
          }}
        />
      )}
    </Frame>
  );
}
