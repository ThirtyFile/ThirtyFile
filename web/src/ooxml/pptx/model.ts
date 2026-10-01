/**
 * Presentation structure: presentation.xml, slide → layout → master → theme,
 * color mapping (clrMap/clrMapOvr) and placeholder matching rules.
 */

import { attr, descendants, kid, kidPath, kids, numAttr, type OoxmlPackage } from "../core/package";
import { parseTheme, type ColorContext, type Theme } from "../core/theme";

export interface Ph {
  type: string;
  idx: string | null;
  el: Element;
}

export interface PartInfo {
  path: string;
  root: Element;
  /** p:cSld/p:spTree */
  tree: Element | null;
  phs: Ph[];
}

export interface MasterInfo extends PartInfo {
  theme: Theme | null;
  clrMap: Record<string, string>;
  titleStyle: Element | null;
  bodyStyle: Element | null;
  otherStyle: Element | null;
}

export interface LayoutInfo extends PartInfo {
  master: MasterInfo;
}

export interface SlideRef {
  path: string;
}

export interface Pres {
  pkg: OoxmlPackage;
  /** Slide size (EMU) */
  cx: number;
  cy: number;
  slides: SlideRef[];
  defaultText: Element | null;
  tableStyles: Map<string, Element>;
  layouts: Map<string, Promise<LayoutInfo | null>>;
  masters: Map<string, Promise<MasterInfo | null>>;
}

export interface SlideInfo extends PartInfo {
  pres: Pres;
  layout: LayoutInfo | null;
  master: MasterInfo | null;
  theme: Theme | null;
  cc: ColorContext;
  /** Slide number (1-based) */
  num: number;
}

// oxfmt-ignore
const DEFAULT_CLRMAP: Record<string, string> = {
  bg1: "lt1", tx1: "dk1", bg2: "lt2", tx2: "dk2",
  accent1: "accent1", accent2: "accent2", accent3: "accent3", accent4: "accent4", accent5: "accent5", accent6: "accent6",
  hlink: "hlink", folHlink: "folHlink",
};

function readClrMap(el: Element | null): Record<string, string> | null {
  if (!el) return null;
  const m: Record<string, string> = {};
  for (const a of Array.from(el.attributes)) if (!a.name.startsWith("xmlns")) m[a.localName] = a.value;
  return Object.keys(m).length ? m : null;
}

/** Placeholder settings of a shape (p:nvSpPr/p:nvPr/p:ph) */
export function phOf(el: Element): { type: string; idx: string | null } | null {
  const nv = kids(el).find((c) => c.localName.startsWith("nv") && c.localName.endsWith("Pr"));
  const ph = kidPath(nv, "nvPr", "ph");
  if (!ph) return null;
  return { type: attr(ph, "type") ?? "obj", idx: attr(ph, "idx") };
}

function collectPhs(tree: Element | null, out: Ph[] = [], depth = 0): Ph[] {
  if (depth > 20) return out;
  for (const c of kids(tree)) {
    if (c.localName === "grpSp") collectPhs(c, out, depth + 1);
    else {
      const ph = phOf(c);
      if (ph) out.push({ ...ph, el: c });
    }
  }
  return out;
}

function partInfo(path: string, doc: Document | null): PartInfo | null {
  const root = doc?.documentElement;
  if (!root) return null;
  const tree = kidPath(root, "cSld", "spTree");
  return { path, root, tree, phs: collectPhs(tree) };
}

const TITLE = new Set(["title", "ctrTitle"]);

/** Placeholder on a slide (or layout) → matching placeholder in the layout */
export function matchLayoutPh(list: Ph[], type: string, idx: string | null): Ph | undefined {
  if (idx !== null) {
    const m = list.find((p) => p.idx === idx && (p.type === type || !TITLE.has(p.type) || TITLE.has(type)));
    if (m) return m;
  }
  const same = list.find((p) => p.type === type);
  if (same) return same;
  if (TITLE.has(type)) return list.find((p) => TITLE.has(p.type));
  if (type === "obj" || type === "body") return list.find((p) => p.type === "body" || p.type === "obj");
  if (["pic", "chart", "tbl", "dgm", "media", "clipArt"].includes(type)) return list.find((p) => p.type === "obj" || p.type === "body");
  return undefined;
}

/** Masters only have types such as title/body/dt/ftr/sldNum */
export function matchMasterPh(list: Ph[], type: string): Ph | undefined {
  const t = TITLE.has(type) ? "title" : ["dt", "ftr", "sldNum", "hdr"].includes(type) ? type : "body";
  return list.find((p) => p.type === t) ?? (t === "title" ? undefined : list.find((p) => p.type === "body"));
}

