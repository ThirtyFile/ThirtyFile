//! What every view of items shows after an item's name: the star of a favorite, and the dots of the person's tags (with
//! their names as the label). The file list's views put it there (rows.tsx), and so can other views of items.

import { StarIcon } from "lucide-react";
import type { Node } from "@/api";
import { TagDots } from "@/components/tags";
import { tc } from "@/lib/i18n";
import { cn } from "@/lib/utils";

/** Whether an item has anything for ItemMarks to show */
export const hasMarks = (item: Pick<Node, "is_favorite" | "tags">) => item.is_favorite || !!item.tags?.length;

/** The star and the tag dots of an item, or nothing; `large`: the size for icon views */
export function ItemMarks({ item, large, className }: { item: Pick<Node, "is_favorite" | "tags">; large?: boolean; className?: string }) {
  if (!hasMarks(item)) return null;
  return (
    <span className={cn("flex shrink-0 items-center gap-1", className)}>
      {item.is_favorite && <StarIcon className={cn("shrink-0 fill-amber-400 text-amber-400", large ? "size-3" : "size-[11px]")} aria-label={tc("state", "Favorite")} />}
      {!!item.tags?.length && <TagDots ids={item.tags} />}
    </span>
  );
}
