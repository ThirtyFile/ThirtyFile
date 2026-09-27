import { useState, type MouseEvent, type ReactNode } from "react";
import { useNavigate } from "react-router";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ChevronDownIcon,
  CirclePlusIcon,
  FolderOpenIcon,
  Grid2X2Icon,
  InfoIcon,
  ListIcon,
  LayersIcon,
  PanelTopIcon,
  PencilIcon,
  RefreshCwIcon,
  Trash2Icon,
  UsersRoundIcon,
  CloudOffIcon,
} from "lucide-react";
import { toast } from "sonner";
import { api, type Drive, type SharedItem } from "@/api";
import { Button } from "@/components/ui/button";
import { ContextMenu, ContextMenuContent, ContextMenuTrigger } from "@/components/ui/context-menu";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { AccessDialog } from "@/components/AccessDialog";
import { ActivityLog } from "@/components/logs/ActivityLog";
import { ConfirmDialog, ErrorText } from "@/components/dialogs";
import { InlineRename } from "@/components/InlineRename";
import { useMarquee } from "@/components/useMarquee";
import { FileIcon } from "@/components/FileIcon";
import { Frame, ToolButton, ToolSeparator } from "@/components/Frame";
import { DRIVE_ICON, DRIVE_KIND_LABEL, ROLE_LABEL, atLeast, useDrives } from "@/lib/drives";
import { usePersisted, useMe } from "@/lib/session";
import { cn, formatBytes } from "@/lib/utils";
import { t, tServer, tc } from "@/lib/i18n";
import { invalidateFiles } from "@/lib/queries";
import { useTabActions } from "@/tabs";

const GB = 1024 ** 3;

type Item = { t: "drive"; drive: Drive } | { t: "shared"; item: SharedItem };
type Selection = Item | null;

/** Key used for selection (space and shared item ids may collide, so add a prefix) */
const keyOf = (s: Item) => (s.t === "drive" ? `d:${s.drive.id}` : `s:${s.item.id}`);
type DialogState =
  | { t: "access"; nodeId: string }
  | { t: "create" }
  | { t: "rename"; drive: Drive }
  | { t: "delete"; drive: Drive }
  | { t: "members"; drive: Drive }
  | { t: "props"; drive: Drive }
  | null;

function Usage({ d }: { d: Drive }) {
  const pct = d.quota_bytes > 0 ? Math.min(100, (d.used_bytes / d.quota_bytes) * 100) : 0;
  return (
    <>
      <div className="mt-1.5 mb-1 h-1.5 overflow-hidden rounded-full border bg-muted">
        <div
          className={cn("h-full", pct > 90 ? "bg-destructive" : pct > 80 ? "bg-amber-500" : "bg-brand")}
          style={{ width: `${d.quota_bytes > 0 ? pct : 100}%`, opacity: d.quota_bytes > 0 ? 1 : 0.25 }}
        />
      </div>
      <div className="truncate text-[11px] text-muted-foreground">
        {d.quota_bytes > 0
          ? t("{free} free of {total}", { free: formatBytes(Math.max(0, d.quota_bytes - d.used_bytes)), total: formatBytes(d.quota_bytes) })
          : t("{used} used · No quota", { used: formatBytes(d.used_bytes) })}
        {d.kind !== "personal" && ` · ${d.kind === "company" && d.member_count <= 1 ? t("Everyone") : tc("drive", "{n} member|{n} members", { n: d.member_count })}`}
      </div>
    </>
  );
}

/** Storage service offline (e.g. S3 disconnected): can be browsed, but not opened, downloaded or uploaded to */
function OfflineBadge({ d }: { d: Drive }) {
  if (!d.offline) return null;
  return (
    <span
      className="ml-1.5 inline-flex items-center gap-1 rounded bg-destructive/12 px-1.5 py-px text-[11px] whitespace-nowrap text-destructive"
      title={t("Can't connect to the storage service: {reason}. You can browse the file list, but can't open, download, or upload for now.", { reason: tServer(d.offline) })}
    >
      <CloudOffIcon className="size-3" /> {t("Offline")}
    </span>
  );
}