/** Master text style used by a placeholder */
export function phStyleKind(type: string | null): "title" | "body" | "other" {
  if (type === null) return "other";
  if (TITLE.has(type)) return "title";
  if (["dt", "ftr", "sldNum", "hdr"].includes(type)) return "other";
  return "body";
}

// ───────────── Loading ─────────────

export async function loadPres(pkg: OoxmlPackage): Promise<Pres> {
  // The main document path is taken from _rels/.rels
  const rootRels = await pkg.rels("");
  const main = rootRels.find((r) => r.type.endsWith("/officeDocument"))?.target ?? "ppt/presentation.xml";
  const doc = await pkg.xml(main);
  const root = doc?.documentElement ?? null;
  const size = kid(root, "sldSz");
  const rels = await pkg.rels(main);
  const byId = new Map(rels.map((r) => [r.id, r]));
  const slides: SlideRef[] = [];
  for (const sid of kids(kid(root, "sldIdLst"), "sldId")) {
    const rel = byId.get(attr(sid, "id") ?? "");
    if (rel && !rel.external && pkg.has(rel.target)) slides.push({ path: rel.target });
  }
  // Without sldIdLst, fall back to file name order
  if (!slides.length) {
    const found = pkg.paths("ppt/slides/").filter((p) => /slide\d+\.xml$/.test(p));
    found.sort((a, b) => Number(/(\d+)\.xml$/.exec(a)![1]) - Number(/(\d+)\.xml$/.exec(b)![1]));
    for (const p of found) slides.push({ path: p });
  }
  const tableStyles = new Map<string, Element>();
  const tsRel = rels.find((r) => r.type.endsWith("/tableStyles"));
  const tsDoc = tsRel ? await pkg.xml(tsRel.target) : null;
  for (const st of descendants(tsDoc, "tblStyle")) {
    const id = attr(st, "styleId");
    if (id) tableStyles.set(id.toUpperCase(), st);
  }
  return {
    pkg,
    cx: numAttr(size, "cx") ?? 12192000,
    cy: numAttr(size, "cy") ?? 6858000,
    slides,
    defaultText: kid(root, "defaultTextStyle"),
    tableStyles,
    layouts: new Map(),
    masters: new Map(),
  };
}

function loadMaster(pres: Pres, path: string): Promise<MasterInfo | null> {
  let p = pres.masters.get(path);
  if (!p) {
    p = (async () => {
      const info = partInfo(path, await pres.pkg.xml(path));
      if (!info) return null;
      const themeRel = await pres.pkg.relOfType(path, "/theme");
      const theme = themeRel ? parseTheme(await pres.pkg.xml(themeRel.target)) : null;
      const tx = kid(info.root, "txStyles");
      return {
        ...info,
        theme,
        clrMap: { ...DEFAULT_CLRMAP, ...readClrMap(kid(info.root, "clrMap")) },
        titleStyle: kid(tx, "titleStyle"),
        bodyStyle: kid(tx, "bodyStyle"),
        otherStyle: kid(tx, "otherStyle"),
      };
    })();
    pres.masters.set(path, p);
  }
  return p;
}

function loadLayout(pres: Pres, path: string): Promise<LayoutInfo | null> {
  let p = pres.layouts.get(path);
  if (!p) {
    p = (async () => {
      const info = partInfo(path, await pres.pkg.xml(path));
      if (!info) return null;
      const mRel = await pres.pkg.relOfType(path, "/slideMaster");
      const master = mRel ? await loadMaster(pres, mRel.target) : null;
      if (!master) return null;
      return { ...info, master };
    })();
    pres.layouts.set(path, p);
  }
  return p;
}

const overrideMap = (root: Element | null) => readClrMap(kidPath(root, "clrMapOvr", "overrideClrMapping"));

export async function loadSlide(pres: Pres, index: number): Promise<SlideInfo | null> {
  const ref = pres.slides[index];
  const info = partInfo(ref.path, await pres.pkg.xml(ref.path));
  if (!info) return null;
  const lRel = await pres.pkg.relOfType(ref.path, "/slideLayout");
  const layout = lRel ? await loadLayout(pres, lRel.target) : null;
  const master = layout?.master ?? null;
  const clrMap = { ...(master?.clrMap ?? DEFAULT_CLRMAP), ...overrideMap(layout?.root ?? null), ...overrideMap(info.root) };
  const theme = master?.theme ?? null;
  return { ...info, pres, layout, master, theme, cc: { theme, clrMap }, num: index + 1 };
}
