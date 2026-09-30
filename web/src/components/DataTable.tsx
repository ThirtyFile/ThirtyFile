import type { ReactNode } from "react";
import { ErrorState } from "@/components/ErrorState";
import { RowMenuArea } from "@/components/RowMenuArea";
import { Skeleton } from "@/components/ui/skeleton";
import { useSelectableList } from "@/lib/listSelection";
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
 * With the keyboard it works like the file list (lib/listSelection.ts), and screen readers hear it as a grid with the selected row.
 */
export function DataTable<T>(p: {
  /** What the table lists, for screen readers */
  label: string;
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
  /** The rows couldn't be loaded: shown with a way to try again instead of an empty list (rows already shown stay) */
  error?: Error | null;
  onRetry?(): unknown;
  /** Shown when there's no data */
  empty?: ReactNode;
  /** Fixed column widths (together with the columns' w-[…]); overlong content is truncated */
  fixed?: boolean;
  /** Compact list with shorter rows */
  compact?: boolean;
  rowClassName?(row: T): string | false | undefined;
}) {
  const selected = p.selectedKey === null ? null : (p.rows.find((r) => p.rowKey(r) === p.selectedKey) ?? null);
  const list = useSelectableList({
    items: p.rows,
    keyOf: p.rowKey,
    selected: new Set(selected ? [p.selectedKey!] : []),
    onSelect: (keys) => p.onSelect(keys.values().next().value ?? null),
    onOpen: p.onOpen && ((row) => p.onOpen!(row)),
  });
  return (
    <RowMenuArea
      className="min-h-0 flex-1 overflow-auto"
      onTarget={p.onSelect}
      onClick={(e) => !(e.target as HTMLElement).closest("[data-row-id]") && p.onSelect(null)}
      menu={p.menu(selected)}
    >
      {p.loading ? (
        <Skeleton className="m-3 h-40" />
      ) : p.error && p.rows.length === 0 ? (
        <ErrorState message={p.error.message} onRetry={() => p.onRetry?.()} />
      ) : p.rows.length === 0 && p.empty ? (
        p.empty
      ) : (
        // role="grid": screen readers only report which row is selected in a grid, not in a plain table
        <table {...list.listProps("grid", p.label)} className={cn("w-full min-w-[560px] border-collapse text-xs", p.fixed ? "table-fixed" : "whitespace-nowrap")}>
          <thead>
            <tr role="row">
              {p.columns.map((c, i) => (
                <th key={i} role="columnheader" className={cn(TH, i === 0 && "pl-3", c.className)}>
                  {c.header}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {p.rows.map((row) => {
              const k = p.rowKey(row);
              return (
                <tr
                  key={k}
                  role="row"
                  data-row-id={k}
                  {...list.itemProps(row)}
                  className={cn("cursor-default outline-none hover:bg-muted/70 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset aria-selected:bg-selection aria-selected:shadow-[inset_3px_0_0_var(--color-brand)]", p.rowClassName?.(row))}
                >
                  {p.columns.map((c, i) => (
                    <td
                      key={i}
                      role="gridcell"
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
