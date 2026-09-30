import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  CirclePlusIcon,
  FolderOpenIcon,
  FolderSyncIcon,
  LockIcon,
  LockOpenIcon,
  GaugeIcon,
  HardDriveIcon,
  PanelTopIcon,
  RefreshCwIcon,
  Trash2Icon,
  TruckIcon,
  UsersRoundIcon,
} from "lucide-react";
import { useNavigate } from "react-router";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { DataTable, type Column } from "@/components/DataTable";
import { useTabActions } from "@/tabs";
import { toast } from "sonner";
import { api, moveActive, type Drive, type ScanReport, type SpaceMove } from "@/api";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { AccessDialog } from "@/components/AccessDialog";
import { CreateDriveDialog } from "@/components/DriveDialogs";
import { ConfirmDialog, ErrorText, NameDialog } from "@/components/dialogs";
import { Frame, ToolButton, ToolSeparator } from "@/components/Frame";
import { DRIVE_ICON, DRIVE_KIND_LABEL, ROLE_LABEL, driveLabel } from "@/lib/drives";
import { useSettingsSearch } from "@/lib/controlPanel";
import { cn, formatBytes, formatDateTime } from "@/lib/utils";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Checkbox } from "@/components/ui/checkbox";
import { t, tServer, tc } from "@/lib/i18n";
import { waitForJob } from "@/lib/jobs";
import { refreshFiles } from "@/lib/queries";
import { useMoves } from "@/lib/moves";
import { MoveProgress, moveRoute } from "@/pages/MovesPage";
import { MoveDialog } from "@/components/MoveDialog";

const GB = 1024 ** 3;

