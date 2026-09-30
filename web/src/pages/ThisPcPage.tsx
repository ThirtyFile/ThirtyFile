import { useState, type ReactNode } from "react";
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
import { useSelectableList } from "@/lib/listSelection";
import { usePersisted, useMe } from "@/lib/session";
import { cn, formatBytes } from "@/lib/utils";
import { t, tServer, tc } from "@/lib/i18n";
import { refreshFiles } from "@/lib/queries";
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
  // Selecting with the mouse and the keyboard, like the file list (lib/listSelection.ts): Ctrl and Shift, arrows (across
  // the rows of tiles and from one section to the next), Home/End, Space, Ctrl+A, Esc, Enter, typing a name
  const nav = useSelectableList({
    items: ordered,
    keyOf,
    selected,
    onSelect: setSelected,
    multi: true,
    onOpen: (s, newTab) => openSel(s, newTab),
    nameOf: (s) => (s.t === "drive" ? s.drive.name : s.item.name),
    hidden: (s) => view !== "list" && collapsed.has(sectionOf(s)),
  });
  const selectOnly = (s: Selection) => nav.selectOnly(s ? keyOf(s) : null);
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

  const itemProps = (s: Item) => ({ "data-node-id": keyOf(s), ...nav.itemProps(s) });

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
    ...(count > 0 ? nav.listProps("listbox", label) : {}),
  });

  const listView = (
    <table
      {...nav.listProps("grid", t("All spaces"))}
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
          {...nav.scopeProps}
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
            void refreshFiles(qc, { spaces: [dialog.drive.id] });
            void qc.invalidateQueries({ queryKey: ["admin-drives"] });
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
