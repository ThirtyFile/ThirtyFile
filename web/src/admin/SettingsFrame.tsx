import { Switch } from "@base-ui/react/switch";
import { RefreshCwIcon, type LucideIcon } from "lucide-react";
import { Frame, ToolButton } from "@/components/Frame";
import { controlPanelItem, type ControlPanelKey, useSettingsSearch } from "@/admin/controlPanel";
import { cn } from "@/lib/utils";
import { t } from "@/lib/i18n";

export function Toggle({ checked, disabled, onChange, label }: { checked: boolean; disabled?: boolean; onChange(v: boolean): void; label: string }) {
  return (
    <Switch.Root
      checked={checked}
      disabled={disabled}
      onCheckedChange={onChange}
      aria-label={label}
      className={cn(
        "relative inline-flex h-5 w-9 shrink-0 items-center rounded-full border border-transparent transition-colors outline-none focus-visible:ring-3 focus-visible:ring-ring disabled:opacity-50",
        checked ? "bg-brand" : "bg-input",
      )}
    >
      <Switch.Thumb className={cn("block size-4 rounded-full bg-white shadow transition-transform", checked ? "translate-x-4" : "translate-x-0.5")} />
    </Switch.Root>
  );
}

export function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="grid gap-3">
      <h2 className="text-xs font-medium text-muted-foreground">{title}</h2>
      <div className="overflow-hidden rounded-lg border bg-card">{children}</div>
    </section>
  );
}

/** Settings page under the control panel: the address bar shows "Control panel › item", and going up returns to the control panel */
export function SettingsFrame({ item, onRefresh, footer, children }: { item: ControlPanelKey; onRefresh(): void; footer?: React.ReactNode; children: React.ReactNode }) {
  const { title, icon } = controlPanelItem(item);
  const searchSettings = useSettingsSearch();
  return (
    <Frame
      toolbar={<ToolButton icon={RefreshCwIcon} label={t("Refresh")} showLabel onClick={onRefresh} />}
      crumbs={[{ label: t("Control panel"), to: "/admin" }, { label: title }]}
      icon={icon as LucideIcon}
      upTo="/admin"
      searchPlaceholder={t("Search settings")}
      onSearch={searchSettings}
      footer={footer ?? <span>{t("Only administrators can make changes")}</span>}
    >
      <div className="min-h-0 flex-1 overflow-auto">
        <div className="mx-auto grid max-w-3xl gap-8 p-6">{children}</div>
      </div>
    </Frame>
  );
}