/** Space management (admins): capacity and members of all spaces; admins can't see the contents of personal spaces */
export function AdminDrivesPage() {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const tabs = useTabActions();
  /** A check started here is running (its request answers only when the check is done) */
  const [scanning, setScanning] = useState(false);
  // Refreshed every few seconds while a folder space is being scanned, to show its progress
  const q = useQuery({
    queryKey: ["admin-drives"],
    queryFn: api.adminDrives,
    refetchInterval: (query) => (scanning || query.state.data?.some((d) => d.scanning) ? 3000 : false),
  });
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [dialog, setDialog] = useState<{ t: "quota" | "members" | "delete" | "create" | "move" | "folder"; drive?: Drive; drives?: Drive[] } | null>(null);
  /** Spaces ticked to be moved together */
  const [checked, setChecked] = useState<string[]>([]);
  /** Read-only folder spaces can be browsed, downloaded and shared, but not changed from the web */
  const setReadOnly = async (d: Drive, readOnly: boolean) => {
    try {
      await api.updateDrive(d.id, { read_only: readOnly });
      toast.success(readOnly ? t("\"{name}\" is read-only now", { name: d.name }) : t("\"{name}\" can be changed from the web now", { name: d.name }));
      refresh();
      void refreshFiles(qc, { spaces: [d.id] });
    } catch (e) {
      toast.error(e instanceof Error ? e.message : t("Couldn't save"));
    }
  };
  /** Checks a folder space for changes made on the server's folder now */
  const scanNow = async (d: Drive) => {
    setScanning(true);
    // A big folder takes a while: the list shows how far the check is as soon as it has started, and a message follows
    // it to the end
    const started = setTimeout(refresh, 500);
    try {
      const job = await waitForJob(await api.scanDrive(d.id));
      const r = job.result as ScanReport;
      if (r.error) toast.error(tServer(r.error));
      else toast.success(scanSummary(r));
    } catch (e) {
      toast.error(e instanceof Error ? e.message : t("Couldn't check the folder"));
    } finally {
      clearTimeout(started);
      setScanning(false);
      refresh();
    }
  };
  // Refreshed every few seconds while a space is being moved
  const moves = useMoves(3000);
  /** The move of a space that isn't over, else its latest */
  const moveOf = (driveId: string) => {
    const of = moves.data?.moves.filter((m) => m.drive_id === driveId) ?? [];
    return of.find(moveActive) ?? of[0];
  };
  const drives = q.data ?? [];
  const selected = drives.find((d) => d.id === selectedId) ?? null;
  const ticked = drives.filter((d) => checked.includes(d.id));
  /** What "Move to another location…" moves: the ticked spaces, else the selected one */
  const toMove = ticked.length ? ticked : selected ? [selected] : [];
  const toggle = (id: string, on: boolean) => setChecked((c) => (on ? [...c.filter((x) => x !== id), id] : c.filter((x) => x !== id)));
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["admin-drives"] });
    qc.invalidateQueries({ queryKey: ["drives"] });
  };

  const toolbar = (
    <>
      <ToolButton
        icon={CirclePlusIcon}
        label={t("New team space")}
        showLabel
        className="h-9 px-2.5 text-[13px]"
        onClick={() => setDialog({ t: "create" })}
      />
      <ToolButton
        icon={FolderSyncIcon}
        label={t("New folder space")}
        showLabel
        className="h-9 px-2.5 text-[13px]"
        onClick={() => setDialog({ t: "folder" })}
      />
      <ToolSeparator />
      {selected?.mode === "folder" && (
        <ToolButton
          icon={RefreshCwIcon}
          label={t("Check for changes")}
          showLabel
          className="h-9 px-2.5 text-[13px]"
          disabled={scanning}
          onClick={() => scanNow(selected)}
        />
      )}
      <ToolButton
        icon={TruckIcon}
        label={ticked.length > 1 ? t("Move {n} spaces to another location…", { n: ticked.length }) : t("Move to another location…")}
        showLabel
        className="h-9 px-2.5 text-[13px]"
        disabled={toMove.length === 0}
        onClick={() => setDialog({ t: "move", drives: toMove })}
      />
      <ToolButton
        icon={GaugeIcon}
        label={t("Change quota")}
        showLabel
        className="h-9 px-2.5 text-[13px]"
        disabled={!selected}
        onClick={() => selected && setDialog({ t: "quota", drive: selected })}
      />
      <ToolButton
        icon={UsersRoundIcon}
        label={t("Manage members")}
        showLabel
        className="h-9 px-2.5 text-[13px]"
        disabled={!selected || selected.kind === "personal"}
        onClick={() => selected && setDialog({ t: "members", drive: selected })}
      />
      <ToolButton
        icon={Trash2Icon}
        label={t("Delete")}
        showLabel
        className="h-9 px-2.5 text-[13px]"
        disabled={!selected || selected.kind !== "team"}
        onClick={() => selected && setDialog({ t: "delete", drive: selected })}
      />
    </>
  );

  const total = drives.reduce((s, d) => s + d.used_bytes, 0);

  const searchSettings = useSettingsSearch();
  const columns: Column<Drive>[] = [
    {
      header: (
        <Checkbox
          aria-label={t("Select all spaces to move them")}
          checked={drives.length > 0 && ticked.length === drives.length}
          indeterminate={ticked.length > 0 && ticked.length < drives.length}
          onCheckedChange={(v) => setChecked(v ? drives.map((d) => d.id) : [])}
        />
      ),
      className: "w-[38px]",
      cell: (d) => (
        <span className="flex items-center" onClick={(e) => e.stopPropagation()} onDoubleClick={(e) => e.stopPropagation()}>
          <Checkbox aria-label={t("Select \"{name}\"", { name: driveLabel(d, true) })} checked={checked.includes(d.id)} onCheckedChange={(v) => toggle(d.id, !!v)} />
        </span>
      ),
    },
    {
      header: t("Name"),
      cell: (d) => {
        const Icon = DRIVE_ICON[d.kind];
        return (
          <span className="flex items-center gap-2">
            <Icon className="size-4 shrink-0" /> {driveLabel(d, true)}
            {d.disabled && <span className="text-[11px]">{t("(disabled)")}</span>}
          </span>
        );
      },
    },
    { header: t("Type"), className: "w-[100px] max-md:hidden", cellClassName: "text-muted-foreground", cell: (d) => DRIVE_KIND_LABEL[d.kind] },
    {
      header: t("Usage"),
      className: "w-[160px]",
      cell: (d) => {
        const pct = d.quota_bytes ? Math.min(100, (d.used_bytes / d.quota_bytes) * 100) : 0;
        return (
          <div className="flex items-center gap-2">
            <span className="w-[92px] shrink-0 tabular-nums">
              {formatBytes(d.used_bytes)}
              <span className="text-muted-foreground"> / {d.quota_bytes ? formatBytes(d.quota_bytes) : tc("short", "Unlimited")}</span>
            </span>
            {d.quota_bytes > 0 && (
              <span className="h-1 flex-1 overflow-hidden rounded bg-muted">
                <span className={cn("block h-full", pct > 90 ? "bg-destructive" : "bg-brand")} style={{ width: `${pct}%` }} />
              </span>
            )}
          </div>
        );
      },
    },
    {
      header: t("Members"),
      className: "w-[80px] max-xl:hidden",
      cellClassName: "text-muted-foreground",
      cell: (d) => (d.kind === "personal" ? tc("short", "Owner only") : d.member_count),
    },
    {
      header: t("Owner"),
      className: "w-[100px] max-lg:hidden",
      cellClassName: "text-muted-foreground",
      cell: (d) => (d.kind === "company" ? t("Company") : d.owner_name),
    },
    {
      header: t("My role"),
      className: "w-[90px] max-xl:hidden",
      cellClassName: "text-muted-foreground",
      cell: (d) => (d.role ? ROLE_LABEL[d.role] : "—"),
    },
    {
      header: t("Storage location"),
      className: "w-[150px]",
      cell: (d) => {
        const move = moveOf(d.id);
        const moving = move && (move.state === "running" || move.state === "queued" || move.state === "paused");
        return d.mode === "folder" && !moving ? <FolderCell drive={d} move={move} /> : <LocationCell drive={d} move={move} />;
      },
    },
  ];

  return (
    <Frame
      toolbar={toolbar}
      icon={HardDriveIcon}
      crumbs={[{ label: t("Control panel"), to: "/admin" }, { label: tc("admin", "Spaces") }]}
      upTo="/admin"
      searchPlaceholder={t("Search settings")}
      onSearch={searchSettings}
      footer={
        <span>
          {t("{n} space · {size} used|{n} spaces · {size} used", { n: drives.length, size: formatBytes(total) })}
        </span>
      }
    >
      <DataTable
        label={t("Spaces")}
        rows={drives}
        rowKey={(d) => d.id}
        columns={columns}
        fixed
        loading={q.isLoading}
        error={q.error}
        onRetry={() => q.refetch()}
        selectedKey={selectedId}
        onSelect={setSelectedId}
        rowClassName={(d) => d.disabled && "text-muted-foreground"}
        menu={() =>
          selected ? (
            <>
              {selected.role && (
                <>
                  <DropdownMenuItem onClick={() => navigate(`/files/${selected.root_id}`)}>
                    <FolderOpenIcon /> {t("Open")}
                  </DropdownMenuItem>
                  <DropdownMenuItem onClick={() => tabs.open(`/files/${selected.root_id}`, { reuse: true })}>
                    <PanelTopIcon /> {t("Open in new tab")}
                  </DropdownMenuItem>
                  <DropdownMenuSeparator />
                </>
              )}
              <DropdownMenuItem onClick={() => setDialog({ t: "quota", drive: selected })}>
                <GaugeIcon /> {t("Change quota")}
              </DropdownMenuItem>
              {selected.mode === "folder" && (
                <DropdownMenuItem disabled={scanning} onClick={() => scanNow(selected)}>
                  <RefreshCwIcon /> {t("Check for changes")}
                </DropdownMenuItem>
              )}
              {/* Only a folder space can be made read-only; one moved into the content store can be opened up again */}
              {(selected.mode === "folder" || selected.read_only) && (
                <DropdownMenuItem onClick={() => setReadOnly(selected, !selected.read_only)}>
                  {selected.read_only ? <LockOpenIcon /> : <LockIcon />}
                  {selected.read_only ? t("Allow changes from the web") : t("Make read-only")}
                </DropdownMenuItem>
              )}
              <DropdownMenuItem onClick={() => setDialog({ t: "move", drives: [selected] })}>
                <TruckIcon /> {t("Move to another location…")}
              </DropdownMenuItem>
              {selected.kind !== "personal" && (
                <DropdownMenuItem onClick={() => setDialog({ t: "members", drive: selected })}>
                  <UsersRoundIcon /> {t("Manage members")}
                </DropdownMenuItem>
              )}
              {selected.kind === "team" && (
                <>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem variant="destructive" onClick={() => setDialog({ t: "delete", drive: selected })}>
                    <Trash2Icon /> {t("Delete space")}
                  </DropdownMenuItem>
                </>
              )}
            </>
          ) : (
            <>
              <DropdownMenuItem onClick={() => setDialog({ t: "create" })}>
                <CirclePlusIcon /> {t("New team space")}
              </DropdownMenuItem>
              <DropdownMenuItem onClick={refresh}>
                <RefreshCwIcon /> {t("Refresh")}
              </DropdownMenuItem>
            </>
          )
        }
      />
      {dialog?.t === "create" && (
        <CreateDriveDialog
          onClose={() => setDialog(null)}
          onCreated={(d) => {
            refresh();
            setSelectedId(d.id);
          }}
        />
      )}
      {dialog?.t === "folder" && (
        <FolderSpaceDialog
          onClose={() => setDialog(null)}
          onCreated={(d) => {
            setDialog(null);
            refresh();
            setSelectedId(d.id);
            toast.success(t("Space created. Its folder is being indexed."));
          }}
        />
      )}
      {dialog?.t === "quota" && dialog.drive && (
        <NameDialog
          title={t("Change quota for \"{name}\"", { name: driveLabel(dialog.drive, true) })}
          label={t("Quota (GB, enter 0 for unlimited)")}
          initial={dialog.drive.quota_bytes ? String(+(dialog.drive.quota_bytes / GB).toFixed(2)) : "0"}
          confirmText={t("Save")}
          onClose={() => setDialog(null)}
          onSubmit={async (v) => {
            const gb = Number(v);
            if (!Number.isFinite(gb) || gb < 0) throw new Error(t("Enter a number of 0 or more"));
            await api.updateDrive(dialog.drive!.id, { quota_bytes: Math.round(gb * GB) });
            setDialog(null);
            refresh();
          }}
        />
      )}
      {dialog?.t === "move" && dialog.drives && (
        <MoveDialog
          spaces={dialog.drives.map((d) => ({ ...d, label: driveLabel(d, true) }))}
          title={
            dialog.drives.length === 1
              ? t("Move \"{name}\" to another location", { name: driveLabel(dialog.drives[0], true) })
              : t("Move {n} spaces to another location", { n: dialog.drives.length })
          }
          description={
            dialog.drives.length === 1
              ? t("Now on {location} · {size}", { location: dialog.drives[0].location_name || t("A folder on the server"), size: formatBytes(dialog.drives[0].used_bytes) })
              : t("{n} spaces · {size}", { n: dialog.drives.length, size: formatBytes(dialog.drives.reduce((n, d) => n + d.used_bytes, 0)) })
          }
          from={dialog.drives.length === 1 ? dialog.drives[0].location_id : undefined}
          onClose={() => setDialog(null)}
          onDone={() => {
            setDialog(null);
            setChecked([]);
            refresh();
            qc.invalidateQueries({ queryKey: ["moves"] });
          }}
        />
      )}
      {dialog?.t === "members" && dialog.drive && (
        <AccessDialog
          nodeId={dialog.drive.root_id}
          onClose={() => {
            setDialog(null);
            refresh();
          }}
        />
      )}
      {dialog?.t === "delete" && dialog.drive && (
        <ConfirmDialog
          title={t("Delete space \"{name}\"?", { name: dialog.drive.name })}
          description={
            dialog.drive.mode === "folder"
              ? t("The space is removed from ThirtyFile. Its folder on the server, {path}, is kept with the files in it ({size}): delete it there when it's no longer needed.", {
                  path: dialog.drive.source_path ?? "",
                  size: formatBytes(dialog.drive.used_bytes),
                })
              : t("All files in this space ({size}) will be permanently deleted. This can't be undone.", { size: formatBytes(dialog.drive.used_bytes) })
          }
          confirmText={t("Delete permanently")}
          destructive
          irreversible
          onClose={() => setDialog(null)}
          onConfirm={async () => {
            await api.deleteDrive(dialog.drive!.id);
            toast.success(t("Space deleted"));
            setDialog(null);
            setSelectedId(null);
            void refreshFiles(qc, { spaces: [dialog.drive!.id] });
            for (const key of ["admin-drives", "storage-locations"]) void qc.invalidateQueries({ queryKey: [key] });
          }}
        />
      )}
    </Frame>
  );
}

