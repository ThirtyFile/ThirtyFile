/**
 * Trendlines (c:trendline) and error bars (c:errBars).
 */

import { attr, kid, kids, numAttr, s, toggle } from "../core/package";
import type { Series } from "./model";
import type { Ctx } from "./paint";
import { r2, strokeAttrs, strokeOr } from "./style";

type ToPx = (x: number, y: number) => [number, number];

/** Solve polynomial coefficients by least squares (from low to high degree) */
function polyFit(xs: number[], ys: number[], order: number): number[] | null {
  const n = order + 1;
  const a: number[][] = Array.from({ length: n }, () => new Array<number>(n + 1).fill(0));
  for (let k = 0; k < xs.length; k++) {
    const pw: number[] = [1];
    for (let i = 1; i < 2 * n; i++) pw.push(pw[i - 1] * xs[k]);
    for (let i = 0; i < n; i++) {
      for (let j = 0; j < n; j++) a[i][j] += pw[i + j];
      a[i][n] += pw[i] * ys[k];
    }
  }
  // Gaussian elimination
  for (let i = 0; i < n; i++) {
    let best = i;
    for (let r = i + 1; r < n; r++) if (Math.abs(a[r][i]) > Math.abs(a[best][i])) best = r;
    [a[i], a[best]] = [a[best], a[i]];
    if (Math.abs(a[i][i]) < 1e-12) return null;
    for (let r = 0; r < n; r++) {
      if (r === i) continue;
      const f = a[r][i] / a[i][i];
      for (let c = i; c <= n; c++) a[r][c] -= f * a[i][c];
    }
  }
  return a.map((row, i) => row[n] / row[i]);
}

/** Trendlines: linear, exponential, logarithmic, polynomial, power, moving average */
export function drawTrendlines(ser: Series, xs: (number | null)[], toPx: ToPx, color: string, ctx: Ctx, g: SVGElement) {
  const trends = kids(ser.el, "trendline");
  if (!trends.length) return;
  const px: number[] = [];
  const py: number[] = [];
  ser.vals.forEach((y, i) => {
    const x = xs[i];
    if (y !== null && x !== null && x !== undefined) {
      px.push(x);
      py.push(y);
    }
  });
  if (px.length < 2) return;
  for (const t of trends) {
    const type = attr(kid(t, "trendlineType"), "val") ?? "linear";
    const st = strokeOr(kid(t, "spPr"), ctx.colors, { color, width: 2, dash: "2 2", cap: "round" });
    if (!st || !st.color) continue;
    const fwd = numAttr(kid(t, "forward"), "val") ?? 0;
    const back = numAttr(kid(t, "backward"), "val") ?? 0;
    const x0 = Math.min(...px) - back;
    const x1 = Math.max(...px) + fwd;
    let f: ((x: number) => number) | null = null;
    const pts: [number, number][] = [];
    if (type === "movingAvg") {
      const period = Math.max(2, numAttr(kid(t, "period"), "val") ?? 2);
      for (let i = period - 1; i < py.length; i++) {
        const avg = py.slice(i - period + 1, i + 1).reduce((a, b) => a + b, 0) / period;
        pts.push(toPx(px[i], avg));
      }
    } else if (type === "linear" || type === "poly") {
      const order = type === "poly" ? Math.min(6, Math.max(2, numAttr(kid(t, "order"), "val") ?? 2)) : 1;
      const icpt = numAttr(kid(t, "intercept"), "val");
      if (icpt !== null && order === 1) {
        // Fixed intercept: solve for slope only
        let sxy = 0;
        let sxx = 0;
        px.forEach((x, i) => {
          sxy += x * (py[i] - icpt);
          sxx += x * x;
        });
        const b = sxx ? sxy / sxx : 0;
        f = (x) => icpt + b * x;
      } else {
        const c = polyFit(px, py, order);
        if (c) f = (x) => c.reduce((sum, k, i) => sum + k * Math.pow(x, i), 0);
      }
    } else if (type === "exp" || type === "power" || type === "log") {
      const lx = type === "exp" ? px : px.map((x) => (x > 0 ? Math.log(x) : NaN));
      const ly = type === "log" ? py : py.map((y) => (y > 0 ? Math.log(y) : NaN));
      const ok = lx.map((x, i) => Number.isFinite(x) && Number.isFinite(ly[i]));
      const c = polyFit(
        lx.filter((_, i) => ok[i]),
        ly.filter((_, i) => ok[i]),
        1,
      );
      if (c) {
        if (type === "exp") f = (x) => Math.exp(c[0] + c[1] * x);
        else if (type === "power") f = (x) => (x > 0 ? Math.exp(c[0]) * Math.pow(x, c[1]) : NaN);
        else f = (x) => (x > 0 ? c[0] + c[1] * Math.log(x) : NaN);
      }
    }
    if (f) {
      const steps = 60;
      for (let k = 0; k <= steps; k++) {
        const x = x0 + ((x1 - x0) * k) / steps;
        const y = f(x);
        if (Number.isFinite(y)) pts.push(toPx(x, y));
      }
    }
    if (pts.length < 2) continue;
    g.append(s("path", { d: pts.map((p, i) => `${i ? "L" : "M"}${r2(p[0])},${r2(p[1])}`).join(""), fill: "none", ...strokeAttrs(st) }));
  }
}

