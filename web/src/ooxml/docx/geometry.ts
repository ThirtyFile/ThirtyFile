/**
 * Outline paths (SVG path d) for DrawingML shapes: common preset geometries (prstGeom) and custom geometry (custGeom).
 * Coordinates are in px; unknown presets are rendered as rectangles.
 */

import { attr, kid, kids, numAttr } from "../core/package";

export interface ShapePath {
  d: string;
  /** Line-type shapes (no fill) */
  open?: boolean;
}

const r2 = (n: number) => Math.round(n * 100) / 100;
const P = (x: number, y: number) => `${r2(x)},${r2(y)}`;

function poly(pts: [number, number][], close = true) {
  return `M${pts.map(([x, y]) => P(x, y)).join(" L")}${close ? " Z" : ""}`;
}

function ellipse(cx: number, cy: number, rx: number, ry: number) {
  return `M${P(cx - rx, cy)} A${r2(rx)},${r2(ry)} 0 1,0 ${P(cx + rx, cy)} A${r2(rx)},${r2(ry)} 0 1,0 ${P(cx - rx, cy)} Z`;
}

function roundRect(w: number, h: number, r: number) {
  const k = Math.max(0, Math.min(r, w / 2, h / 2));
  if (!k) return poly([[0, 0], [w, 0], [w, h], [0, h]]);
  return `M${P(k, 0)} L${P(w - k, 0)} A${r2(k)},${r2(k)} 0 0,1 ${P(w, k)} L${P(w, h - k)} A${r2(k)},${r2(k)} 0 0,1 ${P(w - k, h)} L${P(k, h)} A${r2(k)},${r2(k)} 0 0,1 ${P(0, h - k)} L${P(0, k)} A${r2(k)},${r2(k)} 0 0,1 ${P(k, 0)} Z`;
}

function star(w: number, h: number, points: number, inner: number) {
  const pts: [number, number][] = [];
  for (let i = 0; i < points * 2; i++) {
    const a = -Math.PI / 2 + (i * Math.PI) / points;
    const rr = i % 2 ? inner : 1;
    pts.push([w / 2 + (w / 2) * rr * Math.cos(a), h / 2 + (h / 2) * rr * Math.sin(a)]);
  }
  return poly(pts);
}

function regular(w: number, h: number, n: number) {
  const pts: [number, number][] = [];
  for (let i = 0; i < n; i++) {
    const a = -Math.PI / 2 + (i * 2 * Math.PI) / n;
    pts.push([w / 2 + (w / 2) * Math.cos(a), h / 2 + (h / 2) * Math.sin(a)]);
  }
  return poly(pts);
}

/** Read an avLst adjust value (val in units of 1/100000) */
export function adjustValues(prstGeom: Element | null): Record<string, number> {
  const out: Record<string, number> = {};
  for (const gd of kids(kid(prstGeom, "avLst"), "gd")) {
    const m = /^val\s+(-?\d+)/.exec(attr(gd, "fmla") ?? "");
    const name = attr(gd, "name");
    if (m && name) out[name] = Number(m[1]);
  }
  return out;
}

