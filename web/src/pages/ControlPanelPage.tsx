import { useState, type MouseEvent } from "react";
import { useNavigate, useSearchParams } from "react-router";
import { useQuery } from "@tanstack/react-query";
import { ChevronDownIcon, FolderOpenIcon, Grid2X2Icon, ListIcon, PanelTopIcon, RefreshCwIcon, SettingsIcon } from "lucide-react";
import { api } from "@/api";
import { Button } from "@/components/ui/button";
import { ContextMenu, ContextMenuContent, ContextMenuTrigger } from "@/components/ui/context-menu";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { Frame, ToolButton, ToolSeparator } from "@/components/Frame";
import {
  CONTROL_PANEL_CATEGORIES,
  CONTROL_PANEL_ITEMS,
  controlPanelCategoryLabel,
  type ControlPanelItem,
  type ControlPanelKey,
} from "@/lib/controlPanel";
import { t } from "@/lib/i18n";
import { usePersisted } from "@/lib/session";
import { cn, formatBytes } from "@/lib/utils";
import { useTabActions } from "@/tabs";

/** Control panel: like an OS's settings, lists every admin item as an icon, operated the same way as the file explorer */
export function ControlPanelPage() {
  const navigate = useNavigate();
  const tabs = useTabActions();
  const [view, setView] = usePersisted<"tiles" | "list">("control-panel-view", "tiles");
  const [sel, setSel] = useState<ControlPanelKey | null>(null);
  const [params, setParams] = useSearchParams();
  const [query, setQuery] = useState(params.get("q") ?? "");
  const search = (v: string) => {
    setQuery(v);
    setParams(v ? { q: v } : {}, { replace: true });
  };
  const [collapsed, setCollapsed] = useState<string[]>([]);
  const system = useQuery({ queryKey: ["system"], queryFn: api.systemSettings });
  const locations = useQuery({ queryKey: ["storage-locations"], queryFn: api.storageLocations });
  const drives = useQuery({ queryKey: ["admin-drives"], queryFn: api.adminDrives });
  // The company space can be renamed, so the summary uses its current name
  const company = drives.data?.find((d) => d.kind === "company")?.name ?? t("All files");

  const s = system.data?.stats;
  const defaultLocation = locations.data?.find((l) => l.is_default);
  const summary: Partial<Record<ControlPanelKey, string>> = s
    ? {
        users: t("{n} user|{n} users", { n: s.users }),
        groups: t("{n} group|{n} groups", { n: s.groups }),
        shares: system.data?.public_links ? t("{n} share link|{n} share links", { n: s.share_links }) : t("Public links turned off"),
        drives: t("{n} team space|{n} team spaces", { n: s.team_drives }),
        storage: locations.data
          ? t("{n} location · Default: {name}|{n} locations · Default: {name}", { n: locations.data.length, name: defaultLocation?.name ?? "—" })
          : undefined,
        usage: t("{size} used on disk", { size: formatBytes(s.stored_bytes) }),
        general: system.data?.shared_enabled ? t("\"{name}\" enabled", { name: company }) : t("\"{name}\" disabled", { name: company }),
      }
    : {};

  const q = query.trim().toLowerCase();
  const items = q
    ? CONTROL_PANEL_ITEMS.filter((i) => `${i.title} ${i.desc} ${i.keywords} ${controlPanelCategoryLabel(i.category)}`.toLowerCase().includes(q))
    : CONTROL_PANEL_ITEMS;
  const selected = items.find((i) => i.key === sel) ?? null;

  const open = (item: ControlPanelItem | null, newTab = false) => {
    if (!item) return;
    if (newTab) tabs.open(item.to, { reuse: true });
    else navigate(item.to);
  };

  const itemProps = (item: ControlPanelItem) => ({
    "data-item": true,
    onClick: (e: MouseEvent) => {
      e.stopPropagation();
      setSel(item.key);
    },
    onDoubleClick: () => open(item),
    onKeyDown: (e: React.KeyboardEvent) => e.key === "Enter" && open(item),
    onAuxClick: (e: MouseEvent) => {
      if (e.button === 1) {
        e.preventDefault();
        open(item, true);
      }
    },
    onMouseDown: (e: MouseEvent) => e.button === 1 && e.preventDefault(),
    onContextMenu: () => setSel(item.key),
    tabIndex: 0,
  });

  const toolbar = (
    <>
      <ToolButton
        icon={FolderOpenIcon}
        label={t("Open")}
        showLabel
        className="h-9 px-2.5 text-[13px]"
        disabled={!selected}
        onClick={() => open(selected)}
      />
      <ToolButton icon={PanelTopIcon} label={t("Open in new tab")} disabled={!selected} onClick={() => open(selected, true)} />
      <ToolSeparator />
      <ToolButton
        icon={RefreshCwIcon}
        label={t("Refresh")}
        onClick={() => {
          system.refetch();
          locations.refetch();
        }}
      />
      <div className="flex-1" />
      <Button variant={view === "tiles" ? "secondary" : "ghost"} aria-pressed={view === "tiles"} size="icon-sm" aria-label={t("Large icons")} title={t("Large icons")} onClick={() => setView("tiles")}>
        <Grid2X2Icon />
      </Button>
      <Button variant={view === "list" ? "secondary" : "ghost"} aria-pressed={view === "list"} size="icon-sm" aria-label={t("Details")} title={t("Details")} onClick={() => setView("list")}>
        <ListIcon />
      </Button>
    </>
  );

  // Generated by a function (rather than defining a component inside the component), so re-renders keep the same DOM element and double-click fires
  const tile = (item: ControlPanelItem) => (
    <div
      key={item.key}
      {...itemProps(item)}
      className={cn(
        "flex cursor-default gap-3 rounded-md border border-transparent p-3 outline-none select-none hover:bg-muted/70 focus-visible:ring-2 focus-visible:ring-ring",
        sel === item.key && "border-brand bg-selection hover:bg-selection",
      )}
    >
      <span className={cn("flex size-11 shrink-0 items-center justify-center rounded-lg", item.tone)}>
        <item.icon className="size-6 stroke-[1.6]" />
      </span>
      <div className="min-w-0 flex-1">
        <div className="truncate text-[13px] font-medium">{item.title}</div>
        <div className="line-clamp-2 text-xs leading-snug text-muted-foreground">{item.desc}</div>
        {summary[item.key] && <div className="mt-1 truncate text-[11px] text-brand">{summary[item.key]}</div>}
      </div>
    </div>
  );

  const listView = (
    <table className="w-full table-fixed border-collapse text-xs whitespace-nowrap">
      <thead>
        <tr className="border-b text-left text-muted-foreground">
          <th className="h-[30px] w-[180px] px-2 pl-3 font-normal">{t("Name")}</th>
          <th className="h-[30px] px-2 font-normal">{t("Description")}</th>
          <th className="h-[30px] w-[110px] px-2 font-normal max-md:hidden">{t("Category")}</th>
          <th className="h-[30px] w-[200px] px-2 font-normal max-lg:hidden">{t("Status")}</th>
        </tr>
      </thead>
      <tbody>
        {items.map((item) => (
          <tr
            key={item.key}
            {...itemProps(item)}
            className={cn("h-8 cursor-default outline-none hover:bg-muted/70", sel === item.key && "bg-selection shadow-[inset_3px_0_0_var(--color-brand)] hover:bg-selection")}
          >
            <td className="truncate px-2 pl-3">
              <span className="flex items-center gap-2">
                <span className={cn("flex size-5 shrink-0 items-center justify-center rounded", item.tone)}>
                  <item.icon className="size-3.5" />
                </span>
                {item.title}
              </span>
            </td>
            <td className="truncate px-2 text-muted-foreground">{item.desc}</td>
            <td className="truncate px-2 text-muted-foreground max-md:hidden">{controlPanelCategoryLabel(item.category)}</td>
            <td className="truncate px-2 text-muted-foreground max-lg:hidden">{summary[item.key] ?? ""}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );

  const grid = "grid grid-cols-[repeat(auto-fill,minmax(260px,1fr))] gap-1.5";

  return (
    <Frame
      toolbar={toolbar}
      icon={SettingsIcon}
      crumbs={[{ label: t("Control panel") }]}
      upTo={null}
      searchPlaceholder={t("Search settings")}
      onSearch={search}
      footer={<span>{selected ? t("\"{name}\" selected", { name: selected.title }) : t("{n} item|{n} items", { n: items.length })}</span>}
    >
      <ContextMenu>
        <ContextMenuTrigger
          className="min-h-0 flex-1 overflow-auto px-4 pb-6"
          onClick={() => setSel(null)}
          onContextMenuCapture={(e) => !(e.target as HTMLElement).closest("[data-item]") && setSel(null)}
        >
          {items.length === 0 ? (
            <p className="pt-10 text-center text-xs text-muted-foreground">{t("No settings match \"{query}\".", { query })}</p>
          ) : view === "list" ? (
            <div className="pt-2">{listView}</div>
          ) : (
            CONTROL_PANEL_CATEGORIES.map(({ id: cat, label }) => {
              const inCat = items.filter((i) => i.category === cat);
              if (inCat.length === 0) return null;
              const isOpen = !collapsed.includes(cat);
              return (
                <section key={cat}>
                  <button
                    type="button"
                    onClick={(e) => {
                      e.stopPropagation();
                      setCollapsed(isOpen ? [...collapsed, cat] : collapsed.filter((c) => c !== cat));
                    }}
                    className="mt-4 mb-2 flex items-center gap-1.5 text-xs text-muted-foreground hover:text-foreground"
                  >
                    <ChevronDownIcon className={cn("size-3.5 transition-transform", !isOpen && "-rotate-90")} />
                    {t("{label} ({n})", { label, n: inCat.length })}
                  </button>
                  {isOpen && <div className={grid}>{inCat.map(tile)}</div>}
                </section>
              );
            })
          )}
        </ContextMenuTrigger>
        <ContextMenuContent>
          {selected ? (
            <>
              <DropdownMenuItem onClick={() => open(selected)}>
                <FolderOpenIcon /> {t("Open")}
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => open(selected, true)}>
                <PanelTopIcon /> {t("Open in new tab")}
              </DropdownMenuItem>
            </>
          ) : (
            <>
              <DropdownMenuItem onClick={() => setView("tiles")}>
                <Grid2X2Icon /> {t("Large icons")}
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => setView("list")}>
                <ListIcon /> {t("Details")}
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem
                onClick={() => {
                  system.refetch();
                  locations.refetch();
                }}
              >
                <RefreshCwIcon /> {t("Refresh")}
              </DropdownMenuItem>
            </>
          )}
        </ContextMenuContent>
      </ContextMenu>
    </Frame>
  );
}