/** Custom error amounts (cached values of c:plus/c:minus) */
function custom(el: Element | null): (number | null)[] {
  const src = kid(el, "numRef") ? kid(kid(el, "numRef"), "numCache") : kid(el, "numLit");
  const out: (number | null)[] = [];
  for (const pt of kids(src, "pt")) out[numAttr(pt, "idx") ?? 0] = Number(kid(pt, "v")?.textContent ?? "NaN");
  return out;
}

/** Error bars; when dir is given (category charts) only that direction is drawn */
export function drawErrBars(ser: Series, xs: (number | null)[], toPx: ToPx, dir: "x" | "y" | null, ctx: Ctx, g: SVGElement) {
  for (const eb of kids(ser.el, "errBars")) {
    const d = dir ?? (attr(kid(eb, "errDir"), "val") === "x" ? "x" : "y");
    const type = attr(kid(eb, "errBarType"), "val") ?? "both";
    const valType = attr(kid(eb, "errValType"), "val") ?? "fixedVal";
    // Without c:val, some generators write the values in plus/minus
    const val = numAttr(kid(eb, "val"), "val") ?? custom(kid(eb, "plus"))[0] ?? (valType === "percentage" ? 5 : 1);
    const noCap = !!toggle(kid(eb, "noEndCap"));
    const st = strokeOr(kid(eb, "spPr"), ctx.colors, { color: "#000000", width: 1 });
    if (!st || !st.color) continue;
    const pts: { x: number; y: number }[] = [];
    ser.vals.forEach((y, i) => {
      const x = xs[i];
      if (y !== null && x !== null && x !== undefined) pts.push({ x, y });
    });
    if (!pts.length) continue;
    const data = pts.map((p) => (d === "x" ? p.x : p.y));
    const mean = data.reduce((a, b) => a + b, 0) / data.length;
    const sd = Math.sqrt(data.reduce((a, b) => a + (b - mean) * (b - mean), 0) / Math.max(data.length - 1, 1));
    const plus = valType === "cust" ? custom(kid(eb, "plus")) : [];
    const minus = valType === "cust" ? custom(kid(eb, "minus")) : [];
    let path = "";
    ser.vals.forEach((y, i) => {
      const x = xs[i];
      if (y === null || x === null || x === undefined) return;
      const v = d === "x" ? x : y;
      let center = v;
      let up: number;
      let down: number;
      switch (valType) {
        case "percentage":
          up = down = (Math.abs(v) * val) / 100;
          break;
        case "stdDev":
          // Standard deviation is centered on the mean
          center = mean;
          up = down = sd * val;
          break;
        case "stdErr":
          up = down = sd / Math.sqrt(data.length);
          break;
        case "cust":
          up = plus[i] ?? 0;
          down = minus[i] ?? 0;
          break;
        default:
          up = down = val;
      }
      const hi = type === "minus" ? center : center + (Number.isFinite(up) ? up : 0);
      const lo = type === "plus" ? center : center - (Number.isFinite(down) ? down : 0);
      const a = d === "x" ? toPx(hi, y) : toPx(x, hi);
      const b = d === "x" ? toPx(lo, y) : toPx(x, lo);
      path += `M${r2(a[0])},${r2(a[1])}L${r2(b[0])},${r2(b[1])}`;
      if (!noCap) {
        const vertical = Math.abs(a[0] - b[0]) < Math.abs(a[1] - b[1]);
        for (const [e, keep] of [
          [a, type !== "minus"],
          [b, type !== "plus"],
        ] as const) {
          if (!keep) continue;
          path += vertical ? `M${r2(e[0] - 3)},${r2(e[1])}h6` : `M${r2(e[0])},${r2(e[1] - 3)}v6`;
        }
      }
    });
    if (path) g.append(s("path", { d: path, fill: "none", ...strokeAttrs(st) }));
  }
}
