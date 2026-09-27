/**
 * Charts (DrawingML Chart, c:chartSpace; plus extended charts cx:chartSpace) → SVG. Shared by Word, PowerPoint and Excel previews.
 * Uses only the values cached in the file (numCache/strCache); the embedded workbook is not opened.
 *
 * Modules:
 * - model: parses chart groups, series, axes
 * - render: overall layout (background, title, legend, plot area)
 * - cartesian/axes/scale: cartesian charts and axis ticks
 * - pie/radar: pie, doughnut and radar charts
 * - extended: waterfall, funnel, treemap, sunburst, histogram, box & whisker
 * - labels/legend/title/text/style/paint/extras: data labels, legend, title, text, appearance, trendlines and error bars
 */

import type { OoxmlPackage } from "@/lib/office/ooxml";
import { h } from "@/lib/office/ooxml";
import type { ColorContext } from "@/lib/office/theme";
import { renderChartEx } from "./extended";
import { parseChart } from "./model";
import { renderClassic } from "./render";

export interface ChartOptions {
  /** Chart frame size (px) */
  width: number;
  height: number;
  /** Theme and color mapping (series colors, text colors) */
  colors: ColorContext;
}

function emptyBox(opts: ChartOptions) {
  return h("div", { style: `width:${opts.width}px;height:${opts.height}px;border:1px dashed #bbb;box-sizing:border-box` });
}

/** Chart part (e.g. ppt/charts/chart1.xml) → display element; returns a blank frame when it cannot be parsed */
export async function renderChart(pkg: OoxmlPackage, chartPath: string, opts: ChartOptions): Promise<HTMLElement | SVGElement> {
  const W = Math.max(1, Math.round(opts.width));
  const H = Math.max(1, Math.round(opts.height));
  try {
    const doc = await pkg.xml(chartPath);
    const root = doc?.documentElement;
    if (!doc || !root) return emptyBox(opts);
    // PowerPoint chart areas have no background by default (the slide background shows); Excel and Word default to white
    const transparent = chartPath.startsWith("ppt/");
    if (/chartex/i.test(root.namespaceURI ?? "")) return renderChartEx(doc, W, H, opts.colors, transparent);
    const model = parseChart(doc);
    if (!model) return emptyBox(opts);
    return renderClassic(model, W, H, opts.colors, transparent);
  } catch (e) {
    console.warn("chart render failed", chartPath, e);
    return emptyBox(opts);
  }
}
