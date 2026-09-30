import { useState, type KeyboardEvent, type MouseEvent, type ReactNode } from "react";
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
import { Skeleton } from "@/components/ui/skeleton";
import { AccessDialog } from "@/components/AccessDialog";
import { ConfirmDialog } from "@/components/dialogs";
import { ErrorState } from "@/components/ErrorState";
import { InlineRename } from "@/components/InlineRename";
import { MarqueeBox, useMarquee } from "@/components/useMarquee";
import { FileIcon } from "@/components/FileIcon";
import { Frame, ToolButton, ToolSeparator } from "@/components/Frame";
import { CreateDriveDialog, DrivePropsDialog } from "@/components/DriveDialogs";
import { DRIVE_ICON, DRIVE_KIND_LABEL, ROLE_LABEL, atLeast, useDrives } from "@/lib/drives";
import { usePersisted, useMe } from "@/lib/session";
import { cn, formatBytes } from "@/lib/utils";
import { t, tServer, tc } from "@/lib/i18n";
import { invalidateFiles } from "@/lib/queries";
import { useTabActions } from "@/tabs";

type Item = { t: "drive"; drive: Drive } | { t: "shared"; item: SharedItem };
type Selection = Item | null;

/** Key used for selection (space and shared item ids may collide, so add a prefix) */
const keyOf = (s: Item) => (s.t === "drive" ? `d:${s.drive.id}` : `s:${s.item.id}`);
type SectionId = "personal" | "common" | "shared";
const sectionOf = (s: Item): SectionId => (s.t === "shared" ? "shared" : s.drive.kind === "personal" ? "personal" : "common");
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

