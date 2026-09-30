/**
 * Numbering (numbering.xml): num → lvlOverride → abstractNum (numStyleLink) → lvl,
 * plus counters (lists sharing an abstractNum continue numbering, startOverride, lvlRestart).
 */

import { attr, kid, kids, numAttr, toggle } from "../core/package";
import { formatNumber } from "./numfmt";
import type { Styles } from "./styles";
import { flatKid } from "./xml";

export interface LevelDef {
  ilvl: number;
  start: number;
  fmt: string;
  /** w:format for numFmt="custom" */
  custom: string | null;
  text: string | null;
  jc: string;
  suff: string;
  pPr: Element | null;
  rPr: Element | null;
  /** lvlRestart: undefined means the default (restart when the previous level appears), 0 means never restart */
  restart?: number;
  isLgl: boolean;
  picBullet: Element | null;
}

interface AbstractNum {
  id: string;
  levels: Map<number, Element>;
  numStyleLink: string | null;
}

interface NumDef {
  abstractId: string;
  overrides: Map<number, { start?: number; lvl?: Element }>;
}

interface Counter {
  counts: (number | undefined)[];
  applied: Set<string>;
}

export interface Marker {
  text: string;
  level: LevelDef;
}

export class Numbering {
  private abstracts = new Map<string, AbstractNum>();
  private nums = new Map<string, NumDef>();
  private picBullets = new Map<string, Element>();
  private counters = new Map<string, Counter>();
  private levelCache = new Map<string, LevelDef | null>();

  constructor(
    doc: Document | null,
    private styles: Styles,
  ) {
    const root = doc?.documentElement;
    for (const pb of kids(root, "numPicBullet")) {
      const id = attr(pb, "numPicBulletId");
      if (id !== null) this.picBullets.set(id, pb);
    }
    for (const a of kids(root, "abstractNum")) {
      const id = attr(a, "abstractNumId");
      if (id === null) continue;
      const levels = new Map<number, Element>();
      for (const l of kids(a, "lvl")) levels.set(numAttr(l, "ilvl") ?? 0, l);
      this.abstracts.set(id, { id, levels, numStyleLink: attr(kid(a, "numStyleLink"), "val") });
    }
    for (const n of kids(root, "num")) {
      const id = attr(n, "numId");
      const abs = attr(kid(n, "abstractNumId"), "val");
      if (id === null || abs === null) continue;
      const overrides = new Map<number, { start?: number; lvl?: Element }>();
      for (const o of kids(n, "lvlOverride")) {
        const ilvl = numAttr(o, "ilvl") ?? 0;
        const start = numAttr(kid(o, "startOverride"), "val");
        overrides.set(ilvl, { start: start ?? undefined, lvl: kid(o, "lvl") ?? undefined });
      }
      this.nums.set(id, { abstractId: abs, overrides });
    }
  }

  /** numStyleLink: points to a numbering style whose numPr leads to the actual abstractNum */
  private abstractOf(numId: string, depth = 0): AbstractNum | null {
    const num = this.nums.get(numId);
    const abs = num ? this.abstracts.get(num.abstractId) : undefined;
    if (!abs) return null;
    if (abs.numStyleLink && depth < 4) {
      const st = this.styles.byId.get(abs.numStyleLink);
      const linked = attr(kid(kid(st?.pPr, "numPr"), "numId"), "val");
      if (linked && linked !== numId) return this.abstractOf(linked, depth + 1) ?? abs;
    }
    return abs;
  }

  level(numId: string, ilvl: number): LevelDef | null {
    const key = `${numId}:${ilvl}`;
    if (this.levelCache.has(key)) return this.levelCache.get(key)!;
    const num = this.nums.get(numId);
    const abs = this.abstractOf(numId);
    const el = num?.overrides.get(ilvl)?.lvl ?? abs?.levels.get(ilvl) ?? null;
    let def: LevelDef | null = null;
    if (el) {
      const fmtEl = flatKid(el, "numFmt");
      const lvlText = kid(el, "lvlText");
      const restart = numAttr(kid(el, "lvlRestart"), "val");
      const pic = attr(kid(el, "lvlPicBulletId"), "val");
      def = {
        ilvl,
        start: numAttr(kid(el, "start"), "val") ?? 0,
        fmt: attr(fmtEl, "val") ?? "decimal",
        custom: attr(fmtEl, "format"),
        text: lvlText ? attr(lvlText, "val") ?? "" : null,
        jc: attr(kid(el, "lvlJc"), "val") ?? "left",
        suff: attr(kid(el, "suff"), "val") ?? "tab",
        pPr: kid(el, "pPr"),
        rPr: kid(el, "rPr"),
        restart: restart ?? undefined,
        isLgl: toggle(kid(el, "isLgl")) ?? false,
        picBullet: pic !== null ? this.picBullets.get(pic) ?? null : null,
      };
      // Start from 1 when there is no start (Word's actual behavior; the spec's 0 is rare)
      if (!kid(el, "start")) def.start = 1;
    }
    this.levelCache.set(key, def);
    return def;
  }

  private startOf(numId: string, ilvl: number) {
    const o = this.nums.get(numId)?.overrides.get(ilvl);
    return o?.start ?? this.level(numId, ilvl)?.start ?? 1;
  }

  /** Advance the counter and produce the number text; returns null when numId is invalid or 0 */
  next(numId: string, ilvl: number): Marker | null {
    if (numId === "0" || !this.nums.has(numId)) return null;
    const lvl = Math.max(0, Math.min(8, ilvl));
    const level = this.level(numId, lvl);
    if (!level) return null;
    const abs = this.abstractOf(numId);
    const key = abs?.id ?? numId;
    let c = this.counters.get(key);
    if (!c) {
      c = { counts: [], applied: new Set() };
      this.counters.set(key, c);
    }
    // startOverride: restart the first time this num is used
    if (!c.applied.has(numId)) {
      c.applied.add(numId);
      for (const [k, o] of this.nums.get(numId)!.overrides) if (o.start !== undefined || o.lvl) c.counts[k] = undefined;
    }
    for (let k = 0; k < lvl; k++) if (c.counts[k] === undefined) c.counts[k] = this.startOf(numId, k);
    c.counts[lvl] = c.counts[lvl] === undefined ? this.startOf(numId, lvl) : c.counts[lvl]! + 1;
    for (let k = lvl + 1; k < 9; k++) {
      const r = this.level(numId, k)?.restart;
      // lvlRestart=0: never restart; lvlRestart=n: restart when level n or higher appears
      if (r === 0) continue;
      if (r === undefined || lvl < r) c.counts[k] = undefined;
    }
    const counts = c.counts;
    let text = level.text ?? "";
    if (level.fmt !== "bullet") {
      text = text.replace(/%([1-9])/g, (_, d: string) => {
        const k = Number(d) - 1;
        const def = this.level(numId, k);
        const v = counts[k] ?? this.startOf(numId, k);
        const fmt = level.isLgl && def?.fmt !== "decimalZero" ? "decimal" : def?.fmt ?? "decimal";
        return formatNumber(v, fmt, def?.custom);
      });
    }
    return { text, level };
  }
}
