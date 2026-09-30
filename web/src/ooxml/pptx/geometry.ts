/**
 * DrawingML geometry: evaluates guide formulas (gd fmla) and converts path commands to SVG paths.
 * Preset shapes (defined in a compact text format in presets.ts) and custom shapes (a:custGeom) share the same evaluation pipeline.
 */

import { attr, kid, kids, numAttr } from "../core/package";

export type FillMode = "norm" | "none" | "lighten" | "lightenLess" | "darken" | "darkenLess";

/** Argument: numeric constant or guide name */
type Arg = string | number;

interface RawCmd {
  op: "M" | "L" | "C" | "Q" | "A" | "Z";
  args: Arg[];
}

interface RawPath {
  /** Width/height of the path coordinate system (when given, coordinates are scaled to the shape size) */
  w?: number;
  h?: number;
  fill: FillMode;
  stroke: boolean;
  cmds: RawCmd[];
}

export interface GeomDef {
  av: [string, Arg][];
  gd: [string, string, Arg[]][];
  paths: RawPath[];
  /** Text rectangle l t r b */
  text?: Arg[];
}

export interface OutPath {
  d: string;
  fill: FillMode;
  stroke: boolean;
}

export interface GeomOut {
  paths: OutPath[];
  /** Text rectangle (px, relative to the shape's top-left corner) */
  text: { x: number; y: number; w: number; h: number };
}

// ───────────── Parsing ─────────────

const NUM = /^-?\d+(\.\d+)?$/;
const toArg = (s: string): Arg => (NUM.test(s) ? Number(s) : s);
const OPS = new Set(["M", "L", "C", "Q", "A", "Z"]);

/**
 * Text format of preset shapes (separated by ; or newlines):
 *   av name default                     adjust value
 *   g name operator args…               guide
 *   p [none|nostroke|darken…|w=N|h=N]   start a new path
 *   M x y / L x y / C … / Q … / A wR hR stAng swAng / Z
 *   tx l t r b                          text rectangle
 */
export function parseDsl(src: string): GeomDef {
  const def: GeomDef = { av: [], gd: [], paths: [] };
  let cur: RawPath | null = null;
  for (const raw of src.split(/[;\n]/)) {
    const tok = raw.trim().split(/\s+/).filter(Boolean);
    if (!tok.length) continue;
    const head = tok[0];
    if (head === "av") def.av.push([tok[1], toArg(tok[2])]);
    else if (head === "g") def.gd.push([tok[1], tok[2], tok.slice(3).map(toArg)]);
    else if (head === "tx") def.text = tok.slice(1).map(toArg);
    else if (head === "p") {
      cur = { fill: "norm", stroke: true, cmds: [] };
      for (const o of tok.slice(1)) {
        if (o === "nostroke") cur.stroke = false;
        else if (o.startsWith("w=")) cur.w = Number(o.slice(2));
        else if (o.startsWith("h=")) cur.h = Number(o.slice(2));
        else cur.fill = o as FillMode;
      }
      def.paths.push(cur);
    } else {
      if (!cur) {
        cur = { fill: "norm", stroke: true, cmds: [] };
        def.paths.push(cur);
      }
      let cmd: RawCmd | null = null;
      for (const t of tok) {
        if (OPS.has(t)) {
          cmd = { op: t as RawCmd["op"], args: [] };
          cur.cmds.push(cmd);
        } else cmd?.args.push(toArg(t));
      }
    }
  }
  return def;
}

