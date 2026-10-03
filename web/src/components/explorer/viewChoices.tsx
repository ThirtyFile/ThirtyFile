/** The view, sort and group choices, shared by the command bar's menus and the context menu's submenus (so both show the same state) */
import type { SortKey, SortOrder } from "@/api";
import { useViews } from "@/components/style";
import { DropdownMenuRadioGroup, DropdownMenuRadioItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import type { ViewMode } from "@/components/fileList/layout";
import type { GroupBy } from "@/lib/listView";
import { t } from "@/lib/i18n";

const SORTS: [SortKey, string][] = [
  ["name", t("Name")],
  ["updated", t("Date modified")],
  ["created", t("Date created")],
  ["type", t("Type")],
  ["size", t("Size")],
];

const GROUPS: [GroupBy, string][] = [
  ["none", t("(None)")],
  ["type", t("Type")],
  ["date", t("Date modified")],
];

type Sort = { key: SortKey; order: SortOrder };

/** What the list is sorted by, then which way (radio items, so screen readers say which one is chosen) */
export function SortChoices({ sort, onChange }: { sort: Sort; onChange(sort: Sort): void }) {
  return (
    <>
      <DropdownMenuRadioGroup value={sort.key} onValueChange={(k) => onChange({ key: k as SortKey, order: sort.order })}>
        {SORTS.map(([k, label]) => (
          <DropdownMenuRadioItem key={k} value={k} closeOnClick>
            {label}
          </DropdownMenuRadioItem>
        ))}
      </DropdownMenuRadioGroup>
      <DropdownMenuSeparator />
      <DropdownMenuRadioGroup value={sort.order} onValueChange={(o) => onChange({ key: sort.key, order: o as SortOrder })}>
        <DropdownMenuRadioItem value="asc" closeOnClick>
          {t("Ascending")}
        </DropdownMenuRadioItem>
        <DropdownMenuRadioItem value="desc" closeOnClick>
          {t("Descending")}
        </DropdownMenuRadioItem>
      </DropdownMenuRadioGroup>
    </>
  );
}

/** The views the style offers (components/style) on this screen */
export function ViewChoices({ view, onChange }: { view: ViewMode; onChange(view: ViewMode): void }) {
  const views = useViews();
  return (
    <DropdownMenuRadioGroup value={view} onValueChange={(v) => onChange(v as ViewMode)}>
      {views.map(({ id, Icon, label }) => (
        <DropdownMenuRadioItem key={id} value={id} closeOnClick>
          <Icon /> {label}
        </DropdownMenuRadioItem>
      ))}
    </DropdownMenuRadioGroup>
  );
}

export function GroupChoices({ groupBy, onChange }: { groupBy: GroupBy; onChange(groupBy: GroupBy): void }) {
  return (
    <DropdownMenuRadioGroup value={groupBy} onValueChange={(g) => onChange(g as GroupBy)}>
      {GROUPS.map(([g, label]) => (
        <DropdownMenuRadioItem key={g} value={g} closeOnClick>
          {label}
        </DropdownMenuRadioItem>
      ))}
    </DropdownMenuRadioGroup>
  );
}
