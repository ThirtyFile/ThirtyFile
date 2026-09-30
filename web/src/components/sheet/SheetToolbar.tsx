import type { ReactNode } from "react";
import {
  AlignCenterIcon,
  AlignLeftIcon,
  AlignRightIcon,
  ArrowDownToLineIcon,
  ArrowUpToLineIcon,
  BaselineIcon,
  BetweenHorizontalStartIcon,
  BetweenVerticalStartIcon,
  BoldIcon,
  ChevronDownIcon,
  Columns3Icon,
  DecimalsArrowLeftIcon,
  DecimalsArrowRightIcon,
  FoldVerticalIcon,
  Grid3x3Icon,
  ItalicIcon,
  PaintBucketIcon,
  PanelBottomIcon,
  RemoveFormattingIcon,
  Rows3Icon,
  SquareDashedIcon,
  SquareIcon,
  StrikethroughIcon,
  TableCellsMergeIcon,
  UnderlineIcon,
  WrapTextIcon,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import type { CellStyle } from "@/ooxml/xlsx/model";
import { t, tc } from "@/lib/i18n";
import { shortcut } from "@/lib/keys";
import { cn } from "@/lib/utils";
import { NativeSelect } from "@/components/ui/native-select";

export type BorderKind = "all" | "outside" | "bottom" | "top" | "left" | "right" | "thickOutside" | "none";

export interface ToolbarActions {
  style(change: (s: CellStyle) => CellStyle): void;
  border(kind: BorderKind): void;
  merge(): void;
  decimals(delta: 1 | -1): void;
  clearFormat(): void;
  insertRows(): void;
  insertCols(): void;
  deleteRows(): void;
  deleteCols(): void;
}

const SIZES = [8, 9, 10, 11, 12, 14, 16, 18, 20, 24, 28, 36, 48];

export const NUMBER_FORMATS: { label: string; code?: string; sample: string }[] = [
  { label: tc("numfmt", "General"), sample: "1234.5" },
  { label: t("Number"), code: "0.00", sample: "1234.50" },
  { label: t("Comma style"), code: "#,##0", sample: "1,235" },
  { label: t("Comma style (2 decimals)"), code: "#,##0.00", sample: "1,234.50" },
  { label: t("Currency"), code: '"NT$"#,##0', sample: "NT$1,235" },
  { label: t("Percentage"), code: "0%", sample: "12%" },
  { label: t("Percentage (2 decimals)"), code: "0.00%", sample: "12.35%" },
  { label: t("Short date"), code: "yyyy/m/d", sample: "2026/9/25" },
  { label: t("Long date"), code: 'yyyy"年"m"月"d"日"', sample: "2026年9月25日" }, // i18n-ignore: number format code and its actual rendered result
  { label: t("Time"), code: "h:mm", sample: "14:30" },
  { label: t("Scientific"), code: "0.00E+00", sample: "1.23E+03" },
  { label: t("Text"), code: "@", sample: t("Text") },
];

// Office's common palette: the first row is base colors, followed by tints and shades from light to dark
const PALETTE = [
  ["#000000", "#FFFFFF", "#E7E6E6", "#44546A", "#4472C4", "#ED7D31", "#A5A5A5", "#FFC000", "#5B9BD5", "#70AD47"],
  ["#7F7F7F", "#F2F2F2", "#D0CECE", "#D6DCE4", "#D9E1F2", "#FCE4D6", "#EDEDED", "#FFF2CC", "#DDEBF7", "#E2EFDA"],
  ["#595959", "#D9D9D9", "#AEAAAA", "#ACB9CA", "#B4C6E7", "#F8CBAD", "#DBDBDB", "#FFE699", "#BDD7EE", "#C6E0B4"],
  ["#404040", "#BFBFBF", "#757171", "#8497B0", "#8EA9DB", "#F4B084", "#C9C9C9", "#FFD966", "#9BC2E6", "#A9D08E"],
  ["#262626", "#A6A6A6", "#3A3838", "#333F4F", "#305496", "#C65911", "#7B7B7B", "#BF8F00", "#2F75B5", "#548235"],
  ["#C00000", "#FF0000", "#FFC000", "#FFFF00", "#92D050", "#00B050", "#00B0F0", "#0070C0", "#002060", "#7030A0"],
];

function Tool(props: { label: string; active?: boolean; disabled?: boolean; onClick?(): void; children: ReactNode; className?: string }) {
  return (
    <Button
      variant={props.active ? "secondary" : "ghost"}
      size="icon-sm"
      title={props.label}
      aria-label={props.label}
      aria-pressed={props.active}
      disabled={props.disabled}
      className={cn("size-7 shrink-0", props.className)}
      // Clicking the toolbar shouldn't make the cell lose focus
      onMouseDown={(e) => e.preventDefault()}
      onClick={props.onClick}
    >
      {props.children}
    </Button>
  );
}

const Sep = () => <span className="mx-1 h-5 shrink-0 border-l" />;

function ColorMenu(props: { label: string; icon: ReactNode; value?: string; noneLabel: string; onPick(c: string | undefined): void }) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button
            variant="ghost"
            size="sm"
            title={props.label}
            aria-label={props.label}
            className="h-7 shrink-0 gap-0.5 px-1"
            onMouseDown={(e) => e.preventDefault()}
          />
        }
      >
        <span className="flex flex-col items-center leading-none">
          {props.icon}
          <span className="mt-0.5 h-[3px] w-4 rounded-sm border border-black/10" style={{ background: props.value ?? "transparent" }} />
        </span>
        <ChevronDownIcon className="size-3 opacity-60" />
      </DropdownMenuTrigger>
      <DropdownMenuContent className="w-auto p-2">
        <DropdownMenuItem onClick={() => props.onPick(undefined)}>{props.noneLabel}</DropdownMenuItem>
        <DropdownMenuSeparator />
        <div className="grid grid-cols-10 gap-1 p-1">
          {PALETTE.flat().map((c) => (
            <DropdownMenuItem
              key={c}
              title={c}
              aria-label={c}
              className="size-5 rounded-sm border border-black/15 p-0"
              style={{ background: c }}
              onClick={() => props.onPick(c)}
            />
          ))}
        </div>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