/** Storage location column: the location, and the move of the space when there is one */
function LocationCell({ drive, move }: { drive: Drive; move?: SpaceMove }) {
  if (move && (move.state === "running" || move.state === "queued" || move.state === "paused")) {
    return (
      <span className="grid min-w-0 gap-0.5 text-[11px]" title={moveRoute(move)}>
        <span className="truncate text-brand">
          {move.state === "running"
            ? t("Moving to {name}", { name: move.to_name })
            : move.state === "queued"
              ? t("Waiting to move to {name}", { name: move.to_name })
              : t("Move to {name} paused", { name: move.to_name })}
        </span>
        <MoveProgress m={move} compact />
      </span>
    );
  }
  return (
    <span className="flex min-w-0 items-center gap-1" title={move?.error ? tServer(move.error) : undefined}>
      <span className="truncate">{drive.location_name || "—"}</span>
      {move?.state === "failed" && <span className="shrink-0 text-[11px] text-destructive">{t("Move stopped")}</span>}
    </span>
  );
}

function scanSummary(r: ScanReport) {
  const main = t("Checked: {added} added, {changed} changed, {moved} moved, {removed} removed", {
    added: r.added,
    changed: r.changed,
    moved: r.moved,
    removed: r.removed,
  });
  return r.skipped.length ? `${main} · ${t("{n} item skipped|{n} items skipped", { n: r.skipped.length })}` : main;
}