/** a:custGeom → geometry definition */
export function parseCustGeom(el: Element): GeomDef {
  const def: GeomDef = { av: [], gd: [], paths: [] };
  for (const g of kids(kid(el, "avLst"), "gd")) {
    const f = (attr(g, "fmla") ?? "").trim().split(/\s+/);
    def.av.push([attr(g, "name") ?? "", toArg(f[f.length - 1] ?? "0")]);
  }
  for (const g of kids(kid(el, "gdLst"), "gd")) {
    const f = (attr(g, "fmla") ?? "").trim().split(/\s+/);
    def.gd.push([attr(g, "name") ?? "", f[0] ?? "val", f.slice(1).map(toArg)]);
  }
  const rect = kid(el, "rect");
  if (rect) def.text = ["l", "t", "r", "b"].map((k) => toArg(attr(rect, k) ?? "0"));
  for (const p of kids(kid(el, "pathLst"), "path")) {
    const fill = (attr(p, "fill") ?? "norm") as FillMode;
    const path: RawPath = {
      w: numAttr(p, "w") ?? undefined,
      h: numAttr(p, "h") ?? undefined,
      fill,
      stroke: attr(p, "stroke") !== "0" && attr(p, "stroke") !== "false",
      cmds: [],
    };
    const pts = (c: Element) => kids(c, "pt").flatMap((pt) => [toArg(attr(pt, "x") ?? "0"), toArg(attr(pt, "y") ?? "0")]);
    for (const c of kids(p)) {
      switch (c.localName) {
        case "moveTo":
          path.cmds.push({ op: "M", args: pts(c) });
          break;
        case "lnTo":
          path.cmds.push({ op: "L", args: pts(c) });
          break;
        case "cubicBezTo":
          path.cmds.push({ op: "C", args: pts(c) });
          break;
        case "quadBezTo":
          path.cmds.push({ op: "Q", args: pts(c) });
          break;
        case "arcTo":
          path.cmds.push({ op: "A", args: ["wR", "hR", "stAng", "swAng"].map((k) => toArg(attr(c, k) ?? "0")) });
          break;
        case "close":
          path.cmds.push({ op: "Z", args: [] });
          break;
      }
    }
    def.paths.push(path);
  }
  return def;
}

/** Adjust values on the shape (gd in a:avLst, fmla="val 12345") */
export function readAdjust(avLst: Element | null): Map<string, number> | null {
  const list = kids(avLst, "gd");
  if (!list.length) return null;
  const m = new Map<string, number>();
  for (const g of list) {
    const f = /val\s+(-?[\d.]+)/.exec(attr(g, "fmla") ?? "");
    if (f) m.set(attr(g, "name") ?? "", Number(f[1]));
  }
  return m;
}

// ───────────── Evaluation ─────────────

/** Angle unit 1/60000 degree → radians */
const RAD = Math.PI / 10800000;

function builtins(w: number, h: number): Map<string, number> {
  const ss = Math.min(w, h);
  const m = new Map<string, number>([
    ["w", w], ["h", h], ["l", 0], ["t", 0], ["r", w], ["b", h], ["hc", w / 2], ["vc", h / 2],
    ["ss", ss], ["ls", Math.max(w, h)],
    ["cd2", 10800000], ["cd4", 5400000], ["cd8", 2700000], ["3cd4", 16200000], ["3cd8", 8100000], ["5cd8", 13500000], ["7cd8", 18900000],
  ]);
  for (const n of [2, 3, 4, 5, 6, 8, 10, 12, 32]) {
    m.set(`wd${n}`, w / n);
    m.set(`hd${n}`, h / n);
  }
  for (const n of [2, 4, 6, 8, 16, 32]) m.set(`ssd${n}`, ss / n);
  m.set("3wd4", (3 * w) / 4);
  m.set("3hd4", (3 * h) / 4);
  return m;
}

function formula(op: string, a: number[]): number {
  const [x = 0, y = 0, z = 0] = a;
  switch (op) {
    case "val":
      return x;
    case "*/":
      return z === 0 ? 0 : (x * y) / z;
    case "+-":
      return x + y - z;
    case "+/":
      return z === 0 ? 0 : (x + y) / z;
    case "?:":
      return x > 0 ? y : z;
    case "abs":
      return Math.abs(x);
    case "at2":
      return Math.atan2(y, x) / RAD;
    case "cat2":
      return x * Math.cos(Math.atan2(z, y));
    case "sat2":
      return x * Math.sin(Math.atan2(z, y));
    case "cos":
      return x * Math.cos(y * RAD);
    case "sin":
      return x * Math.sin(y * RAD);
    case "tan":
      return x * Math.tan(y * RAD);
    case "max":
      return Math.max(x, y);
    case "min":
      return Math.min(x, y);
    case "mod":
      return Math.sqrt(x * x + y * y + z * z);
    case "pin":
      return y < x ? x : y > z ? z : y;
    case "sqrt":
      return Math.sqrt(Math.max(0, x));
    default:
      return 0;
  }
}

