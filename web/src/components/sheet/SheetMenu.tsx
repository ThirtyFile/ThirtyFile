import {
  BetweenHorizontalStartIcon,
  BetweenVerticalStartIcon,
  ClipboardPasteIcon,
  Columns3Icon,
  CopyIcon,
  EraserIcon,
  Redo2Icon,
  RemoveFormattingIcon,
  Rows3Icon,
  ScissorsIcon,
  TableCellsMergeIcon,
  Undo2Icon,
} from "lucide-react";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { t } from "@/lib/i18n";
import { shortcut } from "@/lib/keys";

export type MenuTarget = "cell" | "row" | "col";

export interface SheetMenuProps {
  /** Right-clicked on a cell, row header or column header */
  target: MenuTarget;
  rowCount: number;
  colCount: number;
  /** Why rows/columns can't be inserted or deleted (e.g. a pivot table is present) */
  structureBlocked?: string;
  merged: boolean;
  canUndo: boolean;
  canRedo: boolean;
  onCut(): void;
  onCopy(): void;
  onPaste(): void;
  onInsertRows(): void;
  onInsertCols(): void;
  onDeleteRows(): void;
  onDeleteCols(): void;
  onClear(): void;
  onClearFormat(): void;
  onMerge(): void;
  onAutofit(): void;
  onUndo(): void;
  onRedo(): void;
}

const Shortcut = ({ children }: { children: string }) => <span className="ml-auto text-xs text-muted-foreground">{shortcut(children)}</span>;

/** Spreadsheet context menu: shows items matching where it was clicked (cell, row header, column header) */
export function SheetMenu(p: SheetMenuProps) {
  const blocked = !!p.structureBlocked;
  return (
    <>
      <DropdownMenuItem onClick={p.onCut}>
        <ScissorsIcon /> {t("Cut")}
        <Shortcut>Ctrl+X</Shortcut>
      </DropdownMenuItem>
      <DropdownMenuItem onClick={p.onCopy}>
        <CopyIcon /> {t("Copy")}
        <Shortcut>Ctrl+C</Shortcut>
      </DropdownMenuItem>
      <DropdownMenuItem onClick={p.onPaste}>
        <ClipboardPasteIcon /> {t("Paste")}
        <Shortcut>Ctrl+V</Shortcut>
      </DropdownMenuItem>
      <DropdownMenuSeparator />
      {p.target !== "col" && (
        <DropdownMenuItem disabled={blocked} onClick={p.onInsertRows}>
          <BetweenHorizontalStartIcon /> {p.rowCount > 1 ? t("Insert {n} row above|Insert {n} rows above", { n: p.rowCount }) : t("Insert row above")}
        </DropdownMenuItem>
      )}
      {p.target !== "row" && (
        <DropdownMenuItem disabled={blocked} onClick={p.onInsertCols}>
          <BetweenVerticalStartIcon /> {p.colCount > 1 ? t("Insert {n} column left|Insert {n} columns left", { n: p.colCount }) : t("Insert column left")}
        </DropdownMenuItem>
      )}
      {p.target !== "col" && (
        <DropdownMenuItem variant="destructive" disabled={blocked} onClick={p.onDeleteRows}>
          <Rows3Icon /> {p.rowCount > 1 ? t("Delete {n} row|Delete {n} rows", { n: p.rowCount }) : t("Delete row")}
        </DropdownMenuItem>
      )}
      {p.target !== "row" && (
        <DropdownMenuItem variant="destructive" disabled={blocked} onClick={p.onDeleteCols}>
          <Columns3Icon /> {p.colCount > 1 ? t("Delete {n} column|Delete {n} columns", { n: p.colCount }) : t("Delete column")}
        </DropdownMenuItem>
      )}
      <DropdownMenuSeparator />
      <DropdownMenuItem onClick={p.onClear}>
        <EraserIcon /> {t("Clear contents")}
        <Shortcut>Delete</Shortcut>
      </DropdownMenuItem>
      <DropdownMenuItem onClick={p.onClearFormat}>
        <RemoveFormattingIcon /> {t("Clear formats")}
      </DropdownMenuItem>
      {p.target === "cell" && (
        <DropdownMenuItem onClick={p.onMerge}>
          <TableCellsMergeIcon /> {p.merged ? t("Unmerge cells") : t("Merge & center")}
        </DropdownMenuItem>
      )}
      {p.target === "col" && (
        <DropdownMenuItem onClick={p.onAutofit}>
          <Columns3Icon /> {t("AutoFit column width")}
        </DropdownMenuItem>
      )}
      <DropdownMenuSeparator />
      <DropdownMenuItem disabled={!p.canUndo} onClick={p.onUndo}>
        <Undo2Icon /> {t("Undo")}
        <Shortcut>Ctrl+Z</Shortcut>
      </DropdownMenuItem>
      <DropdownMenuItem disabled={!p.canRedo} onClick={p.onRedo}>
        <Redo2Icon /> {t("Redo")}
        <Shortcut>Ctrl+Y</Shortcut>
      </DropdownMenuItem>
    </>
  );
}
