import type { KeyboardEvent, ReactNode } from "react";
import { RowMenuArea } from "@/components/RowMenuArea";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";

export interface Column<T> {
  header: ReactNode;
  /** Class shared by header and cells: width, hiding on small screens (e.g. w-[100px] max-md:hidden) */
  className?: string;
  /** Class applied only to cells (e.g. text-muted-foreground) */
  cellClassName?: string;
  cell(row: T): ReactNode;
  /** Tooltip when hovering a cell */
  title?(row: T): string | undefined;
}

const TH = "sticky top-0 z-[1] h-[30px] border-b bg-background px-2.5 text-left font-normal text-muted-foreground";
const TD = "border-b border-border/40 px-2.5";

/**
 * Table shared by the admin pages: click a row to select, double-click to open, the context menu depends on the selected row, clicking empty space clears the selection.
 */
export function DataTable<T>(p: {
  rows: T[];
  rowKey(row: T): string;
  columns: Column<T>[];
  selectedKey: string | null;
  onSelect(key: string | null): void;
  /** Row double-clicked */
  onOpen?(row: T): void;
  /** Context menu; selected is null when clicking empty space */
  menu(selected: T | null): ReactNode;
  loading?: boolean;
  /** Shown when there's no data */
  empty?: ReactNode;
  /** Fixed column widths (together with the columns' w-[…]); overlong content is truncated */
  fixed?: boolean;
  /** Compact list with shorter rows */
  compact?: boolean;
  rowClassName?(row: T): string | false | undefined;
}) {
  const selected = p.selectedKey === null ? null : (p.rows.find((r) => p.rowKey(r) === p.selectedKey) ?? null);
  return (
    <RowMenuArea
      className="min-h-0 flex-1 overflow-auto"
      onTarget={p.onSelect}
      onClick={(e) => !(e.target as HTMLElement).closest("[data-row-id]") && p.onSelect(null)}
      menu={p.menu(selected)}
    >
      {p.loading ? (
        <Skeleton className="m-3 h-40" />
      ) : p.rows.length === 0 && p.empty ? (
        p.empty
      ) : (
        <table className={cn("w-full min-w-[560px] border-collapse text-xs", p.fixed ? "table-fixed" : "whitespace-nowrap")}>
          <thead>
            <tr>
              {p.columns.map((c, i) => (
                <th key={i} className={cn(TH, i === 0 && "pl-3", c.className)}>
                  {c.header}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {p.rows.map((row, index) => {
              const k = p.rowKey(row);
              // Keyboard: Tab reaches the selected row (or the first), arrows move the selection, Enter opens
              const onKeyDown = (e: KeyboardEvent<HTMLTableRowElement>) => {
                // Keys typed in a control inside the row belong to that control; a held key doesn't open repeatedly
                if (e.target !== e.currentTarget || (e.key === "Enter" && e.repeat)) return;
                let next: number | null = null;
                if (e.key === "ArrowDown") next = Math.min(p.rows.length - 1, index + 1);
                else if (e.key === "ArrowUp") next = Math.max(0, index - 1);
                else if (e.key === "Home") next = 0;
                else if (e.key === "End") next = p.rows.length - 1;
                else if (e.key === "Enter" && p.onOpen) p.onOpen(row);
                else if (e.key === " ") {
                  e.preventDefault();
                  p.onSelect(k);
                }
                if (next === null) return;
                e.preventDefault();
                const target = p.rowKey(p.rows[next]);
                p.onSelect(target);
                e.currentTarget.parentElement?.querySelector<HTMLElement>(`[data-row-id="${CSS.escape(target)}"]`)?.focus();
              };
              return (
                <tr
                  key={k}
                  data-row-id={k}
                  aria-selected={k === p.selectedKey}
                  tabIndex={k === p.selectedKey || (p.selectedKey === null && index === 0) ? 0 : -1}
                  onKeyDown={onKeyDown}
                  onClick={() => p.onSelect(k)}
                  onDoubleClick={p.onOpen && (() => p.onOpen!(row))}
                  className={cn("cursor-default outline-none hover:bg-muted/70 focus-visible:ring-2 focus-visible:ring-ring/60 focus-visible:ring-inset aria-selected:bg-accent", p.rowClassName?.(row))}
                >
                  {p.columns.map((c, i) => (
                    <td
                      key={i}
                      title={c.title?.(row)}
                      className={cn(TD, p.compact ? "h-[30px]" : "h-[34px]", i === 0 && "pl-3", p.fixed && "truncate", c.className, c.cellClassName)}
                    >
                      {c.cell(row)}
                    </td>
                  ))}
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
    </RowMenuArea>
  );
}

/** Hint when there's no data */
export function EmptyState({ icon: Icon, title, hint }: { icon: React.ComponentType<{ className?: string }>; title: string; hint?: string }) {
  return (
    <div className="flex min-h-52 flex-col items-center justify-center gap-2 py-10 text-muted-foreground">
      <Icon className="size-9 stroke-[1.4]" />
      <p>{title}</p>
      {hint && <p className="text-xs">{hint}</p>}
    </div>
  );
}
