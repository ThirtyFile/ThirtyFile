import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArchiveRestoreIcon, FolderOpenIcon, RefreshCwIcon, SquareCheckIcon, Trash2Icon, TrashIcon } from "lucide-react";
import { useNavigate } from "react-router";
import { toast } from "sonner";
import { api, driveName, privateSource } from "@/api";
import { Skeleton } from "@/components/ui/skeleton";
import { ContextMenu, ContextMenuContent, ContextMenuTrigger } from "@/components/ui/context-menu";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { ConfirmDialog } from "@/components/dialogs";
import { ErrorState } from "@/components/ErrorState";
import { FileList } from "@/components/FileList";
import { Frame, ToolButton } from "@/components/Frame";
import { useMe } from "@/lib/session";
import { locale, t } from "@/lib/i18n";
import { invalidateFiles } from "@/lib/queries";
import { trashHint } from "@/lib/utils";

export function TrashPage() {
  const me = useMe();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const q = useQuery({ queryKey: ["trash"], queryFn: api.listTrash });
  // Empty trash deletes only the spaces the person manages; the trash also lists items of spaces they can only view
  const emptyable = useQuery({ queryKey: ["trash", "empty"], queryFn: api.emptyTrashPreview, enabled: me.can_delete });
  const emptyCount = (emptyable.data ?? []).reduce((sum, s) => sum + s.items, 0);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [anchor, setAnchor] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<"delete" | "empty" | null>(null);
  const items = q.data ?? [];
  const ids = [...selected];

  const done = (msg: string) => {
    toast.success(msg);
    setSelected(new Set());
    invalidateFiles(qc);
  };

  const restore = async () => {
    try {
      await api.restore(ids);
      done(t("Restored {n} item|Restored {n} items", { n: ids.length }));
    } catch (e) {
      toast.error(e instanceof Error ? e.message : t("Couldn't restore"));
    }
  };

  const toolbar = (
    <>
      <ToolButton icon={ArchiveRestoreIcon} label={t("Restore")} showLabel disabled={!ids.length} onClick={restore} />
      <ToolButton icon={Trash2Icon} label={t("Delete permanently")} showLabel disabled={!ids.length || !me.can_delete} onClick={() => setConfirm("delete")} />
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
              empty={
                <div className="flex min-h-52 flex-col items-center justify-center gap-2 py-10 text-muted-foreground">
                  <Trash2Icon className="size-9 stroke-[1.4]" />
                  <p>{t("Trash is empty")}</p>
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
              <DropdownMenuItem onClick={() => qc.invalidateQueries({ queryKey: ["trash"] })}>
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
          onClose={() => setConfirm(null)}
          onConfirm={async () => {
            if (confirm === "empty") await api.emptyTrash();
            else await api.deleteForever(ids);
            setConfirm(null);
            done(t("Permanently deleted"));
          }}
        />
      )}
    </Frame>
  );
}