export function SheetToolbar({
  style,
  merged,
  actions,
  disabledStructure,
}: {
  style: CellStyle;
  merged: boolean;
  actions: ToolbarActions;
  disabledStructure?: string;
}) {
  const toggle = (k: "bold" | "italic" | "underline" | "strike" | "wrap") => actions.style((s) => ({ ...s, [k]: !s[k] }));
  const fmt = NUMBER_FORMATS.find((f) => (f.code ?? undefined) === (style.numFmt ?? undefined));
  return (
    <div className="flex min-h-10 shrink-0 flex-wrap items-center gap-0.5 border-b px-2 py-1">
      <NativeSelect
        aria-label={t("Font size")}
        title={t("Font size")}
        size="xs"
        className="w-14 shrink-0 rounded px-1"
        value={style.size ?? 11}
        onChange={(e) => actions.style((s) => ({ ...s, size: Number(e.target.value) }))}
      >
        {[...new Set([...SIZES, style.size ?? 11])]
          .sort((a, b) => a - b)
          .map((n) => (
            <option key={n} value={n}>
              {n}
            </option>
          ))}
      </NativeSelect>
      <Tool label={t("{action} ({keys})", { action: t("Bold"), keys: shortcut("Ctrl+B") })} active={style.bold} onClick={() => toggle("bold")}>
        <BoldIcon />
      </Tool>
      <Tool label={t("{action} ({keys})", { action: t("Italic"), keys: shortcut("Ctrl+I") })} active={style.italic} onClick={() => toggle("italic")}>
        <ItalicIcon />
      </Tool>
      <Tool label={t("{action} ({keys})", { action: t("Underline"), keys: shortcut("Ctrl+U") })} active={style.underline} onClick={() => toggle("underline")}>
        <UnderlineIcon />
      </Tool>
      <Tool label={t("{action} ({keys})", { action: t("Strikethrough"), keys: shortcut("Ctrl+5") })} active={style.strike} onClick={() => toggle("strike")}>
        <StrikethroughIcon />
      </Tool>
      <ColorMenu
        label={t("Font color")}
        icon={<BaselineIcon className="size-4" />}
        value={style.color ?? "#000000"}
        noneLabel={t("Automatic")}
        onPick={(c) => actions.style((s) => ({ ...s, color: c }))}
      />
      <ColorMenu
        label={t("Fill color")}
        icon={<PaintBucketIcon className="size-4" />}
        value={style.bg}
        noneLabel={t("No fill")}
        onPick={(c) => actions.style((s) => ({ ...s, bg: c }))}
      />
      <Sep />
      <Tool
        label={t("Align left")}
        active={style.hAlign === "left"}
        onClick={() => actions.style((s) => ({ ...s, hAlign: s.hAlign === "left" ? undefined : "left" }))}
      >
        <AlignLeftIcon />
      </Tool>
      <Tool
        label={t("Center")}
        active={style.hAlign === "center"}
        onClick={() => actions.style((s) => ({ ...s, hAlign: s.hAlign === "center" ? undefined : "center" }))}
      >
        <AlignCenterIcon />
      </Tool>
      <Tool
        label={t("Align right")}
        active={style.hAlign === "right"}
        onClick={() => actions.style((s) => ({ ...s, hAlign: s.hAlign === "right" ? undefined : "right" }))}
      >
        <AlignRightIcon />
      </Tool>
      <Tool
        label={t("Top align")}
        active={style.vAlign === "top"}
        onClick={() => actions.style((s) => ({ ...s, vAlign: s.vAlign === "top" ? undefined : "top" }))}
      >
        <ArrowUpToLineIcon />
      </Tool>
      <Tool
        label={t("Middle align")}
        active={style.vAlign === "center"}
        onClick={() => actions.style((s) => ({ ...s, vAlign: s.vAlign === "center" ? undefined : "center" }))}
      >
        <FoldVerticalIcon />
      </Tool>
      <Tool label={t("Bottom align")} active={!style.vAlign || style.vAlign === "bottom"} onClick={() => actions.style((s) => ({ ...s, vAlign: undefined }))}>
        <ArrowDownToLineIcon />
      </Tool>
      <Tool label={t("Wrap text")} active={style.wrap} onClick={() => toggle("wrap")}>
        <WrapTextIcon />
      </Tool>
      <Tool label={merged ? t("Unmerge cells") : t("Merge & center")} active={merged} onClick={actions.merge}>
        <TableCellsMergeIcon />
      </Tool>
      <DropdownMenu>
        <DropdownMenuTrigger
          render={
            <Button
              variant="ghost"
              size="sm"
              title={t("Borders")}
              aria-label={t("Borders")}
              className="h-7 shrink-0 gap-0.5 px-1"
              onMouseDown={(e) => e.preventDefault()}
            />
          }
        >
          <Grid3x3Icon className="size-4" />
          <ChevronDownIcon className="size-3 opacity-60" />
        </DropdownMenuTrigger>
        <DropdownMenuContent>
          <DropdownMenuItem onClick={() => actions.border("all")}>
            <Grid3x3Icon /> {t("All borders")}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={() => actions.border("outside")}>
            <SquareIcon /> {t("Outside borders")}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={() => actions.border("thickOutside")}>
            <SquareIcon className="stroke-[3]" /> {t("Thick outside borders")}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={() => actions.border("bottom")}>
            <PanelBottomIcon /> {t("Bottom border")}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={() => actions.border("top")}>
            <PanelBottomIcon className="rotate-180" /> {t("Top border")}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={() => actions.border("left")}>
            <PanelBottomIcon className="rotate-90" /> {t("Left border")}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={() => actions.border("right")}>
            <PanelBottomIcon className="-rotate-90" /> {t("Right border")}
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem onClick={() => actions.border("none")}>
            <SquareDashedIcon /> {t("No border")}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      <Sep />
      <DropdownMenu>
        <DropdownMenuTrigger
          render={
            <Button
              variant="outline"
              size="sm"
              title={t("Number format")}
              aria-label={t("Number format")}
              className="h-7 w-28 shrink-0 justify-between px-2 text-xs font-normal"
              onMouseDown={(e) => e.preventDefault()}
            />
          }
        >
          <span className="truncate">{fmt?.label ?? t("Custom")}</span>
          <ChevronDownIcon className="size-3 opacity-60" />
        </DropdownMenuTrigger>
        <DropdownMenuContent>
          {NUMBER_FORMATS.map((f) => (
            <DropdownMenuItem key={f.label} onClick={() => actions.style((s) => ({ ...s, numFmt: f.code }))}>
              <span className="flex-1">{f.label}</span>
              <span className="ml-6 text-xs text-muted-foreground tabular-nums">{f.sample}</span>
            </DropdownMenuItem>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>
      <Tool label={t("Increase decimal")} onClick={() => actions.decimals(1)}>
        <DecimalsArrowRightIcon />
      </Tool>
      <Tool label={t("Decrease decimal")} onClick={() => actions.decimals(-1)}>
        <DecimalsArrowLeftIcon />
      </Tool>
      <Tool label={t("Clear formats")} onClick={actions.clearFormat}>
        <RemoveFormattingIcon />
      </Tool>
      <Sep />
      <DropdownMenu>
        <DropdownMenuTrigger
          render={
            <Button
              variant="ghost"
              size="sm"
              title={disabledStructure ?? t("Insert")}
              disabled={!!disabledStructure}
              className="h-7 shrink-0 gap-1 px-1.5 text-xs"
              onMouseDown={(e) => e.preventDefault()}
            />
          }
        >
          <BetweenHorizontalStartIcon className="size-4" /> {t("Insert")}
          <ChevronDownIcon className="size-3 opacity-60" />
        </DropdownMenuTrigger>
        <DropdownMenuContent>
          <DropdownMenuItem onClick={actions.insertRows}>
            <BetweenHorizontalStartIcon /> {t("Insert row above")}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={actions.insertCols}>
            <BetweenVerticalStartIcon /> {t("Insert column left")}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      <DropdownMenu>
        <DropdownMenuTrigger
          render={
            <Button
              variant="ghost"
              size="sm"
              title={disabledStructure ?? t("Delete")}
              disabled={!!disabledStructure}
              className="h-7 shrink-0 gap-1 px-1.5 text-xs"
              onMouseDown={(e) => e.preventDefault()}
            />
          }
        >
          <Rows3Icon className="size-4" /> {t("Delete")}
          <ChevronDownIcon className="size-3 opacity-60" />
        </DropdownMenuTrigger>
        <DropdownMenuContent>
          <DropdownMenuItem variant="destructive" onClick={actions.deleteRows}>
            <Rows3Icon /> {t("Delete row")}
          </DropdownMenuItem>
          <DropdownMenuItem variant="destructive" onClick={actions.deleteCols}>
            <Columns3Icon /> {t("Delete column")}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}

/** Increase/decrease decimals: add or remove one 0 in the number format's decimal part */
export function changeDecimals(fmt: string | undefined, delta: 1 | -1, sample: unknown): string | undefined {
  const code = fmt;
  if (!code || code === "General") {
    // General format: build a format from the current value's number of decimals
    const n = typeof sample === "number" ? sample : 0;
    const dec = (String(n).split(".")[1] ?? "").length;
    const next = Math.max(0, dec + delta);
    return next === 0 ? "0" : `0.${"0".repeat(next)}`;
  }
  return code
    .split(";")
    .map((sec) => {
      // Leave date/time formats alone
      if (/[ymdhs]/i.test(sec.replace(/"[^"]*"|\\./g, ""))) return sec;
      const parts = sec.split(/("[^"]*"|\\.)/);
      for (let i = 0; i < parts.length; i += 2) {
        const m = /([#0,]*[0#])(\.[0#]*)?/.exec(parts[i]);
        if (!m) continue;
        const dec = m[2] ? m[2].length - 1 : 0;
        const next = Math.max(0, dec + delta);
        parts[i] = parts[i].slice(0, m.index) + m[1] + (next ? "." + "0".repeat(next) : "") + parts[i].slice(m.index + m[0].length);
        break;
      }
      return parts.join("");
    })
    .join(";");
}