export function presetPath(prst: string, w: number, h: number, av: Record<string, number>): ShapePath {
  const ss = Math.min(w, h);
  const a = (name: string, def: number) => (av[name] ?? def) / 100000;
  switch (prst) {
    case "rect":
    case "flowChartProcess":
    case "textBox":
      return { d: poly([[0, 0], [w, 0], [w, h], [0, h]]) };
    case "roundRect":
    case "flowChartAlternateProcess":
      return { d: roundRect(w, h, ss * a("adj", 16667)) };
    case "round2SameRect":
    case "round1Rect":
      return { d: roundRect(w, h, ss * a("adj1", 16667)) };
    case "flowChartTerminator":
      return { d: roundRect(w, h, Math.min(w, h) / 2) };
    case "snip1Rect": {
      const k = ss * a("adj", 16667);
      return { d: poly([[0, 0], [w - k, 0], [w, k], [w, h], [0, h]]) };
    }
    case "snip2SameRect": {
      const k = ss * a("adj1", 16667);
      return { d: poly([[k, 0], [w - k, 0], [w, k], [w, h], [0, h], [0, k]]) };
    }
    case "ellipse":
    case "flowChartConnector":
    case "cloud":
    case "cloudCallout":
      return { d: ellipse(w / 2, h / 2, w / 2, h / 2) };
    case "donut": {
      const k = ss * a("adj", 25000);
      return { d: `${ellipse(w / 2, h / 2, w / 2, h / 2)} ${ellipse(w / 2, h / 2, Math.max(0, w / 2 - k), Math.max(0, h / 2 - k))}` };
    }
    case "triangle":
    case "flowChartExtract": {
      const x = w * a("adj", 50000);
      return { d: poly([[x, 0], [w, h], [0, h]]) };
    }
    case "flowChartMerge":
      return { d: poly([[0, 0], [w, 0], [w / 2, h]]) };
    case "rtTriangle":
      return { d: poly([[0, 0], [w, h], [0, h]]) };
    case "diamond":
    case "flowChartDecision":
      return { d: poly([[w / 2, 0], [w, h / 2], [w / 2, h], [0, h / 2]]) };
    case "parallelogram":
    case "flowChartInputOutput": {
      const k = prst === "parallelogram" ? ss * a("adj", 25000) : w / 5;
      return { d: poly([[k, 0], [w, 0], [w - k, h], [0, h]]) };
    }
    case "trapezoid": {
      const k = ss * a("adj", 25000);
      return { d: poly([[k, 0], [w - k, 0], [w, h], [0, h]]) };
    }
    case "flowChartManualOperation":
      return { d: poly([[0, 0], [w, 0], [w * 0.8, h], [w * 0.2, h]]) };
    case "flowChartManualInput":
      return { d: poly([[0, h * 0.2], [w, 0], [w, h], [0, h]]) };
    case "pentagon":
      return { d: regular(w, h, 5) };
    case "hexagon":
    case "flowChartPreparation": {
      const k = prst === "hexagon" ? ss * a("adj", 25000) : w / 5;
      return { d: poly([[k, 0], [w - k, 0], [w, h / 2], [w - k, h], [k, h], [0, h / 2]]) };
    }
    case "heptagon":
      return { d: regular(w, h, 7) };
    case "octagon": {
      const k = ss * a("adj", 29289);
      return { d: poly([[k, 0], [w - k, 0], [w, k], [w, h - k], [w - k, h], [k, h], [0, h - k], [0, k]]) };
    }
    case "decagon":
      return { d: regular(w, h, 10) };
    case "dodecagon":
      return { d: regular(w, h, 12) };
    case "plus":
    case "mathPlus": {
      const k = ss * a("adj", 25000);
      return { d: poly([[k, 0], [w - k, 0], [w - k, k], [w, k], [w, h - k], [w - k, h - k], [w - k, h], [k, h], [k, h - k], [0, h - k], [0, k], [k, k]]) };
    }
    case "star4":
      return { d: star(w, h, 4, a("adj", 12500) * 2) };
    case "star5":
      return { d: star(w, h, 5, 0.38) };
    case "star6":
      return { d: star(w, h, 6, 0.58) };
    case "star7":
      return { d: star(w, h, 7, 0.6) };
    case "star8":
      return { d: star(w, h, 8, 0.7) };
    case "star10":
    case "star12":
    case "star16":
    case "star24":
    case "star32":
      return { d: star(w, h, Number(prst.slice(4)), 0.8) };
    case "irregularSeal1":
    case "irregularSeal2":
      return { d: star(w, h, 12, 0.7) };
    case "rightArrow":
    case "leftArrow": {
      const t = h * a("adj1", 50000);
      const hl = ss * a("adj2", 50000);
      const y1 = (h - t) / 2;
      const y2 = y1 + t;
      const pts: [number, number][] = [[0, y1], [w - hl, y1], [w - hl, 0], [w, h / 2], [w - hl, h], [w - hl, y2], [0, y2]];
      return { d: poly(prst === "leftArrow" ? pts.map(([x, y]) => [w - x, y]) : pts) };
    }
    case "upArrow":
    case "downArrow": {
      const t = w * a("adj1", 50000);
      const hl = ss * a("adj2", 50000);
      const x1 = (w - t) / 2;
      const x2 = x1 + t;
      const pts: [number, number][] = [[x1, h], [x1, hl], [0, hl], [w / 2, 0], [w, hl], [x2, hl], [x2, h]];
      return { d: poly(prst === "downArrow" ? pts.map(([x, y]) => [x, h - y]) : pts) };
    }
    case "leftRightArrow": {
      const t = h * a("adj1", 50000);
      const hl = ss * a("adj2", 50000);
      const y1 = (h - t) / 2;
      const y2 = y1 + t;
      return { d: poly([[0, h / 2], [hl, 0], [hl, y1], [w - hl, y1], [w - hl, 0], [w, h / 2], [w - hl, h], [w - hl, y2], [hl, y2], [hl, h]]) };
    }
    case "upDownArrow": {
      const t = w * a("adj1", 50000);
      const hl = ss * a("adj2", 50000);
      const x1 = (w - t) / 2;
      const x2 = x1 + t;
      return { d: poly([[w / 2, 0], [w, hl], [x2, hl], [x2, h - hl], [w, h - hl], [w / 2, h], [0, h - hl], [x1, h - hl], [x1, hl], [0, hl]]) };
    }
    case "chevron": {
      const k = ss * a("adj", 50000);
      return { d: poly([[0, 0], [w - k, 0], [w, h / 2], [w - k, h], [0, h], [k, h / 2]]) };
    }
    case "homePlate":
    case "flowChartOffpageConnector": {
      if (prst === "flowChartOffpageConnector") return { d: poly([[0, 0], [w, 0], [w, h * 0.8], [w / 2, h], [0, h * 0.8]]) };
      const k = ss * a("adj", 50000);
      return { d: poly([[0, 0], [w - k, 0], [w, h / 2], [w - k, h], [0, h]]) };
    }
    case "frame": {
      const k = ss * a("adj1", 12500);
      return { d: `${poly([[0, 0], [w, 0], [w, h], [0, h]])} ${poly([[k, k], [k, h - k], [w - k, h - k], [w - k, k]])}` };
    }
    case "flowChartDocument": {
      return { d: `M0,0 L${P(w, 0)} L${P(w, h * 0.83)} C${P(w * 0.75, h * 0.7)} ${P(w * 0.5, h * 1.05)} ${P(0, h * 0.92)} Z` };
    }
    case "flowChartPredefinedProcess":
      return { d: `${poly([[0, 0], [w, 0], [w, h], [0, h]])} M${P(w / 8, 0)} L${P(w / 8, h)} M${P((w * 7) / 8, 0)} L${P((w * 7) / 8, h)}` };
    case "can":
    case "flowChartMagneticDisk": {
      const ry = Math.min(h / 4, ss * a("adj", 25000) / 2);
      return {
        d: `M0,${r2(ry)} A${r2(w / 2)},${r2(ry)} 0 0,1 ${P(w, ry)} L${P(w, h - ry)} A${r2(w / 2)},${r2(ry)} 0 0,1 ${P(0, h - ry)} Z M0,${r2(ry)} A${r2(w / 2)},${r2(ry)} 0 0,0 ${P(w, ry)}`,
      };
    }
    case "cube": {
      const k = ss * a("adj", 25000);
      return { d: `${poly([[0, k], [k, 0], [w, 0], [w, h - k], [w - k, h], [0, h]])} M0,${r2(k)} L${P(w - k, k)} L${P(w, 0)} M${P(w - k, k)} L${P(w - k, h)}` };
    }
    case "heart":
      return {
        d: `M${P(w / 2, h * 0.25)} C${P(w / 2, 0)} ${P(0, 0)} ${P(0, h * 0.3)} C${P(0, h * 0.6)} ${P(w / 2, h * 0.8)} ${P(w / 2, h)} C${P(w / 2, h * 0.8)} ${P(w, h * 0.6)} ${P(w, h * 0.3)} C${P(w, 0)} ${P(w / 2, 0)} ${P(w / 2, h * 0.25)} Z`,
      };
    case "wedgeRectCallout":
    case "wedgeRoundRectCallout":
    case "wedgeEllipseCallout": {
      const tx = w / 2 + w * a("adj1", -20833);
      const ty = h / 2 + h * a("adj2", 62500);
      const body = prst === "wedgeEllipseCallout" ? ellipse(w / 2, h / 2, w / 2, h / 2) : prst === "wedgeRoundRectCallout" ? roundRect(w, h, ss * 0.1667) : poly([[0, 0], [w, 0], [w, h], [0, h]]);
      // Pointer: draw a triangle from the nearest edge
      const bx = Math.max(0, Math.min(w, tx));
      const by = ty > h ? h : ty < 0 ? 0 : h / 2;
      const spread = Math.min(w, h) * 0.12;
      const tail = ty > h || ty < 0 ? poly([[bx - spread, by], [tx, ty], [bx + spread, by]]) : poly([[bx, h / 2 - spread], [tx, ty], [bx, h / 2 + spread]]);
      return { d: `${body} ${tail}` };
    }
    case "line":
    case "straightConnector1":
      return { d: `M0,0 L${P(w, h)}`, open: true };
    case "bentConnector2":
      return { d: `M0,0 L${P(w, 0)} L${P(w, h)}`, open: true };
    case "bentConnector3": {
      const x = w * a("adj1", 50000);
      return { d: `M0,0 L${P(x, 0)} L${P(x, h)} L${P(w, h)}`, open: true };
    }
    case "bentConnector4":
    case "bentConnector5":
      return { d: `M0,0 L${P(w / 2, 0)} L${P(w / 2, h)} L${P(w, h)}`, open: true };
    case "curvedConnector2":
    case "curvedConnector3":
    case "curvedConnector4":
    case "curvedConnector5":
      return { d: `M0,0 C${P(w / 2, 0)} ${P(w / 2, h)} ${P(w, h)}`, open: true };
    case "arc": {
      return { d: `M${P(w / 2, 0)} A${r2(w / 2)},${r2(h / 2)} 0 0,1 ${P(w, h / 2)}`, open: true };
    }
    case "leftBracket":
      return { d: `M${P(w, 0)} Q0,0 0,${r2(h * 0.1)} L0,${r2(h * 0.9)} Q0,${r2(h)} ${P(w, h)}`, open: true };
    case "rightBracket":
      return { d: `M0,0 Q${P(w, 0)} ${P(w, h * 0.1)} L${P(w, h * 0.9)} Q${P(w, h)} 0,${r2(h)}`, open: true };
    case "bracketPair": {
      const k = ss * 0.1667;
      return { d: `M${P(k, 0)} Q0,0 0,${r2(k)} L0,${r2(h - k)} Q0,${r2(h)} ${P(k, h)} M${P(w - k, 0)} Q${P(w, 0)} ${P(w, k)} L${P(w, h - k)} Q${P(w, h)} ${P(w - k, h)}`, open: true };
    }
    case "leftBrace":
      return { d: `M${P(w, 0)} Q${P(w / 2, 0)} ${P(w / 2, h * 0.1)} L${P(w / 2, h * 0.4)} Q${P(w / 2, h / 2)} 0,${r2(h / 2)} Q${P(w / 2, h / 2)} ${P(w / 2, h * 0.6)} L${P(w / 2, h * 0.9)} Q${P(w / 2, h)} ${P(w, h)}`, open: true };
    case "rightBrace":
      return { d: `M0,0 Q${P(w / 2, 0)} ${P(w / 2, h * 0.1)} L${P(w / 2, h * 0.4)} Q${P(w / 2, h / 2)} ${P(w, h / 2)} Q${P(w / 2, h / 2)} ${P(w / 2, h * 0.6)} L${P(w / 2, h * 0.9)} Q${P(w / 2, h)} 0,${r2(h)}`, open: true };
    default:
      return { d: poly([[0, 0], [w, 0], [w, h], [0, h]]) };
  }
}