function RoleBadge({ role }: { role: string | null }) {
  if (!role) return null;
  const cls =
    role === "owner"
      ? "bg-brand/15 text-brand"
      : role === "manager"
        ? "bg-violet-500/15 text-violet-600 dark:text-violet-300"
        : role === "editor"
          ? "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300"
          : "bg-muted text-muted-foreground";
  return <span className={cn("ml-1.5 rounded px-1.5 py-px text-[11px] whitespace-nowrap", cls)}>{ROLE_LABEL[role as keyof typeof ROLE_LABEL]}</span>;
}

function Section({ title, count, children }: { title: string; count: number; children: ReactNode }) {
  const [open, setOpen] = useState(true);
  return (
    <section>
      <button
        type="button"
        onClick={() => setOpen(!open)}
        className="mt-4 mb-2 flex items-center gap-1.5 text-xs text-muted-foreground hover:text-foreground"
      >
        <ChevronDownIcon className={cn("size-3.5 transition-transform", !open && "-rotate-90")} />
        {t("{title} ({n})", { title, n: count })}
      </button>
      {open && children}
    </section>
  );
}

export function ThisPcPage() {
  const me = useMe();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const tabs = useTabActions();
  const drives = useDrives();
  const shared = useQuery({ queryKey: ["shared-with-me"], queryFn: api.sharedWithMe });
  const [view, setView] = usePersisted<"tiles" | "list">("tf-drives-view", "tiles");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [anchor, setAnchor] = useState<string | null>(null);
  const [dialog, setDialog] = useState<DialogState>(null);

  const list = drives.data ?? [];
  const personal = list.filter((d) => d.kind === "personal");
  const common = list.filter((d) => d.kind !== "personal");
  const sharedItems = shared.data ?? [];

  // On-screen order (for Shift range selection)
  const ordered: Item[] = [
    ...(view === "list" ? list : [...personal, ...common]).map((drive) => ({ t: "drive" as const, drive })),
    ...sharedItems.map((item) => ({ t: "shared" as const, item })),
  ];
  const selItems = ordered.filter((s) => selected.has(keyOf(s)));
  // Single-item actions in the toolbar and context menu are only available when exactly one is selected
  const sel: Selection = selItems.length === 1 ? selItems[0] : null;
  const selectOnly = (s: Selection) => {
    setSelected(s ? new Set([keyOf(s)]) : new Set());
    setAnchor(s ? keyOf(s) : null);
  };
  // Hold the left button and drag on empty space to marquee-select (disabled while renaming)
  const marquee = useMarquee({ selected, onSelect: setSelected, enabled: !drives.isLoading && dialog?.t !== "rename" });

  const drive = sel?.t === "drive" ? sel.drive : null;
  const canMembers = !!drive && drive.kind !== "personal" && (atLeast(drive.role, "manager") || me.role === "admin");
  const canRename = !!drive && drive.kind === "team" && (atLeast(drive.role, "manager") || me.role === "admin");
  const canDelete = !!drive && drive.kind === "team" && (drive.role === "owner" || me.role === "admin");

  const openSel = (s: Selection, newTab = false) => {
    if (!s) return;
    const url = s.t === "drive" ? `/files/${s.drive.root_id}` : s.item.kind === "folder" ? `/files/${s.item.id}` : `/view/${s.item.id}`;
    if (newTab) tabs.open(url, { reuse: true });
    else if (s.t === "shared" && s.item.kind === "file") tabs.openFile(url);
    else navigate(url);
  };

  const refresh = () => qc.invalidateQueries({ queryKey: ["drives"] });

  const driveMenu = (d: Drive) => (
    <>
      <DropdownMenuItem onClick={() => openSel({ t: "drive", drive: d })}>
        <FolderOpenIcon /> {t("Open")}
      </DropdownMenuItem>
      <DropdownMenuItem onClick={() => openSel({ t: "drive", drive: d }, true)}>
        <PanelTopIcon /> {t("Open in new tab")}
      </DropdownMenuItem>
      <DropdownMenuSeparator />
      {d.kind !== "personal" && (atLeast(d.role, "manager") || me.role === "admin") && (
        <DropdownMenuItem onClick={() => setDialog({ t: "members", drive: d })}>
          <UsersRoundIcon /> {t("Manage members")}
        </DropdownMenuItem>
      )}
      {d.kind === "team" && (atLeast(d.role, "manager") || me.role === "admin") && (
        <DropdownMenuItem onClick={() => setDialog({ t: "rename", drive: d })}>
          <PencilIcon /> {t("Rename")}
        </DropdownMenuItem>
      )}
      <DropdownMenuItem onClick={() => setDialog({ t: "props", drive: d })}>
        <InfoIcon /> {t("Properties")}
      </DropdownMenuItem>
      {d.kind === "team" && (d.role === "owner" || me.role === "admin") && (
        <>
          <DropdownMenuSeparator />
          <DropdownMenuItem variant="destructive" onClick={() => setDialog({ t: "delete", drive: d })}>
            <Trash2Icon /> {t("Delete space")}
          </DropdownMenuItem>
        </>
      )}
    </>
  );

  // Click: Ctrl toggles, Shift selects a range (like Windows)
  const clickItem = (e: MouseEvent, s: Item) => {
    const key = keyOf(s);
    const a = anchor === null ? -1 : ordered.findIndex((x) => keyOf(x) === anchor);
    if (e.shiftKey && a >= 0) {
      const i = ordered.findIndex((x) => keyOf(x) === key);
      const next = new Set(e.ctrlKey || e.metaKey ? selected : []);
      for (let j = Math.min(a, i); j <= Math.max(a, i); j++) next.add(keyOf(ordered[j]));
      setSelected(next);
    } else if (e.ctrlKey || e.metaKey) {
      const next = new Set(selected);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      setSelected(next);
      setAnchor(key);
    } else {
      selectOnly(s);
    }
  };

  const itemProps = (s: Item) => ({
    "data-node-id": keyOf(s),
    onClick: (e: MouseEvent) => {
      e.stopPropagation();
      clickItem(e, s);
    },
    onDoubleClick: () => openSel(s),
    onAuxClick: (e: MouseEvent) => {
      if (e.button === 1) {
        e.preventDefault();
        openSel(s, true);
      }
    },
    onMouseDown: (e: MouseEvent) => e.button === 1 && e.preventDefault(),
    // Right-clicking an unselected item selects only that item
    onContextMenu: () => !selected.has(keyOf(s)) && selectOnly(s),
  });

  const isSel = (s: Item) => selected.has(keyOf(s));

  // Generated by a function (rather than defining a component inside the component), so re-renders keep the same DOM element and double-click fires
  // Inline rename (like renaming a drive in Windows "This PC")
  const isRenaming = (d: Drive) => dialog?.t === "rename" && dialog.drive.id === d.id;
  const renameBox = (d: Drive, className?: string) => (
    <InlineRename
      initial={d.name}
      selectAll
      className={className}
      onSubmit={async (name) => {
        await api.updateDrive(d.id, { name });
        refresh();
      }}
      onDone={() => setDialog(null)}
    />
  );

  const driveTile = (d: Drive) => {
    const Icon = DRIVE_ICON[d.kind];
    const s = { t: "drive" as const, drive: d };
    return (
      <div
        key={d.id}
        {...itemProps(s)}
        data-item
        className={cn(
          "flex cursor-default gap-3 rounded-md border border-transparent p-3 select-none hover:bg-muted/70",
          isSel(s) && "border-brand bg-selection hover:bg-selection",
        )}
      >
        <Icon
          className={cn(
            "mt-0.5 size-8 shrink-0 stroke-[1.4]",
            d.offline && "opacity-40",
            d.kind === "personal"
              ? "text-brand"
              : d.kind === "company"
                ? "text-emerald-600 dark:text-emerald-400"
                : "text-violet-600 dark:text-violet-400",
          )}
        />
        <div className="min-w-0 flex-1">
          <div className="flex items-center">
            {isRenaming(d) ? (
              renameBox(d, "h-6 text-[13px]")
            ) : (
              <>
                <span className="truncate text-[13px]">{d.name}</span>
                <RoleBadge role={d.role} />
                <OfflineBadge d={d} />
              </>
            )}
          </div>
          <Usage d={d} />
        </div>
      </div>
    );
  };

  const sharedTile = (item: SharedItem) => {
    const s = { t: "shared" as const, item };
    return (
      <div
        key={item.id}
        {...itemProps(s)}
        data-item
        className={cn(
          "flex cursor-default gap-3 rounded-md border border-transparent p-3 select-none hover:bg-muted/70",
          isSel(s) && "border-brand bg-selection hover:bg-selection",
        )}
        title={item.name}
      >
        <FileIcon node={item} className="mt-0.5 size-8 stroke-[1.4]" />
        <div className="min-w-0 flex-1">
          <div className="flex items-center">
            <span className="truncate text-[13px]">{item.name}</span>
            <RoleBadge role={item.role} />
          </div>
          <div className="mt-1 truncate text-[11px] text-muted-foreground">{tc("drive", "Shared by {name}", { name: item.sharer || t("(unknown)") })}</div>
        </div>
      </div>
    );
  };

  const grid = "grid grid-cols-[repeat(auto-fill,minmax(250px,1fr))] gap-1.5";

  const listView = (
    <table className="w-full table-fixed border-collapse text-xs whitespace-nowrap">
      <thead>
        <tr className="border-b text-left text-muted-foreground">
          {[t("Name"), t("Type"), t("Used"), t("Total size"), t("My role"), t("Owner")].map((h, i) => (
            <th
              key={h}
              className={cn(
                "h-[30px] px-2 font-normal",
                i === 0 ? "pl-3" : i === 1 ? "w-[120px] max-md:hidden" : i === 5 ? "w-[110px] max-md:hidden" : "w-[100px]",
              )}
            >
              {h}
            </th>
          ))}
        </tr>
      </thead>
      <tbody>
        {list.map((d) => {
          const Icon = DRIVE_ICON[d.kind];
          const s = { t: "drive" as const, drive: d };
          return (
            <tr
              key={d.id}
              {...itemProps(s)}
              data-item
              className={cn("h-7 cursor-default hover:bg-muted/70", isSel(s) && "bg-selection shadow-[inset_3px_0_0_var(--color-brand)] hover:bg-selection")}
            >
              <td className="truncate px-2 pl-3">
                <span className="flex items-center gap-2">
                  <Icon className={cn("size-4 shrink-0", d.offline && "opacity-50")} /> {isRenaming(d) ? renameBox(d) : d.name}
                  <OfflineBadge d={d} />
                </span>
              </td>
              <td className="px-2 text-muted-foreground max-md:hidden">{DRIVE_KIND_LABEL[d.kind]}</td>
              <td className="px-2 text-muted-foreground">{formatBytes(d.used_bytes)}</td>
              <td className="px-2 text-muted-foreground">{d.quota_bytes ? formatBytes(d.quota_bytes) : tc("short", "Unlimited")}</td>
              <td className="px-2 text-muted-foreground">{d.role ? ROLE_LABEL[d.role] : "—"}</td>
              <td className="truncate px-2 text-muted-foreground max-md:hidden">{d.kind === "company" ? t("Company") : d.owner_name}</td>
            </tr>
          );
        })}
        {sharedItems.map((item) => {
          const s = { t: "shared" as const, item };
          return (
            <tr
              key={item.id}
              {...itemProps(s)}
              data-item
              className={cn("h-7 cursor-default hover:bg-muted/70", isSel(s) && "bg-selection shadow-[inset_3px_0_0_var(--color-brand)] hover:bg-selection")}
            >
              <td className="truncate px-2 pl-3">
                <span className="flex items-center gap-2">
                  <FileIcon node={item} className="size-4" /> {item.name}
                </span>
              </td>
              <td className="px-2 text-muted-foreground max-md:hidden">{t("Shared with me")}</td>
              <td className="px-2 text-muted-foreground">{item.kind === "file" ? formatBytes(item.size) : ""}</td>
              <td className="px-2 text-muted-foreground" />
              <td className="px-2 text-muted-foreground">{ROLE_LABEL[item.role]}</td>
              <td className="truncate px-2 text-muted-foreground max-md:hidden">{item.sharer}</td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );

  const toolbar = (
    <>
      <ToolButton
        icon={CirclePlusIcon}
        label={t("New space")}
        showLabel
        className="h-9 px-2.5 text-[13px]"
        disabled={!me.can_create_drive}
        onClick={() => setDialog({ t: "create" })}
      />
      <ToolSeparator />
      <ToolButton icon={FolderOpenIcon} label={t("Open")} showLabel className="h-9 px-2.5 text-[13px]" disabled={!sel} onClick={() => openSel(sel)} />
      <ToolButton
        icon={UsersRoundIcon}
        label={t("Manage members")}
        showLabel
        className="h-9 px-2.5 text-[13px]"
        disabled={!canMembers}
        onClick={() => drive && setDialog({ t: "members", drive })}
      />
      <ToolButton
        icon={PencilIcon}
        label={t("Rename")}
        className="size-9 px-0 [&_svg]:size-[18px]"
        disabled={!canRename}
        onClick={() => drive && setDialog({ t: "rename", drive })}
      />
      <ToolButton
        icon={InfoIcon}
        label={t("Properties")}
        className="size-9 px-0 [&_svg]:size-[18px]"
        disabled={!drive}
        onClick={() => drive && setDialog({ t: "props", drive })}
      />
      <ToolButton
        icon={Trash2Icon}
        label={t("Delete space")}
        className="size-9 px-0 [&_svg]:size-[18px]"
        disabled={!canDelete}
        onClick={() => drive && setDialog({ t: "delete", drive })}
      />
      <span className="flex-1" />
      <Button variant={view === "tiles" ? "secondary" : "ghost"} aria-pressed={view === "tiles"} size="icon-sm" aria-label={t("Tiles")} title={t("Tiles")} onClick={() => setView("tiles")}>
        <Grid2X2Icon />
      </Button>
      <Button variant={view === "list" ? "secondary" : "ghost"} aria-pressed={view === "list"} size="icon-sm" aria-label={t("Details")} title={t("Details")} onClick={() => setView("list")}>
        <ListIcon />
      </Button>
    </>
  );

  return (
    <Frame
      toolbar={toolbar}
      icon={LayersIcon}
      crumbs={[{ label: t("All spaces") }]}
      upTo={null}
      searchPlaceholder={t("Search all spaces")}
      footer={
        <span>
          {[
            t("{n} space|{n} spaces", { n: list.length }),
            sharedItems.length > 0 && t("{n} shared item|{n} shared items", { n: sharedItems.length }),
            selItems.length > 0 && t("{n} item selected|{n} items selected", { n: selItems.length }),
          ]
            .filter(Boolean)
            .join(" · ")}
        </span>
      }
    >
      <ContextMenu>
        <ContextMenuTrigger
          className="relative min-h-0 flex-1 overflow-auto px-4 pb-6"
          onClick={() => selectOnly(null)}
          onContextMenuCapture={(e) => !(e.target as HTMLElement).closest("[data-item]") && selectOnly(null)}
          {...marquee.containerProps}
        >
          {marquee.box && (
            <div
              className="pointer-events-none absolute z-10 border border-brand bg-brand/15"
              style={{ left: marquee.box.x, top: marquee.box.y, width: marquee.box.w, height: marquee.box.h }}
            />
          )}
          {drives.isLoading ? (
            <div className="grid gap-2 pt-4">
              {[0, 1, 2].map((i) => (
                <Skeleton key={i} className="h-16" />
              ))}
            </div>
          ) : view === "list" ? (
            <div className="pt-2">{listView}</div>
          ) : (
            <>
              <Section title={t("Personal")} count={personal.length}>
                <div className={grid}>{personal.map((d) => driveTile(d))}</div>
              </Section>
              <Section title={t("Shared spaces")} count={common.length}>
                <div className={grid}>
                  {common.map((d) => driveTile(d))}
                  {common.length === 0 && <p className="px-3 text-xs text-muted-foreground">{t("You haven't joined any shared spaces yet.")}</p>}
                </div>
              </Section>
              {sharedItems.length > 0 && (
                <Section title={t("Shared with me")} count={sharedItems.length}>
                  <div className={grid}>{sharedItems.map((item) => sharedTile(item))}</div>
                </Section>
              )}
            </>
          )}
        </ContextMenuTrigger>
        <ContextMenuContent>
          {selItems.length > 1 ? (
            <>
              <div className="px-2 py-1 text-xs text-muted-foreground">{t("{n} item selected|{n} items selected", { n: selItems.length })}</div>
              <DropdownMenuItem onClick={() => selItems.forEach((s) => openSel(s, true))}>
                <PanelTopIcon /> {t("Open all in new tabs")}
              </DropdownMenuItem>
            </>
          ) : sel?.t === "drive" ? (
            driveMenu(sel.drive)
          ) : sel?.t === "shared" ? (
            <>
              <DropdownMenuItem onClick={() => openSel(sel)}>
                <FolderOpenIcon /> {t("Open")}
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => openSel(sel, true)}>
                <PanelTopIcon /> {t("Open in new tab")}
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem onClick={() => setDialog({ t: "access", nodeId: sel.item.id })}>
                <UsersRoundIcon /> {t("Access ({role})", { role: ROLE_LABEL[sel.item.role] })}
              </DropdownMenuItem>
            </>
          ) : (
            <>
              <DropdownMenuItem disabled={!me.can_create_drive} onClick={() => setDialog({ t: "create" })}>
                <CirclePlusIcon /> {t("New space")}
              </DropdownMenuItem>
              <DropdownMenuItem
                onClick={() => {
                  qc.invalidateQueries({ queryKey: ["drives"] });
                  qc.invalidateQueries({ queryKey: ["shared-with-me"] });
                }}
              >
                <RefreshCwIcon /> {t("Refresh")}
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem onClick={() => setView("tiles")}>
                <Grid2X2Icon /> {t("Tiles")}{view === "tiles" && " ✓"}
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => setView("list")}>
                <ListIcon /> {t("Details")}{view === "list" && " ✓"}
              </DropdownMenuItem>
            </>
          )}
        </ContextMenuContent>
      </ContextMenu>

      {dialog?.t === "access" && <AccessDialog nodeId={dialog.nodeId} onClose={() => setDialog(null)} />}
      {dialog?.t === "create" && (
        <CreateDriveDialog
          onClose={() => setDialog(null)}
          onCreated={(d) => {
            refresh();
            selectOnly({ t: "drive", drive: d });
          }}
        />
      )}
      {dialog?.t === "delete" && (
        <ConfirmDialog
          title={t("Delete space \"{name}\"?", { name: dialog.drive.name })}
          description={t("All files in this space ({size}) will be permanently deleted and no member will be able to access them. This can't be undone.", { size: formatBytes(dialog.drive.used_bytes) })}
          confirmText={t("Delete permanently")}
          destructive
          onClose={() => setDialog(null)}
          onConfirm={async () => {
            await api.deleteDrive(dialog.drive.id);
            toast.success(t("Space deleted"));
            setDialog(null);
            selectOnly(null);
            invalidateFiles(qc, "admin-drives");
          }}
        />
      )}
      {dialog?.t === "members" && (
        <AccessDialog
          nodeId={dialog.drive.root_id}
          onClose={() => {
            setDialog(null);
            refresh();
          }}
        />
      )}
      {dialog?.t === "props" && <DrivePropsDialog drive={dialog.drive} onClose={() => setDialog(null)} />}
    </Frame>
  );
}

function CreateDriveDialog({ onClose, onCreated }: { onClose(): void; onCreated(d: Drive): void }) {
  const me = useMe();
  const [name, setName] = useState("");
  const [quota, setQuota] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent>
        <form
          className="grid gap-4"
          onSubmit={async (e) => {
            e.preventDefault();
            setBusy(true);
            setError(null);
            try {
              const d = await api.createDrive(name.trim(), quota ? Math.round(Number(quota) * GB) : 0);
              toast.success(t("Space created. You can now invite members"));
              onCreated(d);
              onClose();
            } catch (err) {
              setError(err instanceof Error ? err.message : t("Couldn't create"));
            } finally {
              setBusy(false);
            }
          }}
        >
          <DialogHeader>
            <DialogTitle>{t("New team space")}</DialogTitle>
          </DialogHeader>
          <div className="grid gap-2">
            <Label htmlFor="drive-name">{t("Name")}</Label>
            <Input id="drive-name" value={name} onChange={(e) => setName(e.target.value)} placeholder={tc("example", "Marketing")} autoFocus />
            {me.role === "admin" && (
              <>
                <Label htmlFor="drive-quota">{t("Quota (GB, leave blank for unlimited)")}</Label>
                <Input
                  id="drive-quota"
                  type="number"
                  min={0}
                  step="0.1"
                  value={quota}
                  onChange={(e) => setQuota(e.target.value)}
                  placeholder={t("Unlimited")}
                />
              </>
            )}
            <p className="text-xs text-muted-foreground">{t("You'll be the owner of this space. After creating it, you can invite users or groups.")}</p>
            <ErrorText>{error}</ErrorText>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={busy || !name.trim()}>
              {t("Create")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function DrivePropsDialog({ drive, onClose }: { drive: Drive; onClose(): void }) {
  const me = useMe();
  const Icon = DRIVE_ICON[drive.kind];
  const canSeeActivity = atLeast(drive.role, "manager") || (me.role === "admin" && drive.kind !== "personal") || drive.kind === "personal";
  const rows: [string, string][] = [
    [t("Type"), DRIVE_KIND_LABEL[drive.kind]],
    [t("Owner"), drive.kind === "company" ? t("Company") : drive.owner_name],
    [t("Used"), formatBytes(drive.used_bytes)],
    [t("Quota"), drive.quota_bytes ? formatBytes(drive.quota_bytes) : t("Unlimited")],
    [t("Members"), drive.kind === "personal" ? t("Owner only") : t("{n} permission|{n} permissions", { n: drive.member_count })],
    [t("My role"), drive.role ? ROLE_LABEL[drive.role] : "—"],
  ];
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2 pr-8">
            <Icon className="size-5" /> {drive.name}
          </DialogTitle>
        </DialogHeader>
        <dl className="grid grid-cols-[auto_minmax(0,1fr)_auto_minmax(0,1fr)] gap-x-4 gap-y-2 text-[13px]">
          {rows.map(([k, v]) => (
            <div key={k} className="contents">
              <dt className="text-muted-foreground">{k}</dt>
              <dd className="truncate">{v}</dd>
            </div>
          ))}
        </dl>
        {canSeeActivity && (
          <div className="grid gap-2">
            <div className="text-xs font-medium text-muted-foreground">{t("Recent activity")}</div>
            <ActivityLog driveId={drive.id} compact className="h-80 rounded-md border" />
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
