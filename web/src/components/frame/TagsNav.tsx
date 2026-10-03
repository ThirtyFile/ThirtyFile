//! The Tags section of the Windows style's navigation pane: the person's own tags, each opening the list of the items
//! that have it, with New tag; each tag's context menu renames, recolours or deletes it. The Mac style's sidebar shows
//! the same tags and button in a group of its own.

import { NavLink, useNavigate } from "react-router";
import { useQueryClient } from "@tanstack/react-query";
import { FolderOpenIcon, PaletteIcon, PanelTopIcon, PencilIcon, PlusIcon, Trash2Icon } from "lucide-react";
import type { Tag } from "@/api";
import { ColorItems, TagDot, askToDeleteTag, tagPath } from "@/components/tags";
import { ContextMenu, ContextMenuContent, ContextMenuTrigger } from "@/components/ui/context-menu";
import { DropdownMenuItem, DropdownMenuSeparator, DropdownMenuSub, DropdownMenuSubContent, DropdownMenuSubTrigger } from "@/components/ui/dropdown-menu";
import { t } from "@/lib/i18n";
import { editTag, useTags } from "@/lib/tags";
import { cn } from "@/lib/utils";
import { useTabActions } from "@/tabs";

/** A location's link in the Windows style's navigation pane: the one open has the brand colour's bar on its left */
export const windowsNavItem = (isActive: boolean) =>
  cn(
    "flex h-[29px] items-center gap-[7px] rounded px-2 whitespace-nowrap text-muted-foreground hover:bg-muted",
    isActive && "bg-selection text-accent-foreground shadow-[inset_3px_0_0_var(--color-brand)] hover:bg-selection",
  );

/** A tag: its link, and its context menu. `itemClass`: how the link looks, open or not */
export function TagItem({ tag, itemClass = windowsNavItem }: { tag: Tag; itemClass?: (isActive: boolean) => string }) {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const tabs = useTabActions();
  const to = tagPath(tag);
  return (
    <ContextMenu>
      <ContextMenuTrigger className="contents">
        <NavLink to={to} className={({ isActive }) => itemClass(isActive)}>
          {/* The dot stands where the other locations have their icon */}
          <span className="flex size-[15px] shrink-0 items-center justify-center">
            <TagDot color={tag.color} />
          </span>
          <span className="truncate">{tag.name}</span>
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
        <DropdownMenuItem onClick={() => void editTag(tag)}>
          <PencilIcon /> {t("Rename…")}
        </DropdownMenuItem>
        <DropdownMenuSub>
          <DropdownMenuSubTrigger>
            <PaletteIcon /> {t("Color")}
          </DropdownMenuSubTrigger>
          <DropdownMenuSubContent className="w-40">
            <ColorItems tag={tag} />
          </DropdownMenuSubContent>
        </DropdownMenuSub>
        <DropdownMenuSeparator />
        <DropdownMenuItem variant="destructive" onClick={() => void askToDeleteTag(qc, tag, () => location.pathname === to && navigate("/files"))}>
          <Trash2Icon /> {t("Delete tag")}
        </DropdownMenuItem>
      </ContextMenuContent>
    </ContextMenu>
  );
}

/** The Tags section: a heading with New tag, then each tag */
export function TagsNav() {
  const { tags } = useTags();
  return (
    <section aria-labelledby="tags-nav-heading" className="mt-3 border-t pt-2">
      <div className="flex items-center justify-between px-2 pb-1">
        <h2 id="tags-nav-heading" className="text-[11px] font-normal text-muted-foreground">
          {t("Tags")}
        </h2>
        <NewTagButton />
      </div>
      {tags.length > 0 && (
        <ul className="grid">
          {tags.map((tag) => (
            <li key={tag.id}>
              <TagItem tag={tag} />
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** The small + button by the Tags heading */
export function NewTagButton() {
  return (
    <button
      type="button"
      onClick={() => void editTag()}
      aria-label={t("New tag")}
      title={t("New tag")}
      className="flex size-5 items-center justify-center rounded text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
    >
      <PlusIcon className="size-3.5" />
    </button>
  );
}
