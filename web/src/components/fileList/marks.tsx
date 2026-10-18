//! What every view of items shows after an item's name: a restricted folder's lock, the star of a favorite, and the dots
//! of the person's tags (with their names as the label). The file list's views put it there (rows.tsx), and so can
//! other views of items.

import { LockKeyholeIcon, StarIcon } from "lucide-react";
import type { Node } from "@/api";
import { TagDots } from "@/components/tags";
import { t, tc } from "@/lib/i18n";
import { cn } from "@/lib/utils";

/** Whether an item has anything for ItemMarks to show */
export const hasMarks = (item: Pick<Node, "is_favorite" | "tags" | "restricted">) => item.restricted || item.is_favorite || !!item.tags?.length;

/** The lock, the star and the tag dots of an item, or nothing; `large`: the size for icon views */
export function ItemMarks({ item, large, className }: { item: Pick<Node, "is_favorite" | "tags" | "restricted">; large?: boolean; className?: string }) {
  if (!hasMarks(item)) return null;
  return (
    <span className={cn("flex shrink-0 items-center gap-1", className)}>
      {item.restricted && <LockKeyholeIcon className={cn("shrink-0 text-muted-foreground", large ? "size-3" : "size-[11px]")} aria-label={t("Restricted folder")} role="img" />}
      {item.is_favorite && <StarIcon className={cn("shrink-0 fill-amber-400 text-amber-400", large ? "size-3" : "size-[11px]")} aria-label={tc("state", "Favorite")} />}
      {!!item.tags?.length && <TagDots ids={item.tags} />}
    </span>
  );
}
