/** Phones: while items are selected, their main actions sit in a bar below the list, within reach of the thumb */
import { useLayoutEffect, useRef, type ReactNode } from "react";
import { DownloadIcon, EllipsisVerticalIcon, FolderInputIcon, Share2Icon, SquareCheckIcon, Trash2Icon, XIcon, type LucideIcon } from "lucide-react";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { t } from "@/lib/i18n";
import type { ExplorerProps } from "../Explorer";
import type { ExplorerState } from "./state";
import type { ExplorerActions } from "./actions";

function BarButton({ icon: Icon, label, ...props }: { icon: LucideIcon; label: string } & React.ComponentProps<"button">) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      className="flex min-w-11 flex-col items-center gap-0.5 rounded-md px-1.5 py-1 text-[11px] text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-40"
      {...props}
    >
      <Icon className="size-5" />
      <span className="max-w-16 truncate">{label}</span>
    </button>
  );
}

export function SelectionBar({ p, s, a, menuItems }: { p: ExplorerProps; s: ExplorerState; a: ExplorerActions; menuItems: ReactNode }) {
  const { caps, selectedNodes, selectedIds, single, setSelected, setDialog } = s;
  const all = selectedNodes.length === p.items.length;
  // Transfer panels and messages move up while the bar shows, so they don't cover it
  const ref = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const style = document.documentElement.style;
    const place = () => ref.current && style.setProperty("--tf-bottom-inset", `${Math.max(0, window.innerHeight - ref.current.getBoundingClientRect().top)}px`);
    place();
    window.addEventListener("resize", place);
    return () => {
      window.removeEventListener("resize", place);
      style.removeProperty("--tf-bottom-inset");
    };
  }, []);
  return (
    <div
      ref={ref}
      role="toolbar" aria-label={t("Selected items")} className="flex shrink-0 items-center gap-1 border-t bg-background px-1.5 py-1 shadow-[0_-2px_6px_rgb(0_0_0/0.06)]">
      <button
        type="button"
        aria-label={t("Select none")}
        title={t("Select none")}
        className="flex size-9 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-muted"
        onClick={() => setSelected(new Set())}
      >
        <XIcon className="size-5" />
      </button>
      <span className="min-w-0 flex-1 truncate text-xs">{t("{n} selected", { n: selectedNodes.length })}</span>
      <BarButton icon={DownloadIcon} label={t("Download")} onClick={() => a.download(selectedIds)} />
      {/* A share link where the role allows one, else sharing with people */}
      <BarButton
        icon={Share2Icon}
        label={t("Share")}
        disabled={!single}
        onClick={() => single && setDialog(caps.share ? { t: "share", node: single } : { t: "access", nodeId: single.id })}
      />
      <BarButton icon={FolderInputIcon} label={t("Move")} disabled={!caps.write} onClick={() => setDialog({ t: "move", ids: selectedIds })} />
      <BarButton icon={Trash2Icon} label={t("Delete")} disabled={!caps.del} onClick={() => setDialog({ t: "trash", ids: selectedIds })} />
      <DropdownMenu>
        <DropdownMenuTrigger render={<BarButton icon={EllipsisVerticalIcon} label={t("More")} />} />
        <DropdownMenuContent side="top" align="end" className="w-56">
          {!all && (
            <>
              <DropdownMenuItem onClick={() => setSelected(new Set(p.items.map((n) => n.id)))}>
                <SquareCheckIcon /> {t("Select all")}
              </DropdownMenuItem>
              <DropdownMenuSeparator />
            </>
          )}
          {menuItems}
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}