const fmt = (n: number) => (Number.isFinite(n) ? String(Math.round(n * 100) / 100) : "0");

/** Point on the ellipse at "visual angle" ang (radians), relative to the center */
function ellipsePoint(wR: number, hR: number, ang: number): [number, number] {
  const t = Math.atan2(wR * Math.sin(ang), hR * Math.cos(ang));
  return [wR * Math.cos(t), hR * Math.sin(t)];
}

/**
 * Evaluate: wEmu/hEmu are the shape size (EMU); output coordinates are px.
 * adj holds the adjust values specified on the shape (overriding defaults).
 */
export function evaluate(def: GeomDef, wEmu: number, hEmu: number, adj: Map<string, number> | null): GeomOut {
  const env = builtins(wEmu, hEmu);
  const val = (a: Arg) => (typeof a === "number" ? a : (env.get(a) ?? (NUM.test(a) ? Number(a) : 0)));
  for (const [name, v] of def.av) env.set(name, adj?.get(name) ?? val(v));
  // For shapes with a single adjust value, files sometimes write adj1 or adj
  if (adj && def.av.length === 1) {
    const only = def.av[0][0];
    const alt = only === "adj" ? adj.get("adj1") : only === "adj1" ? adj.get("adj") : undefined;
    if (alt !== undefined && !adj.has(only)) env.set(only, alt);
  }
  for (const [name, op, args] of def.gd) env.set(name, formula(op, args.map(val)));

  const E = 1 / 9525;
  const paths: OutPath[] = [];
  for (const p of def.paths) {
    const sx = p.w ? (wEmu * E) / p.w : E;
    const sy = p.h ? (hEmu * E) / p.h : E;
    let d = "";
    let cx = 0;
    let cy = 0;
    for (const c of p.cmds) {
      const v = c.args.map(val);
      switch (c.op) {
        case "M":
        case "L":
          cx = v[0] ?? 0;
          cy = v[1] ?? 0;
          d += `${c.op}${fmt(cx * sx)} ${fmt(cy * sy)}`;
          break;
        case "C":
        case "Q": {
          const n = c.op === "C" ? 6 : 4;
          const pts: string[] = [];
          for (let i = 0; i < n; i += 2) pts.push(`${fmt((v[i] ?? 0) * sx)} ${fmt((v[i + 1] ?? 0) * sy)}`);
          cx = v[n - 2] ?? 0;
          cy = v[n - 1] ?? 0;
          d += `${c.op}${pts.join(" ")}`;
          break;
        }
        case "A": {
          const [wR = 0, hR = 0, st = 0, sw = 0] = v;
          const s0 = st * RAD;
          const [px0, py0] = ellipsePoint(wR, hR, s0);
          const ox = cx - px0;
          const oy = cy - py0;
          if (!wR || !hR || !sw) {
            const [ex, ey] = ellipsePoint(wR, hR, (st + sw) * RAD);
            cx = ox + ex;
            cy = oy + ey;
            d += `L${fmt(cx * sx)} ${fmt(cy * sy)}`;
            break;
          }
          // Each segment spans at most 90 degrees, avoiding ambiguity in the SVG large-arc flag
          const n = Math.max(1, Math.ceil(Math.abs(sw) / 5400000));
          for (let i = 1; i <= n; i++) {
            const [ex, ey] = ellipsePoint(wR, hR, (st + (sw * i) / n) * RAD);
            cx = ox + ex;
            cy = oy + ey;
            d += `A${fmt(wR * sx)} ${fmt(hR * sy)} 0 0 ${sw > 0 ? 1 : 0} ${fmt(cx * sx)} ${fmt(cy * sy)}`;
          }
          break;
        }
        case "Z":
          d += "Z";
          break;
      }
    }
    if (d) paths.push({ d, fill: p.fill, stroke: p.stroke });
  }

  const W = wEmu * E;
  const H = hEmu * E;
  let text = { x: 0, y: 0, w: W, h: H };
  if (def.text) {
    const [l, t, r, b] = def.text.map((a) => val(a) * E);
    if (r - l > 0 && b - t > 0) text = { x: l, y: t, w: r - l, h: b - t };
  }
  return { paths, text };
}