/** A folder space's folder and its last check (and a move that stopped) */
function FolderCell({ drive, move }: { drive: Drive; move?: SpaceMove }) {
  const r = drive.scan_report;
  const title = [drive.source_path, r?.error ? tServer(r.error) : r ? scanSummary(r) : "", ...(r?.skipped ?? [])].filter(Boolean).join("\n");
  return (
    <span className="flex min-w-0 flex-col" title={title}>
      <span className="truncate font-mono text-[12px]">{drive.source_path}</span>
      <span className={cn("truncate text-[11px]", r?.error ? "text-destructive" : "text-muted-foreground")}>
        {move?.state === "failed" && <span className="text-destructive" title={move.error ? tServer(move.error) : undefined}>{`${t("Move stopped")} · `}</span>}
        {drive.read_only && !r?.error && `${t("Read-only")} · `}
        {drive.scanning
          ? drive.scanning.phase === "reading"
            ? t("Checking the folder: {n} items read…", { n: drive.scanning.found })
            : t("Updating: {done} of {total} changes…", { done: drive.scanning.done, total: drive.scanning.total })
          : r?.error
          ? t("Can't read the folder")
          : drive.last_scan_at
            ? t("Checked {time}", { time: formatDateTime(drive.last_scan_at) })
            : t("Being indexed…")}
      </span>
    </span>
  );
}

