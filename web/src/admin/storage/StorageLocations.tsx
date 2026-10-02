import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  CheckCircle2Icon,
  CloudIcon,
  CopyCheckIcon,
  CopyIcon,
  DatabaseBackupIcon,
  EllipsisIcon,
  FolderOpenIcon,
  HardDriveIcon,
  LayersIcon,
  ListChecksIcon,
  Loader2Icon,
  PencilIcon,
  PlugZapIcon,
  PlusIcon,
  SearchXIcon,
  ServerIcon,
  StarIcon,
  Trash2Icon,
  TruckIcon,
  XCircleIcon,
} from "lucide-react";
import { useNavigate } from "react-router";
import { toast } from "sonner";
import { api, type StorageLocation } from "@/api";
import { affected, invalidate, keys, queries } from "@/api/queryKeys";
import { CopyEverythingDialog } from "@/admin/storage/CopyEverythingDialog";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { ConfirmDialog } from "@/components/dialogs";
import { ErrorState } from "@/components/ErrorState";
import { RowMenuArea } from "@/components/RowMenuArea";
import { LocationBrowseDialog, LocationTestDialog, UnusedContentDialog } from "@/admin/storage/StorageTools";
import { cn, formatBytes, formatDateTime, errorMessage } from "@/lib/utils";
import { t, tServer } from "@/lib/i18n";
import { useSelectableList } from "@/lib/listSelection";
import { useMoves } from "@/admin/storage/moves";
import { MoveEverythingDialog, MovedOff, SpacesDialog } from "@/admin/storage/LocationSpaces";
import { STORAGE_KIND_LABEL, StorageDialog } from "@/admin/storage/StorageDialog";

