import { useEffect, useRef, useState, type ReactNode } from "react";
import { CalendarIcon, ChevronDownIcon, FilterIcon, SearchIcon, XIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { t } from "@/lib/i18n";

/** Apply only after typing pauses for 300 ms, to avoid a query on every keystroke */
export function SearchBox({
  value,
  onChange,
  placeholder,
  className,
}: {
  value: string;
  onChange(v: string): void;
  placeholder: string;
  className?: string;
}) {
  const [text, setText] = useState(value);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => setText(value), [value]);
  useEffect(() => () => clearTimeout(timer.current), []);
  return (
    <div className={cn("relative", className)}>
      <SearchIcon className="pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-muted-foreground" />
      <Input
        value={text}
        placeholder={placeholder}
        aria-label={placeholder}
        className="h-8 pr-7 pl-7 text-xs md:text-xs"
        onChange={(e) => {
          setText(e.target.value);
          clearTimeout(timer.current);
          const v = e.target.value;
          timer.current = setTimeout(() => onChange(v.trim()), 300);
        }}
      />
      {text && (
        <button
          type="button"
          aria-label={t("Clear")}
          className="absolute top-1/2 right-1.5 -translate-y-1/2 rounded p-0.5 text-muted-foreground hover:bg-muted"
          onClick={() => {
            setText("");
            onChange("");
          }}
        >
          <XIcon className="size-3.5" />
        </button>
      )}
    </div>
  );
}

export interface OptionGroup {
  label?: string;
  options: { value: string; label: string }[];
}

/** Multi-select dropdown (e.g. action types, events) */
export function MultiSelect({
  label,
  groups,
  value,
  onChange,
}: {
  label: string;
  groups: OptionGroup[];
  value: string[];
  onChange(v: string[]): void;
}) {
  const all = groups.flatMap((g) => g.options);
  const summary =
    value.length === 0
      ? t("{label}: All", { label })
      : value.length === 1
        ? (all.find((o) => o.value === value[0])?.label ?? value[0])
        : t("{label} ({n})", { label, n: value.length });
  const toggle = (v: string, on: boolean) => onChange(on ? [...value, v] : value.filter((x) => x !== v));
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={<Button variant="outline" size="sm" className={cn("h-8 gap-1.5 text-xs font-normal", value.length && "border-brand text-brand")} />}
      >
        <FilterIcon className="size-3.5" />
        {summary}
        <ChevronDownIcon className="size-3 opacity-60" />
      </DropdownMenuTrigger>
      <DropdownMenuContent className="max-h-80 w-52 overflow-y-auto">
        <DropdownMenuItem disabled={!value.length} onClick={() => onChange([])}>
          {t("Show all")}
        </DropdownMenuItem>
        {groups.map((g, i) => (
          <DropdownMenuGroup key={g.label ?? i}>
            <DropdownMenuSeparator />
            {g.label && (
              <DropdownMenuLabel className="flex items-center justify-between text-xs">
                {g.label}
                <button
                  type="button"
                  className="text-[11px] font-normal text-brand hover:underline"
                  onClick={() => {
                    const vs = g.options.map((o) => o.value);
                    const allOn = vs.every((v) => value.includes(v));
                    onChange(allOn ? value.filter((v) => !vs.includes(v)) : [...new Set([...value, ...vs])]);
                  }}
                >
                  {g.options.every((o) => value.includes(o.value)) ? t("Cancel") : t("Select all")}
                </button>
              </DropdownMenuLabel>
            )}
            {g.options.map((o) => (
              <DropdownMenuCheckboxItem
                key={o.value}
                checked={value.includes(o.value)}
                onCheckedChange={(on) => toggle(o.value, !!on)}
                closeOnClick={false}
              >
                {o.label}
              </DropdownMenuCheckboxItem>
            ))}
          </DropdownMenuGroup>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

export type RangeKey = "all" | "today" | "7" | "30" | "90" | "custom";
export interface DateRange {
  key: RangeKey;
  /** Custom dates (yyyy-mm-dd, local time) */
  from?: string;
  to?: string;
}

const RANGES: { key: RangeKey; label: string }[] = [
  { key: "all", label: t("All time") },
  { key: "today", label: t("Today") },
  { key: "7", label: t("Last {n} day|Last {n} days", { n: 7 }) },
  { key: "30", label: t("Last {n} day|Last {n} days", { n: 30 }) },
  { key: "90", label: t("Last {n} day|Last {n} days", { n: 90 }) },
  { key: "custom", label: t("Custom range…") },
];

/** Date range → Unix seconds (start inclusive, end exclusive) */
export function rangeToUnix(r: DateRange): { from?: number; to?: number } {
  const startOfToday = new Date();
  startOfToday.setHours(0, 0, 0, 0);
  const sec = (d: Date) => Math.floor(d.getTime() / 1000);
  switch (r.key) {
    case "all":
      return {};
    case "today":
      return { from: sec(startOfToday) };
    case "custom": {
      const from = r.from ? sec(new Date(`${r.from}T00:00:00`)) : undefined;
      const to = r.to ? sec(new Date(`${r.to}T00:00:00`)) + 86400 : undefined;
      return { from, to };
    }
    default:
      return { from: sec(startOfToday) - (Number(r.key) - 1) * 86400 };
  }
}

export function DateRangeFilter({ value, onChange }: { value: DateRange; onChange(v: DateRange): void }) {
  const label = RANGES.find((r) => r.key === value.key)?.label ?? "";
  return (
    <div className="flex flex-wrap items-center gap-1.5">
      <DropdownMenu>
        <DropdownMenuTrigger
          render={
            <Button variant="outline" size="sm" className={cn("h-8 gap-1.5 text-xs font-normal", value.key !== "all" && "border-brand text-brand")} />
          }
        >
          <CalendarIcon className="size-3.5" />
          {value.key === "custom" ? t("Custom range") : label}
          <ChevronDownIcon className="size-3 opacity-60" />
        </DropdownMenuTrigger>
        <DropdownMenuContent className="w-40">
          {RANGES.map((r) => (
            <DropdownMenuItem key={r.key} onClick={() => onChange({ ...value, key: r.key })}>
              {r.label}
            </DropdownMenuItem>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>
      {value.key === "custom" && (
        <>
          <Input
            type="date"
            aria-label={t("Start date")}
            className="h-8 w-36 text-xs md:text-xs"
            value={value.from ?? ""}
            onChange={(e) => onChange({ ...value, from: e.target.value })}
          />
          <span className="text-xs text-muted-foreground">{t("to")}</span>
          <Input
            type="date"
            aria-label={t("End date")}
            className="h-8 w-36 text-xs md:text-xs"
            value={value.to ?? ""}
            onChange={(e) => onChange({ ...value, to: e.target.value })}
          />
        </>
      )}
    </div>
  );
}

/** Filter bar container */
export function FilterBar({ children, right }: { children: ReactNode; right?: ReactNode }) {
  return (
    <div className="flex flex-wrap items-center gap-2 border-b px-3 py-2">
      {children}
      <span className="flex-1" />
      {right}
    </div>
  );
}