/** A new space that shows a folder on the server */
function FolderSpaceDialog({ onClose, onCreated }: { onClose(): void; onCreated(d: Drive): void }) {
  const [name, setName] = useState("");
  const [path, setPath] = useState("");
  const [readOnly, setReadOnly] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const submit = async () => {
    setBusy(true);
    setError("");
    try {
      onCreated(await api.createDrive(name.trim(), 0, path.trim(), readOnly));
    } catch (e) {
      setError(e instanceof Error ? e.message : t("Couldn't create"));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("New folder space")}</DialogTitle>
          <DialogDescription>
            {t("Shows a folder on the server as a space. Its files stay where they are: changes made here are made in the folder, and changes made there (for example over SMB) appear here automatically.")}
          </DialogDescription>
        </DialogHeader>
        <form
          className="grid gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <div className="grid gap-1.5">
            <Label htmlFor="fs-name">{t("Name")}</Label>
            <Input id="fs-name" value={name} onChange={(e) => setName(e.target.value)} autoFocus />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="fs-path">{t("Folder on the server")}</Label>
            <Input id="fs-path" className="font-mono" value={path} onChange={(e) => setPath(e.target.value)} placeholder="/mnt/nas/shared" />
            <p className="text-xs text-muted-foreground">{t("The folder's path inside the container, for example a folder mounted with -v /srv/shared:/mnt/shared.")}</p>
          </div>
          <Label className="flex items-center gap-2 font-normal">
            <Checkbox checked={readOnly} onCheckedChange={(v) => setReadOnly(!!v)} />
            {t("Read-only: browse, download and share only")}
          </Label>
          {error && <ErrorText>{error}</ErrorText>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={busy || !name.trim() || !path.trim()}>
              {t("Create")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
