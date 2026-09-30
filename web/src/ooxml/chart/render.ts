/**
 * Overall layout of regular charts (c:chartSpace): background, title, legend and plot area, then hands off to the per-type renderers.
 */

import { kid, s, toggle } from "../core/package";
import { fontStack, schemeColor, themeFont, type ColorContext } from "../core/theme";
import { cartesianLegend, legendReversed, renderCartesian, seriesLegend } from "./cartesian";
import type { Rect } from "./axes";
import { buildLegend, manualRect, type LegendEntry } from "./legend";
import type { ChartModel, Group } from "./model";
import type { Ctx } from "./paint";
import { pieLegend, renderPie } from "./pie";
import { renderRadar } from "./radar";
import { lumModOff, readFill, r2, strokeAttrs, strokeOr } from "./style";
import { drawBlock, txPrStyle, type TextStyle } from "./text";
import { titleBlock } from "./title";

let counter = 0;

/** Create the SVG root element and drawing context */
export function createCanvas(W: number, H: number, colors: ColorContext, txPr: Element | null, transparent: boolean) {
  const n = ++counter;
  let k = 0;
  const svg = s("svg", {
    xmlns: "http://www.w3.org/2000/svg",
    width: W,
    height: H,
    viewBox: `0 0 ${W} ${H}`,
    style: `display:block;width:${W}px;height:${H}px;overflow:hidden`,
  });
  const defs = s("defs");
  svg.append(defs);
  const theme = colors.theme;
  // Default text color: tx1 at 65% luminance + 35% (also works when tx1 is light in dark themes)
  const tx1 = schemeColor("tx1", colors);
  const base0: TextStyle = {
    size: 10,
    bold: false,
    italic: false,
    underline: false,
    strike: false,
    color: tx1 ? lumModOff(tx1, 0.65, 0.35) : "#595959",
    font: fontStack(themeFont("+mn-lt", theme) ?? "Calibri", themeFont("+mn-ea", theme)),
  };
  const ctx: Ctx = { colors, defs, uid: () => `chart${n}-${++k}`, base: base0, blanks: "gap", autoFill: transparent ? null : "#FFFFFF" };
  ctx.base = txPrStyle(base0, txPr, ctx);
  svg.setAttribute("font-family", ctx.base.font);
  return { svg, ctx };
}

/** Chart area background and outline (spPr of chartSpace/cx:chartSpace) */
export function drawBackground(svg: SVGElement, spPr: Element | null, W: number, H: number, ctx: Ctx, rounded: boolean) {
  const fill = readFill(spPr, ctx);
  const stroke = strokeOr(spPr, ctx.colors, null);
  const inset = stroke ? stroke.width / 2 : 0;
  svg.append(
    s("rect", {
      x: r2(inset),
      y: r2(inset),
      width: r2(W - inset * 2),
      height: r2(H - inset * 2),
      rx: rounded && stroke ? 8 : undefined,
      fill: (fill === undefined ? ctx.autoFill : fill) ?? "none",
      ...strokeAttrs(stroke),
    }),
  );
}

export function renderClassic(model: ChartModel, W: number, H: number, colors: ColorContext, transparent: boolean): SVGElement {
  const { svg, ctx } = createCanvas(W, H, colors, kid(model.space, "txPr"), transparent);
  ctx.blanks = model.dispBlanksAs;
  const rounded = kid(model.space, "roundedCorners") ? !!toggle(kid(model.space, "roundedCorners")) : false;
  drawBackground(svg, kid(model.space, "spPr"), W, H, ctx, rounded);

  const pad = Math.max(4, Math.min(10, Math.min(W, H) * 0.03));
  let area: Rect = { x: pad, y: pad, w: W - pad * 2, h: H - pad * 2 };
  const groups = model.groups.filter((g) => g.series.length);
  const chart = model.chart;

  // Title: without text (auto title), the series name is shown only when there is a single series
  const titleEl = kid(chart, "title");
  const autoDeleted = !!toggle(kid(chart, "autoTitleDeleted"));
  const allSeries = groups.flatMap((g) => g.series);
  const titleStyle: TextStyle = { ...ctx.base, size: Math.round(ctx.base.size * 1.4 * 10) / 10, bold: true };
  const title = titleEl && !autoDeleted ? titleBlock(titleEl, titleStyle, ctx, allSeries.length === 1 ? allSeries[0].name : null, W * 0.85) : null;
  let drawTitle: (() => void) | null = null;
  if (title) {
    const manual = manualRect(kid(titleEl, "layout"), W, H, { x: (W - title.w) / 2, y: pad, w: title.w, h: title.h });
    const overlay = !!toggle(kid(titleEl, "overlay"));
    const box = manual ?? { x: (W - title.w) / 2, y: pad, w: title.w, h: title.h };
    if (!manual && !overlay) {
      area = { ...area, y: area.y + title.h + 4, h: area.h - title.h - 4 };
    }
    const sp = kid(titleEl, "spPr");
    drawTitle = () => {
      const fill = readFill(sp, ctx);
      const st = strokeOr(sp, ctx.colors, null);
      if (fill || st) svg.append(s("rect", { x: r2(box.x - 3), y: r2(box.y - 2), width: r2(title.w + 6), height: r2(title.h + 4), fill: fill ?? "none", ...strokeAttrs(st) }));
      svg.append(drawBlock(title, box.x + title.w / 2, box.y, "middle", "top"));
    };
  }

  // Legend
  const pies = groups.filter((g) => g.kind === "pie" || g.kind === "doughnut");
  const radars = groups.filter((g) => g.kind === "radar");
  const others = groups.filter((g) => g.kind !== "pie" && g.kind !== "doughnut" && g.kind !== "radar" && g.kind !== "surface");
  let entries: LegendEntry[] = [];
  if (others.length) entries = cartesianLegend(others, ctx);
  // Pie chart legends always list categories (whether or not colors vary by point)
  else if (pies.length) entries = pieLegend(pies[0], ctx);
  else if (radars.length) entries = radarLegend(radars, ctx);
  const legend = buildLegend(kid(chart, "legend"), entries, area, W, H, ctx, legendReversed(others));
  if (legend?.take) {
    const { side, size } = legend.take;
    if (side === "b") area = { ...area, h: area.h - size };
    else if (side === "t") area = { ...area, y: area.y + size, h: area.h - size };
    else if (side === "l") area = { ...area, x: area.x + size, w: area.w - size };
    else area = { ...area, w: area.w - size };
  }

  // Manual layout of the plot area
  const layout = kid(model.plotArea, "layout");
  const manual = manualRect(layout, W, H, null);
  const target = kid(kid(layout, "manualLayout"), "layoutTarget")?.getAttribute("val");
  let inner: Rect | null = null;
  if (manual) {
    if (target === "inner") inner = manual;
    else area = manual;
  }

  const plot = s("g");
  if (others.length) renderCartesian(model, others, area, inner, plot, ctx);
  else if (pies.length) renderPie(pies[0], inner ?? area, plot, ctx);
  else if (radars.length) renderRadar(model, radars, inner ?? area, plot, ctx);
  svg.append(plot);
  if (legend) {
    const lg = s("g");
    legend.draw(lg, area);
    svg.append(lg);
  }
  drawTitle?.();
  return svg;
}

function radarLegend(groups: Group[], ctx: Ctx): LegendEntry[] {
  return groups.flatMap((g) => g.series.map((ser) => seriesLegend(g, ser, ctx)));
}
