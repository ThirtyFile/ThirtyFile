//! The Details view's columns, and the menu that chooses them

import { DropdownMenuCheckboxItem, DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import type { SortKey } from "@/api";
import { columnShown, resetColumns, showColumn, useColumnPrefs, type ColumnId } from "@/lib/listView";
import { t } from "@/lib/i18n";
import type { FileListProps } from "@/components/FileList";

export interface ListColumn {
  id: ColumnId;
  label: string;
  /** Clicking the header sorts by it */
  sort?: SortKey;
}

/** The Details view's columns a list can show besides the name, in order (shown or not, see lib/listView.ts) */
export function listColumns(p: Pick<FileListProps, "showLocation" | "showOwner" | "extraColumn" | "dateLabel">): ListColumn[] {
  const out: ListColumn[] = [];
  if (p.showLocation) out.push({ id: "location", label: t("Location") });
  out.push({ id: "date", label: p.dateLabel ?? t("Date modified"), sort: "updated" });
  out.push({ id: "created", label: t("Date created"), sort: "created" });
  out.push({ id: "type", label: t("Type"), sort: "type" });
  out.push({ id: "size", label: t("Size"), sort: "size" });
  if (p.showOwner) out.push({ id: "owner", label: t("Uploaded by") });
  if (p.extraColumn) out.push({ id: "extra", label: p.extraColumn.label });
  return out;
}

/** Menu items choosing the columns (the column headers' context menu, and View › Columns) */
export function ColumnChoices({ columns }: { columns: ListColumn[] }) {
  const prefs = useColumnPrefs();
  return (
    <>
      <DropdownMenuCheckboxItem checked disabled>
        {t("Name")}
      </DropdownMenuCheckboxItem>
      {columns.map((c) => (
        <DropdownMenuCheckboxItem key={c.id} checked={columnShown(prefs, c.id)} onCheckedChange={(on) => showColumn(c.id, on)} closeOnClick>
          {c.label}
        </DropdownMenuCheckboxItem>
      ))}
      <DropdownMenuSeparator />
      <DropdownMenuItem onClick={resetColumns}>{t("Restore default columns")}</DropdownMenuItem>
    </>
  );
}
