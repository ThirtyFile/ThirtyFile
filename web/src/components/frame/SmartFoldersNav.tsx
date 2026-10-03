//! The Smart folders section of the Windows style's navigation pane: the person's saved searches, each opening what it
//! finds, with New smart folder; each one's context menu opens, edits or deletes it. Nothing can be dropped on one: a
//! smart folder holds no items, so moving an item never puts it in one.

import { NavLink, useNavigate } from "react-router";
import { useQueryClient } from "@tanstack/react-query";
import { FolderOpenIcon, FolderSearchIcon, PanelTopIcon, PencilIcon, PlusIcon, Trash2Icon } from "lucide-react";
import type { SmartFolder } from "@/api";
import { askToDeleteSmartFolder } from "@/components/smartFolders";
import { ContextMenu, ContextMenuContent, ContextMenuTrigger } from "@/components/ui/context-menu";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { t } from "@/lib/i18n";
import { editSmartFolder, smartPath, useSmartFolders } from "@/lib/smartFolders";
import { cn } from "@/lib/utils";
import { useTabActions } from "@/tabs";

function SmartFolderItem({ folder }: { folder: SmartFolder }) {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const tabs = useTabActions();
  const to = smartPath(folder);
  return (
    <ContextMenu>
      <ContextMenuTrigger className="contents">
        <NavLink
          to={to}
          className={({ isActive }) =>
            cn(
              "flex h-[29px] items-center gap-[7px] rounded px-2 whitespace-nowrap text-muted-foreground hover:bg-muted",
              isActive && "bg-selection text-accent-foreground shadow-[inset_3px_0_0_var(--color-brand)] hover:bg-selection",
            )
          }
        >
          <FolderSearchIcon aria-hidden className="size-[15px] shrink-0" />
          <span className="truncate">{folder.name}</span>
        </NavLink>
      </ContextMenuTrigger>
      <ContextMenuContent>
        <DropdownMenuItem onClick={() => navigate(to)}>
          <FolderOpenIcon /> {t("Open")}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={() => tabs.open(to, { reuse: true })}>
          <PanelTopIcon /> {t("Open in new tab")}
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem onClick={() => void editSmartFolder(folder)}>
          <PencilIcon /> {t("Edit…")}
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem variant="destructive" onClick={() => void askToDeleteSmartFolder(qc, folder, () => location.pathname === to && navigate("/files"))}>
          <Trash2Icon /> {t("Delete smart folder")}
        </DropdownMenuItem>
      </ContextMenuContent>
    </ContextMenu>
  );
}

/** The Smart folders section: a heading with New smart folder, then each smart folder */
export function SmartFoldersNav() {
  const { folders } = useSmartFolders();
  const navigate = useNavigate();
  return (
    <section aria-labelledby="smart-nav-heading" className="mt-3 border-t pt-2">
      <div className="flex items-center justify-between px-2 pb-1">
        <h2 id="smart-nav-heading" className="text-[11px] font-normal text-muted-foreground">
          {t("Smart folders")}
        </h2>
        <button
          type="button"
          onClick={async () => {
            const folder = await editSmartFolder();
            if (folder) navigate(smartPath(folder));
          }}
          aria-label={t("New smart folder")}
          title={t("New smart folder")}
          className="flex size-5 items-center justify-center rounded text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
        >
          <PlusIcon className="size-3.5" />
        </button>
      </div>
      {folders.length > 0 && (
        <ul className="grid">
          {folders.map((folder) => (
            <li key={folder.id}>
              <SmartFolderItem folder={folder} />
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
