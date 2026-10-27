import { useMemo, useState } from "react";
import { ChevronRightIcon } from "lucide-react";
import type { AnalyticsTreeNode } from "@/api";
import { Button } from "@/components/ui/button";
import { squarify } from "@/lib/treemap";
import { t } from "@/lib/i18n";
import { cn, formatBytes } from "@/lib/utils";

type By = "bytes" | "files";

const W = 720;
const H = 420;
/** A block smaller than this is drawn without a name */
const LABEL_MIN = { w: 70, h: 28 };

const count = (n: number) => new Intl.NumberFormat().format(n);

/** The name of a block */
function label(n: AnalyticsTreeNode): string {
  if (n.kind === "other") return t("Other folders");
  if (n.kind === "files") return t("Files here");
  return n.name;
}

/** A colour for a block, the same each time for the same name */
function hue(name: string): number {
  let h = 0;
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) % 360;
  return h;
}

const share = (part: number, whole: number) => (whole > 0 ? `${Math.round((part / whole) * 100)}%` : "0%");

/** The folders of a space as nested blocks by size (or by number of files), with the same data as a list beside it */
export function Treemap({ root }: { root: AnalyticsTreeNode }) {
  // The folders gone into, from the root: each is a node of the tree
  const [path, setPath] = useState<AnalyticsTreeNode[]>([]);
  const [by, setBy] = useState<By>("bytes");
  const [hover, setHover] = useState<AnalyticsTreeNode | null>(null);
  const here = path.at(-1) ?? root;
  const value = (n: AnalyticsTreeNode) => (by === "bytes" ? n.bytes : n.files);
  const blocks = useMemo(() => squarify(here.children, (n) => (by === "bytes" ? n.bytes : n.files), { x: 0, y: 0, w: W, h: H }), [here, by]);
  const total = value(here);
  const format = (n: AnalyticsTreeNode) => (by === "bytes" ? formatBytes(n.bytes) : t("{n} file|{n} files", { n: n.files }));
  const goInto = (n: AnalyticsTreeNode) => {
    if (n.kind === "folder" && n.children.length > 0) {
      setPath([...path, n]);
      setHover(null);
    }
  };
  const crumbs = [root, ...path];

  return (
    <div className="grid gap-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <nav aria-label={t("Folders gone into")} className="flex flex-wrap items-center gap-1 text-sm">
          {crumbs.map((c, i) => (
            <span key={`${c.id ?? c.name}-${i}`} className="flex items-center gap-1">
              {i > 0 && <ChevronRightIcon className="size-3.5 text-muted-foreground" />}
              {i === crumbs.length - 1 ? (
                <span className="font-medium">{c.name}</span>
              ) : (
                <button type="button" className="underline-offset-2 hover:underline" onClick={() => setPath(path.slice(0, i))}>
                  {c.name}
                </button>
              )}
            </span>
          ))}
        </nav>
        <div className="flex gap-1" role="group" aria-label={t("Size blocks by")}>
          {(
            [
              ["bytes", t("Size")],
              ["files", t("Number of files")],
            ] as const
          ).map(([v, name]) => (
            <Button key={v} type="button" size="xs" variant={by === v ? "default" : "outline"} aria-pressed={by === v} onClick={() => setBy(v)}>
              {name}
            </Button>
          ))}
        </div>
      </div>

      <div className="grid gap-4 lg:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]">
        <div className="min-w-0">
          {blocks.length === 0 ? (
            <p className="rounded-lg border p-6 text-center text-sm text-muted-foreground">{t("Nothing in this folder takes room.")}</p>
          ) : (
            <svg
              viewBox={`0 0 ${W} ${H}`}
              className="h-auto w-full rounded-lg border"
              role="img"
              aria-label={t("Treemap of {name}, by {by}", { name: here.name, by: by === "bytes" ? t("size") : t("number of files") })}
            >
              {blocks.map(({ item, x, y, w, h }) => {
                const enter = item.kind === "folder" && item.children.length > 0;
                const dark = item.kind !== "folder";
                return (
                  <g
                    key={`${item.id ?? item.kind}-${item.name}`}
                    onMouseEnter={() => setHover(item)}
                    onMouseLeave={() => setHover(null)}
                    onClick={() => goInto(item)}
                    className={enter ? "cursor-pointer" : undefined}
                  >
                    <title>{`${label(item)}: ${format(item)} (${share(value(item), total)})`}</title>
                    <rect
                      x={x + 1}
                      y={y + 1}
                      width={Math.max(0, w - 2)}
                      height={Math.max(0, h - 2)}
                      rx={3}
                      fill={dark ? "hsl(220 8% 45%)" : `hsl(${hue(item.name)} 55% 48%)`}
                      opacity={hover && hover !== item ? 0.75 : 1}
                    />
                    {w >= LABEL_MIN.w && h >= LABEL_MIN.h && (
                      <text x={x + 7} y={y + 17} fontSize={12} fill="white" pointerEvents="none">
                        {label(item).slice(0, Math.max(3, Math.floor(w / 7.5)))}
                        <tspan x={x + 7} y={y + 31} fontSize={10.5} opacity={0.85}>
                          {format(item)}
                        </tspan>
                      </text>
                    )}
                  </g>
                );
              })}
            </svg>
          )}
          <p className="mt-1 min-h-4 text-xs text-muted-foreground" aria-live="polite">
            {hover ? `${label(hover)}: ${format(hover)} (${share(value(hover), total)})` : t("Click a folder to go into it.")}
          </p>
        </div>

        <ul className="grid content-start gap-0.5 text-sm" aria-label={t("Folders and their sizes")}>
          {here.children.map((n) => (
            <li key={`${n.id ?? n.kind}-${n.name}`}>
              <button
                type="button"
                disabled={!(n.kind === "folder" && n.children.length > 0)}
                onClick={() => goInto(n)}
                onMouseEnter={() => setHover(n)}
                onMouseLeave={() => setHover(null)}
                className={cn("flex w-full items-center justify-between gap-3 rounded px-2 py-1 text-left", n.kind === "folder" && n.children.length > 0 && "hover:bg-muted")}
              >
                <span className="flex min-w-0 items-center gap-2">
                  <span className="size-2.5 shrink-0 rounded-sm" style={{ background: n.kind === "folder" ? `hsl(${hue(n.name)} 55% 48%)` : "hsl(220 8% 45%)" }} />
                  <span className="truncate">{label(n)}</span>
                </span>
                <span className="shrink-0 text-xs text-muted-foreground tabular-nums">
                  {by === "bytes" ? formatBytes(n.bytes) : count(n.files)} · {share(value(n), total)}
                </span>
              </button>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
