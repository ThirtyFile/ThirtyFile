import type { ReactNode } from "react";
import { ContextMenu, ContextMenuContent, ContextMenuTrigger } from "@/components/ui/context-menu";

/**
 * Context menu for a list / table area: right-clicking a row with `data-row-id` first reports that row (usually to select it),
 * right-clicking empty space reports null. The menu contents depend on the current selection.
 */
export function RowMenuArea(props: {
  className?: string;
  onTarget(id: string | null): void;
  menu: ReactNode;
  children: ReactNode;
  onClick?(e: React.MouseEvent): void;
}) {
  return (
    <ContextMenu>
      <ContextMenuTrigger
        className={props.className}
        onClick={props.onClick}
        onContextMenuCapture={(e) => {
          const row = (e.target as HTMLElement).closest<HTMLElement>("[data-row-id]");
          props.onTarget(row?.dataset.rowId ?? null);
        }}
      >
        {props.children}
      </ContextMenuTrigger>
      <ContextMenuContent>{props.menu}</ContextMenuContent>
    </ContextMenu>
  );
}
