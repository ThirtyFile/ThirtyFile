//! A color menu: Office's common palette, and none

import type { ReactNode } from "react";
import { ChevronDownIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { t } from "@/lib/i18n";

// Office's common palette: the first row is base colors, followed by tints and shades from light to dark
export const PALETTE = [
  ["#000000", "#FFFFFF", "#E7E6E6", "#44546A", "#4472C4", "#ED7D31", "#A5A5A5", "#FFC000", "#5B9BD5", "#70AD47"],
  ["#7F7F7F", "#F2F2F2", "#D0CECE", "#D6DCE4", "#D9E1F2", "#FCE4D6", "#EDEDED", "#FFF2CC", "#DDEBF7", "#E2EFDA"],
  ["#595959", "#D9D9D9", "#AEAAAA", "#ACB9CA", "#B4C6E7", "#F8CBAD", "#DBDBDB", "#FFE699", "#BDD7EE", "#C6E0B4"],
  ["#404040", "#BFBFBF", "#757171", "#8497B0", "#8EA9DB", "#F4B084", "#C9C9C9", "#FFD966", "#9BC2E6", "#A9D08E"],
  ["#262626", "#A6A6A6", "#3A3838", "#333F4F", "#305496", "#C65911", "#7B7B7B", "#BF8F00", "#2F75B5", "#548235"],
  ["#C00000", "#FF0000", "#FFC000", "#FFFF00", "#92D050", "#00B050", "#00B0F0", "#0070C0", "#002060", "#7030A0"],
];

/**
 * A palette color's name, for screen readers and the tooltip: the theme colors (the first row, with their lighter and
 * darker variants below) and the standard colors (the last row), as Office arranges them; null for another color
 */
export function paletteName(color: string): string | null {
  const at = PALETTE.findIndex((row) => row.includes(color.toUpperCase()));
  if (at < 0) return null;
  const col = PALETTE[at].indexOf(color.toUpperCase());
  // The six accent colors are named by their number too, as Office does, so none shares a name with a standard color
  const accent = (color: string, n: number) => t("{color}, accent {n}", { color, n });
  const theme = [
    t("Black"),
    t("White"),
    t("Light gray"),
    t("Blue-gray"),
    accent(t("Blue"), 1),
    accent(t("Orange"), 2),
    accent(t("Gray"), 3),
    accent(t("Gold"), 4),
    accent(t("Blue"), 5),
    accent(t("Green"), 6),
  ];
  const standard = [t("Dark red"), t("Red"), t("Orange"), t("Yellow"), t("Light green"), t("Green"), t("Light blue"), t("Blue"), t("Dark blue"), t("Purple")];
  if (at === 0) return theme[col];
  if (at === PALETTE.length - 1) return standard[col];
  return t("{color}, variant {n}", { color: theme[col], n: at });
}

/** A color chosen from Office's palette, or none: for fill and font colors (workbooks) and shading (documents) */
/** `colors`: the colors offered, when not the whole palette (Word's highlight colors, say), with `names` for them */
export function ColorMenu(props: {
  label: string;
  icon: ReactNode;
  value?: string;
  noneLabel: string;
  disabled?: boolean;
  colors?: string[];
  names?: Record<string, string>;
  onPick(c: string | undefined): void;
}) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button
            variant="ghost"
            size="sm"
            title={props.label}
            aria-label={props.label}
            disabled={props.disabled}
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
          {(props.colors ?? PALETTE.flat()).map((c) => {
            // Named for screen readers; the tooltip adds the hex code
            const name = props.names?.[c] ?? paletteName(c) ?? c;
            return (
              <DropdownMenuItem
                key={c}
                title={name === c ? c : `${name} (${c})`}
                aria-label={name}
                className="size-5 rounded-sm border border-black/15 p-0"
                style={{ background: c }}
                onClick={() => props.onPick(c)}
              />
            );
          })}
        </div>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
