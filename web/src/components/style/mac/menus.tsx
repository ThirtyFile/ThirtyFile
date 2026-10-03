/**
 * The Mac style's context menu, in Finder's order: on items, opening them first, then the trash, then Get info and
 * Rename, the clipboard, sharing, favourites and tags; on empty space, New and Upload, pasting, Get info, the view and
 * how items are sorted and grouped
 */
import { Fragment, type ReactNode } from "react";
import { ArrowDownUpIcon, CopyIcon, EyeIcon, GroupIcon, InfoIcon, LayoutListIcon, PencilIcon, Trash2Icon, Undo2Icon } from "lucide-react";
import { DropdownMenuItem, DropdownMenuSub, DropdownMenuSubContent, DropdownMenuSubTrigger } from "@/components/ui/dropdown-menu";
import { sections, type MenuEntries } from "@/components/explorer/menus";
import { Kbd } from "@/components/explorer/ui";
import { GroupChoices, SortChoices, ViewChoices } from "@/components/explorer/viewChoices";
import { t } from "@/lib/i18n";
import { undoLast } from "@/lib/undo";
import { openQuickLook } from "./quickLook";

export function macMenu(m: MenuEntries): ReactNode {
  const { p, s, a } = m;
  const { caps, single, canCreate } = s;
  const k = s.kit.keys;
  const getInfo = (key: string) => (
    <DropdownMenuItem key={key} onClick={() => s.setDetailsOpen(true)}>
      <InfoIcon /> {t("Get info")} <Kbd>{k.details[0]}</Kbd>
    </DropdownMenuItem>
  );
  if (s.count)
    return sections(
      [
        m.openItem,
        m.openInNewTab,
        m.openLocation,
        <DropdownMenuItem key="look" onClick={openQuickLook}>
          <EyeIcon /> {t("Quick look")} <Kbd>{k.quickLook[0]}</Kbd>
        </DropdownMenuItem>,
      ],
      [
        caps.del && (
          <DropdownMenuItem key="trash" variant="destructive" onClick={m.remove}>
            <Trash2Icon /> {t("Move to trash")} <Kbd>{k.trash[0]}</Kbd>
          </DropdownMenuItem>
        ),
      ],
      [
        getInfo("info"),
        caps.write && single && (
          <DropdownMenuItem key="rename" onClick={m.rename}>
            <PencilIcon /> {t("Rename")} <Kbd>{k.rename[0]}</Kbd>
          </DropdownMenuItem>
        ),
        m.compressItem,
        m.extractItem,
        m.downloadItem,
      ],
      [
        <DropdownMenuItem key="copy" onClick={a.copy}>
          <CopyIcon /> {t("Copy")} <Kbd>{k.copy[0]}</Kbd>
        </DropdownMenuItem>,
        m.moveTo,
        m.copyTo,
      ],
      [m.shareWith, m.shareLink],
      [m.favorite, m.tags],
    );
  // What can be made here, pasting, then how the list shows
  return sections(
    [canCreate && <Fragment key="new">{m.newItems}</Fragment>],
    [
      m.pasteItem,
      s.undoLabel && (
        <DropdownMenuItem key="undo" onClick={() => undoLast()}>
          <Undo2Icon /> {s.undoLabel} <Kbd>{k.undo[0]}</Kbd>
        </DropdownMenuItem>
      ),
    ],
    [getInfo("properties")],
    [
      <DropdownMenuSub key="view">
        <DropdownMenuSubTrigger>
          <LayoutListIcon /> {t("View")}
        </DropdownMenuSubTrigger>
        <DropdownMenuSubContent className="w-48">
          <ViewChoices view={s.view} onChange={s.setView} />
        </DropdownMenuSubContent>
      </DropdownMenuSub>,
      p.sort && p.onSortChange && (
        <DropdownMenuSub key="sort">
          <DropdownMenuSubTrigger>
            <ArrowDownUpIcon /> {t("Sort by")}
          </DropdownMenuSubTrigger>
          <DropdownMenuSubContent className="w-44">
            <SortChoices sort={p.sort} onChange={p.onSortChange} />
          </DropdownMenuSubContent>
        </DropdownMenuSub>
      ),
      <DropdownMenuSub key="group">
        <DropdownMenuSubTrigger disabled={s.view === "columns"}>
          <GroupIcon /> {t("Group by")}
        </DropdownMenuSubTrigger>
        <DropdownMenuSubContent className="w-44">
          <GroupChoices groupBy={s.groupBy} onChange={s.setGroupBy} />
        </DropdownMenuSubContent>
      </DropdownMenuSub>,
      m.refreshItem("refresh"),
    ],
  );
}