function describe(l: StorageLocation) {
  if (l.kind === "local") return l.config.path || "—";
  if (l.kind === "sftp" || l.kind === "ftp") {
    const c = l.config;
    const scheme = l.kind === "sftp" ? "sftp" : c.tls ? "ftps" : "ftp";
    const port = c.port && c.port !== (l.kind === "sftp" ? 22 : 21) ? `:${c.port}` : "";
    return `${scheme}://${c.username ? `${c.username}@` : ""}${c.host}${port}${c.path ? (c.path.startsWith("/") ? c.path : `/${c.path}`) : ""}`;
  }
  const host = l.config.endpoint ? l.config.endpoint.replace(/^https?:\/\//, "") : "AWS S3";
  return `${host} · ${l.config.bucket}${l.config.prefix ? `/${l.config.prefix}` : ""}`;
}

/** System settings › Storage locations */
export function StorageLocations() {
  const qc = useQueryClient();
  const navigate = useNavigate();
  // The server checks connection status every 30 seconds; the view refreshes every 30 seconds
  const q = useQuery({ ...queries.storageLocations, refetchInterval: 30_000 });
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [editing, setEditing] = useState<StorageLocation | "new" | null>(null);
  const [deleting, setDeleting] = useState<StorageLocation | null>(null);
  const [showing, setShowing] = useState<StorageLocation | null>(null);
  const [testing, setTesting] = useState<string | null>(null);
  const [tool, setTool] = useState<{ kind: "test" | "browse" | "unused"; location: StorageLocation } | null>(null);
  const [emptying, setEmptying] = useState<StorageLocation | null>(null);
  const [copying, setCopying] = useState<StorageLocation | null>(null);
  // Moves off each location: how far they are, and where the spaces went
  const moves = useMoves(3000);
  const movesFrom = (id: string) => (moves.data?.moves ?? []).filter((m) => m.from_location === id);
  const list = q.data ?? [];
  const selected = list.find((l) => l.id === selectedId) ?? null;
  // Selected and opened with the keyboard as well as the mouse (lib/listSelection.ts); Enter edits
  const rows = useSelectableList({
    items: list,
    keyOf: (l) => l.id,
    selected: new Set(selected ? [selected.id] : []),
    onSelect: (keys) => setSelectedId(keys.values().next().value ?? null),
    onOpen: (l) => setEditing(l),
    nameOf: (l) => l.name,
  });
  const refresh = () => {
    void invalidate(qc, ...affected.storage());
  };

  const test = async (l: StorageLocation) => {
    setTesting(l.id);
    try {
      await api.testExistingStorage(l.id);
      toast.success(t('Connected to "{name}" successfully', { name: l.name }));
    } catch (e) {
      toast.error(errorMessage(e, t("Connection failed")));
    } finally {
      setTesting(null);
      refresh();
    }
  };
  const makeDefault = async (l: StorageLocation) => {
    try {
      await api.setDefaultStorage(l.id);
      toast.success(t('New spaces will be stored in "{name}"', { name: l.name }));
      refresh();
    } catch (e) {
      toast.error(errorMessage(e, t("Operation failed")));
    }
  };

  const menu = (l: StorageLocation) => (
    <>
      <DropdownMenuItem onClick={() => setEditing(l)}>
        <PencilIcon /> {t("Edit")}
      </DropdownMenuItem>
      <DropdownMenuItem onClick={() => test(l)}>
        <PlugZapIcon /> {t("Test connection")}
      </DropdownMenuItem>
      <DropdownMenuItem onClick={() => setShowing(l)}>
        <LayersIcon /> {t("Spaces on this location")}
      </DropdownMenuItem>
      <DropdownMenuItem disabled={l.drive_count === 0} onClick={() => setEmptying(l)}>
        <TruckIcon /> {t("Move everything to…")}
      </DropdownMenuItem>
      <DropdownMenuItem disabled={l.drive_count === 0 || list.length < 2} onClick={() => setCopying(l)}>
        <CopyIcon /> {t("Copy everything to…")}
      </DropdownMenuItem>
      <DropdownMenuItem disabled={l.drive_count === 0 || list.length < 2} onClick={() => navigate(`/admin/backups?new=${encodeURIComponent(l.id)}`)}>
        <DatabaseBackupIcon /> {t("Back up…")}
      </DropdownMenuItem>
      <DropdownMenuItem disabled={l.drive_count === 0 || list.length < 2} onClick={() => navigate(`/admin/replicas?new=${encodeURIComponent(l.id)}`)}>
        <CopyCheckIcon /> {t("Replicate…")}
      </DropdownMenuItem>
      <DropdownMenuItem onClick={() => setTool({ kind: "test", location: l })}>
        <ListChecksIcon /> {t("Test step by step")}
      </DropdownMenuItem>
      <DropdownMenuItem onClick={() => setTool({ kind: "browse", location: l })}>
        <FolderOpenIcon /> {t("Browse")}
      </DropdownMenuItem>
      <DropdownMenuItem onClick={() => setTool({ kind: "unused", location: l })}>
        <SearchXIcon /> {t("Find unused content")}
      </DropdownMenuItem>
      <DropdownMenuItem disabled={l.is_default} onClick={() => makeDefault(l)}>
        <StarIcon /> {t("Set as default location")}
      </DropdownMenuItem>
      {!l.builtin && (
        <>
          <DropdownMenuSeparator />
          <DropdownMenuItem variant="destructive" onClick={() => setDeleting(l)}>
            <Trash2Icon /> {t("Delete")}
          </DropdownMenuItem>
        </>
      )}
    </>
  );

  return (
    <div>
      <div className="flex items-center justify-between gap-3 border-b px-4 py-3">
        <p className="text-xs leading-relaxed text-muted-foreground">
          {(() => {
            const [before, after] = t(
              "Where spaces keep their files. New spaces are created on the {default}; changing it doesn't move existing spaces. A space's files can be moved in Control panel › Spaces.",
            ).split("{default}");
            return (
              <>
                {before}
                <span className="text-foreground">{t("default location")}</span>
                {after}
              </>
            );
          })()}
        </p>
        <Button size="sm" className="shrink-0" onClick={() => setEditing("new")}>
          <PlusIcon /> {t("Add storage location")}
        </Button>
      </div>
      <RowMenuArea
        onTarget={setSelectedId}
        onClick={(e) => !(e.target as HTMLElement).closest("[data-row-id]") && setSelectedId(null)}
        menu={
          selected ? (
            menu(selected)
          ) : (
            <DropdownMenuItem onClick={() => setEditing("new")}>
              <PlusIcon /> {t("Add storage location")}
            </DropdownMenuItem>
          )
        }
      >
        {q.isLoading ? (
          <div className="flex h-20 items-center justify-center text-muted-foreground">
            <Loader2Icon className="size-5 animate-spin" />
          </div>
        ) : q.error && !q.data ? (
          <ErrorState message={q.error.message} onRetry={() => q.refetch()} />
        ) : (
          <div {...rows.listProps("grid", t("Storage locations"))} className="divide-y">
            {list.map((l) => {
              const Icon = l.kind === "s3" ? CloudIcon : l.kind === "local" ? HardDriveIcon : ServerIcon;
              return (
                <div
                  key={l.id}
                  role="row"
                  data-row-id={l.id}
                  {...rows.itemProps(l)}
                  className={cn(
                    "flex flex-wrap items-center gap-3 px-4 py-3 outline-none select-none hover:bg-muted/50 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset sm:flex-nowrap",
                    selectedId === l.id && "bg-selection shadow-[inset_3px_0_0_var(--color-brand)] hover:bg-selection",
                  )}
                >
                  <Icon className={cn("size-5 shrink-0", l.kind === "s3" ? "text-sky-500" : l.kind === "local" ? "text-muted-foreground" : "text-violet-500")} />
                  <div role="gridcell" className="min-w-0 flex-1">
                    <div className="flex items-center gap-1.5 text-sm">
                      <span className="truncate">{l.name}</span>
                      {l.is_default && <span className="shrink-0 rounded bg-brand/15 px-1.5 py-px text-[11px] text-blue-700 dark:text-blue-300">{t("Default")}</span>}
                      {l.builtin && <span className="shrink-0 rounded bg-muted px-1.5 py-px text-[11px] text-muted-foreground">{t("Built-in")}</span>}
                    </div>
                    <div className="truncate text-xs text-muted-foreground">
                      {STORAGE_KIND_LABEL[l.kind]} · {describe(l)}
                    </div>
                    {!l.connected && l.health_error && (
                      <div className="mt-0.5 truncate text-xs text-destructive" title={tServer(l.health_error)}>
                        {tServer(l.health_error)}
                      </div>
                    )}
                    <MovedOff location={l} moves={movesFrom(l.id)} list={list} onDefault={makeDefault} onDelete={() => setDeleting(l)} />
                    {l.pending_deletes > 0 && (
                      <div className="mt-0.5 text-xs text-amber-600 dark:text-amber-400">
                        {t(
                          "{n} deleted file hasn't been removed from here yet; it will be retried automatically once the connection is restored|{n} deleted files haven't been removed from here yet; they will be retried automatically once the connection is restored",
                          { n: l.pending_deletes },
                        )}
                      </div>
                    )}
                  </div>
                  {/* On a phone, on a line of its own below the rest */}
                  <div role="gridcell" className="w-48 shrink-0 text-right text-xs text-muted-foreground max-sm:order-last max-sm:w-full max-sm:pl-8 max-sm:text-left">
                    <div className="tabular-nums" title={l.folder_bytes > 0 ? t("{size} in folder spaces", { size: formatBytes(l.folder_bytes) }) : undefined}>
                      {t("{size} used", { size: formatBytes(l.used_bytes) })}
                    </div>
                    {l.disk_total_bytes != null && l.disk_free_bytes != null && (
                      <div className="tabular-nums">{t("{free} free of {total}", { free: formatBytes(l.disk_free_bytes), total: formatBytes(l.disk_total_bytes) })}</div>
                    )}
                    <div>
                      {t("{n} file|{n} files", { n: l.blob_count })} ·{" "}
                      <button
                        type="button"
                        className="underline-offset-2 hover:text-foreground hover:underline"
                        onClick={(e) => {
                          e.stopPropagation();
                          setShowing(l);
                        }}
                      >
                        {t("{n} space|{n} spaces", { n: l.drive_count })}
                      </button>
                    </div>
                  </div>
                  <span
                    role="gridcell"
                    title={l.checked_at ? t("Last checked: {time} (checked automatically every 30 seconds)", { time: formatDateTime(l.checked_at) }) : undefined}
                    className={cn("flex w-20 shrink-0 items-center gap-1 text-xs", l.connected ? "text-emerald-700 dark:text-emerald-400" : "text-destructive")}
                  >
                    {testing === l.id ? <Loader2Icon className="size-3.5 animate-spin" /> : l.connected ? <CheckCircle2Icon className="size-3.5" /> : <XCircleIcon className="size-3.5" />}
                    {l.connected ? t("Connected") : t("Unreachable")}
                  </span>
                  <div role="gridcell">
                    <DropdownMenu>
                      <DropdownMenuTrigger render={<Button size="icon-sm" variant="ghost" aria-label={t("More actions")} onClick={(e) => e.stopPropagation()} />}>
                        <EllipsisIcon />
                      </DropdownMenuTrigger>
                      <DropdownMenuContent align="end" className="w-max max-w-(--available-width)">
                        {menu(l)}
                      </DropdownMenuContent>
                    </DropdownMenu>
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </RowMenuArea>
      {editing && (
        <StorageDialog
          location={editing === "new" ? null : editing}
          onClose={() => setEditing(null)}
          onSaved={() => {
            setEditing(null);
            refresh();
          }}
        />
      )}
      {showing && <SpacesDialog location={showing} onClose={() => setShowing(null)} />}
      {emptying && (
        <MoveEverythingDialog
          location={emptying}
          onClose={() => setEmptying(null)}
          onDone={() => {
            setEmptying(null);
            refresh();
            qc.invalidateQueries({ queryKey: keys.moves() });
          }}
        />
      )}
      {copying && <CopyEverythingDialog location={copying} onClose={() => setCopying(null)} />}
      {tool?.kind === "test" && <LocationTestDialog location={tool.location} onClose={() => setTool(null)} />}
      {tool?.kind === "browse" && <LocationBrowseDialog location={tool.location} onClose={() => setTool(null)} />}
      {tool?.kind === "unused" && <UnusedContentDialog location={tool.location} onClose={() => setTool(null)} />}
      {deleting && (
        <ConfirmDialog
          title={t('Delete storage location "{name}"?', { name: deleting.name })}
          description={t("This only removes the setting; data in the storage service isn't deleted. A location can't be deleted while files or spaces are still using it.")}
          confirmText={t("Delete")}
          destructive
          onClose={() => setDeleting(null)}
          onConfirm={async () => {
            await api.deleteStorage(deleting.id);
            toast.success(t("Storage location deleted"));
            setDeleting(null);
            setSelectedId(null);
            refresh();
          }}
        />
      )}
    </div>
  );
}