function Section({ title, count, open, onToggle, children }: { title: string; count: number; open: boolean; onToggle(): void; children: ReactNode }) {
  return (
    <section>
      <button
        type="button"
        aria-expanded={open}
        onClick={onToggle}
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
  // Collapsed sections of the tiles view
  const [collapsed, setCollapsed] = useState<Set<SectionId>>(new Set());

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

  /** Keyboard (like the file list): arrows move the selection (Shift extends it), Space selects (toggles with Ctrl), Home/End jump, Enter opens */
  const keyNav = (e: KeyboardEvent<HTMLElement>, s: Item) => {
    // Keys typed in the rename box belong to it
    if (e.target !== e.currentTarget) return;
    if (e.key === "Enter" && !e.altKey && !e.repeat) {
      e.preventDefault();
      openSel(s);
      return;
    }
    if (e.key === " ") {
      e.preventDefault();
      if (e.ctrlKey || e.metaKey) {
        const next = new Set(selected);
        if (next.has(keyOf(s))) next.delete(keyOf(s));
        else next.add(keyOf(s));
        setSelected(next);
        setAnchor(keyOf(s));
      } else selectOnly(s);
      return;
    }
    // Work on the items as laid out (collapsed sections aren't there): each tiles section is its own grid
    const el = e.currentTarget;
    const all = Array.from(el.closest("[data-spaces]")?.querySelectorAll<HTMLElement>("[data-node-id]") ?? []);
    const groups: HTMLElement[][] = [];
    for (const x of all) {
      if (groups.at(-1)?.[0].parentElement === x.parentElement) groups.at(-1)!.push(x);
      else groups.push([x]);
    }
    const g = groups.findIndex((group) => group.includes(el));
    const group = groups[g];
    const i = group.indexOf(el);
    const perRow = view === "tiles" ? Math.max(1, getComputedStyle(el.parentElement!).gridTemplateColumns.split(" ").filter(Boolean).length) : 1;
    const col = i % perRow;
    let target: HTMLElement | undefined;
    if (e.key === "ArrowDown") {
      if (i + perRow < group.length) target = group[i + perRow];
      // Below is empty but there is a shorter last row: go to its last item
      else if (Math.floor(i / perRow) < Math.floor((group.length - 1) / perRow)) target = group.at(-1);
      else if (groups[g + 1]) target = groups[g + 1][Math.min(col, groups[g + 1].length - 1)];
    } else if (e.key === "ArrowUp") {
      if (i - perRow >= 0) target = group[i - perRow];
      else if (groups[g - 1]) {
        const prev = groups[g - 1];
        target = prev[Math.min(Math.floor((prev.length - 1) / perRow) * perRow + col, prev.length - 1)];
      }
    } else if (e.key === "ArrowRight" && view === "tiles") target = all[all.indexOf(el) + 1];
    else if (e.key === "ArrowLeft" && view === "tiles") target = all[all.indexOf(el) - 1];
    else if (e.key === "Home") target = all[0];
    else if (e.key === "End") target = all.at(-1);
    else return;
    e.preventDefault();
    const next = target && ordered.find((x) => keyOf(x) === target.dataset.nodeId);
    if (!target || !next) return;
    const a = anchor === null ? -1 : ordered.findIndex((x) => keyOf(x) === anchor);
    if (e.shiftKey && a >= 0) {
      const b = ordered.indexOf(next);
      const range = new Set<string>();
      for (let j = Math.min(a, b); j <= Math.max(a, b); j++) range.add(keyOf(ordered[j]));
      setSelected(range);
    } else {
      selectOnly(next);
    }
    target.focus();
  };

  // One item is reachable with Tab (the first selected one shown, else the first one shown); arrows move between the others
  const shown = view === "list" ? ordered : ordered.filter((s) => !collapsed.has(sectionOf(s)));
  const tabStop = (shown.find((s) => selected.has(keyOf(s))) ?? shown[0]) as Item | undefined;

  const itemProps = (s: Item) => ({
    "data-node-id": keyOf(s),
    "aria-selected": selected.has(keyOf(s)),
    tabIndex: tabStop && keyOf(tabStop) === keyOf(s) ? 0 : -1,
    onKeyDown: (e: KeyboardEvent<HTMLElement>) => keyNav(e, s),
    onClick: (e: MouseEvent) => {
      e.stopPropagation();
      // Rows start a marquee on mousedown, which keeps them from getting the focus: arrows continue from the clicked item
      (e.currentTarget as HTMLElement).focus({ preventScroll: true });
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
        role="option"
        className={cn(
          "flex cursor-default gap-3 rounded-md border border-transparent p-3 outline-none select-none hover:bg-muted/70 focus-visible:ring-2 focus-visible:ring-ring",
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
        role="option"
        className={cn(
          "flex cursor-default gap-3 rounded-md border border-transparent p-3 outline-none select-none hover:bg-muted/70 focus-visible:ring-2 focus-visible:ring-ring",
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

  const section = (id: SectionId, title: string, count: number) => ({
    title,
    count,
    open: !collapsed.has(id),
    onToggle: () => {
      const next = new Set(collapsed);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      setCollapsed(next);
    },
  });
  // Each section's tiles are a list of options (an empty section only shows its note)
  const tiles = (label: string, count: number) => ({
    className: "grid grid-cols-[repeat(auto-fill,minmax(250px,1fr))] gap-1.5",
    ...(count > 0 ? { role: "listbox", "aria-multiselectable": true, "aria-label": label } : {}),
  });

  const listView = (
    <table
      role="grid"
      aria-multiselectable
      aria-label={t("All spaces")}
      aria-rowcount={ordered.length + 1}
      className="w-full table-fixed border-collapse text-xs whitespace-nowrap"
    >
      <thead>
        <tr role="row" aria-rowindex={1} className="border-b text-left text-muted-foreground">
          {[t("Name"), t("Type"), t("Used"), t("Total size"), t("My role"), t("Owner")].map((h, i) => (
            <th
              key={h}
              role="columnheader"
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
        {list.map((d, i) => {
          const Icon = DRIVE_ICON[d.kind];
          const s = { t: "drive" as const, drive: d };
          return (
            <tr
              key={d.id}
              {...itemProps(s)}
              data-item
              role="row"
              aria-rowindex={i + 2}
              className={cn(
                "h-7 cursor-default outline-none hover:bg-muted/70 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset",
                isSel(s) && "bg-selection shadow-[inset_3px_0_0_var(--color-brand)] hover:bg-selection",
              )}
            >
              <td role="gridcell" className="truncate px-2 pl-3">
                <span className="flex items-center gap-2">
                  <Icon className={cn("size-4 shrink-0", d.offline && "opacity-50")} /> {isRenaming(d) ? renameBox(d) : d.name}
                  <OfflineBadge d={d} />
                </span>
              </td>
              <td role="gridcell" className="px-2 text-muted-foreground max-md:hidden">{DRIVE_KIND_LABEL[d.kind]}</td>
              <td role="gridcell" className="px-2 text-muted-foreground">{formatBytes(d.used_bytes)}</td>
              <td role="gridcell" className="px-2 text-muted-foreground">{d.quota_bytes ? formatBytes(d.quota_bytes) : tc("short", "Unlimited")}</td>
              <td role="gridcell" className="px-2 text-muted-foreground">{d.role ? ROLE_LABEL[d.role] : "—"}</td>
              <td role="gridcell" className="truncate px-2 text-muted-foreground max-md:hidden">{d.kind === "company" ? t("Company") : d.owner_name}</td>
            </tr>
          );
        })}
        {sharedItems.map((item, i) => {
          const s = { t: "shared" as const, item };
          return (
            <tr
              key={item.id}
              {...itemProps(s)}
              data-item
              role="row"
              aria-rowindex={list.length + i + 2}
              className={cn(
                "h-7 cursor-default outline-none hover:bg-muted/70 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset",
                isSel(s) && "bg-selection shadow-[inset_3px_0_0_var(--color-brand)] hover:bg-selection",
              )}
            >
              <td role="gridcell" className="truncate px-2 pl-3">
                <span className="flex items-center gap-2">
                  <FileIcon node={item} className="size-4" /> {item.name}
                </span>
              </td>
              <td role="gridcell" className="px-2 text-muted-foreground max-md:hidden">{t("Shared with me")}</td>
              <td role="gridcell" className="px-2 text-muted-foreground">{item.kind === "file" ? formatBytes(item.size) : ""}</td>
              <td role="gridcell" className="px-2 text-muted-foreground" />
              <td role="gridcell" className="px-2 text-muted-foreground">{ROLE_LABEL[item.role]}</td>
              <td role="gridcell" className="truncate px-2 text-muted-foreground max-md:hidden">{item.sharer}</td>
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
      crumbs={[{ label: t("All spaces"), virtual: true }]}
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
          data-spaces
          onClick={() => selectOnly(null)}
          onContextMenuCapture={(e) => !(e.target as HTMLElement).closest("[data-item]") && selectOnly(null)}
          {...marquee.containerProps}
        >
          <div role="status" className="sr-only">
            {selItems.length > 0 ? t("{n} item selected|{n} items selected", { n: selItems.length }) : ""}
          </div>
          <MarqueeBox store={marquee.box} />
          {drives.isLoading ? (
            <div className="grid gap-2 pt-4">
              {[0, 1, 2].map((i) => (
                <Skeleton key={i} className="h-16" />
              ))}
            </div>
          ) : drives.error && !drives.data ? (
            <ErrorState message={drives.error.message} onRetry={() => drives.refetch()} />
          ) : view === "list" ? (
            <div className="pt-2">{listView}</div>
          ) : (
            <>
              {/* Someone without "My files" has no personal section */}
              {personal.length > 0 && (
                <Section {...section("personal", t("Personal"), personal.length)}>
                  <div {...tiles(t("Personal"), personal.length)}>{personal.map((d) => driveTile(d))}</div>
                </Section>
              )}
              <Section {...section("common", t("Shared spaces"), common.length)}>
                <div {...tiles(t("Shared spaces"), common.length)}>
                  {common.map((d) => driveTile(d))}
                  {common.length === 0 && <p className="px-3 text-xs text-muted-foreground">{t("You haven't joined any shared spaces yet.")}</p>}
                </div>
              </Section>
              {sharedItems.length > 0 && (
                <Section {...section("shared", t("Shared with me"), sharedItems.length)}>
                  <div {...tiles(t("Shared with me"), sharedItems.length)}>{sharedItems.map((item) => sharedTile(item))}</div>
                </Section>
              )}
            </>
          )}
          {/* The spaces showed, but what was shared with me couldn't be loaded */}
          {drives.data && shared.error && !shared.data && (
            <ErrorState compact className="mt-4" message={shared.error.message} onRetry={() => shared.refetch()} />
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
          description={
            dialog.drive.mode === "folder"
              ? t("The space is removed from ThirtyFile and no member will be able to access it. Its folder on the server is kept with the files in it ({size}), for an administrator to delete.", {
                  size: formatBytes(dialog.drive.used_bytes),
                })
              : t("All files in this space ({size}) will be permanently deleted and no member will be able to access them. This can't be undone.", { size: formatBytes(dialog.drive.used_bytes) })
          }
          confirmText={t("Delete permanently")}
          destructive
          irreversible
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