/** Custom geometry (custGeom): each path is scaled from its own w/h to the bounding box */
export function customPath(custGeom: Element, w: number, h: number): ShapePath {
  const parts: string[] = [];
  let open = true;
  for (const path of kids(kid(custGeom, "pathLst"), "path")) {
    const pw = numAttr(path, "w") || w;
    const ph = numAttr(path, "h") || h;
    const sx = w / pw;
    const sy = h / ph;
    const pt = (el: Element | null): [number, number] => [(numAttr(el, "x") ?? 0) * sx, (numAttr(el, "y") ?? 0) * sy];
    if (attr(path, "fill") !== "none") open = false;
    let cx = 0;
    let cy = 0;
    for (const c of kids(path)) {
      const pts = kids(c, "pt").map(pt);
      switch (c.localName) {
        case "moveTo":
          [cx, cy] = pts[0] ?? [0, 0];
          parts.push(`M${P(cx, cy)}`);
          break;
        case "lnTo":
          [cx, cy] = pts[0] ?? [cx, cy];
          parts.push(`L${P(cx, cy)}`);
          break;
        case "cubicBezTo":
          if (pts.length === 3) {
            parts.push(`C${pts.map(([x, y]) => P(x, y)).join(" ")}`);
            [cx, cy] = pts[2];
          }
          break;
        case "quadBezTo":
          if (pts.length === 2) {
            parts.push(`Q${pts.map(([x, y]) => P(x, y)).join(" ")}`);
            [cx, cy] = pts[1];
          }
          break;
        case "arcTo": {
          const wr = (numAttr(c, "wR") ?? 0) * sx;
          const hr = (numAttr(c, "hR") ?? 0) * sy;
          const st = (((numAttr(c, "stAng") ?? 0) / 60000) * Math.PI) / 180;
          const sw = (((numAttr(c, "swAng") ?? 0) / 60000) * Math.PI) / 180;
          const ox = cx - wr * Math.cos(st);
          const oy = cy - hr * Math.sin(st);
          const ex = ox + wr * Math.cos(st + sw);
          const ey = oy + hr * Math.sin(st + sw);
          parts.push(`A${r2(wr)},${r2(hr)} 0 ${Math.abs(sw) > Math.PI ? 1 : 0},${sw > 0 ? 1 : 0} ${P(ex, ey)}`);
          cx = ex;
          cy = ey;
          break;
        }
        case "close":
          parts.push("Z");
          break;
      }
    }
  }
  return { d: parts.join(" ") || poly([[0, 0], [w, 0], [w, h], [0, h]]), open };
}
