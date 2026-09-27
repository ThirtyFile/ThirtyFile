/**
 * PowerPoint (.pptx) preview: slides laid out in order, scaled to the container width.
 *
 * Structure:
 *   model.ts     presentation/slides/layouts/masters, color mapping, placeholder mapping
 *   shapes.ts    shape tree (shapes, pictures, groups, connectors, tables, charts, SmartArt, OLE)
 *   geometry.ts  geometry formula evaluation; presets.ts preset shape definitions
 *   paint.ts     fills, lines, shadows
 *   text.ts      text and bullets; table.ts tables
 *
 * Slides are rendered only when near the viewport, so large presentations don't decompress all images at once.
 */

import { h, OoxmlPackage, css } from "@/lib/office/ooxml";
import { fontStack, rgbaCss, schemeColor } from "@/lib/office/theme";
import { loadPres, loadSlide, type Pres } from "./model";
import { applyCssFill } from "./paint";
import { relsMap, renderTree, ROOT_MAP, slideBackground, type Ctx, type SlideRender } from "./shapes";
import { C, CSS } from "./styles";

export interface RenderResult {
  /** Release image URLs and event listeners */
  dispose(): void;
}

/** Maximum zoom factor (relative to the design size) */
const MAX_SCALE = 1.5;
/** Number of slides rendered up front */
const EAGER = 3;

async function renderSlide(pres: Pres, index: number, slideEl: HTMLElement, alive: () => boolean) {
  const info = await loadSlide(pres, index);
  if (!info || !alive()) return;
  // Hidden slides (show="0") are shown faded
  if (info.root.getAttribute("show") === "0") slideEl.parentElement?.classList.add(C.hidden);
  const sr: SlideRender = { info, pkg: pres.pkg, after: [], rels: new Map(), alive };

  // Default text: the theme's body font, tx1
  const minor = info.theme?.minor;
  slideEl.style.fontFamily = fontStack(minor?.latin, minor?.ea || minor?.scripts.Hant);
  slideEl.style.color = rgbaCss(schemeColor("tx1", info.cc)) ?? "#000";

  const bg = slideBackground(info);
  const bgEl = h("div", { class: C.bg });
  if (bg.fill && bg.fill.t !== "none") applyCssFill(bgEl, bg.fill, pres.pkg);
  else bgEl.style.backgroundColor = "#fff";
  slideEl.append(bgEl);

  const showSlide = (el: Element | null) => (el?.getAttribute("showMasterSp") ?? "1") !== "0";
  const layers: { tree: Element | null; part: string; layer: Ctx["layer"] }[] = [];
  if (showSlide(info.root)) {
    if (info.master && showSlide(info.layout?.root ?? null)) layers.push({ tree: info.master.tree, part: info.master.path, layer: "master" });
    if (info.layout) layers.push({ tree: info.layout.tree, part: info.layout.path, layer: "layout" });
  }
  layers.push({ tree: info.tree, part: info.path, layer: "slide" });

  for (const l of layers) {
    if (!l.tree || !alive()) continue;
    const ctx: Ctx = { sr, part: l.part, rels: await relsMap(sr, l.part), layer: l.layer, groupFill: null, depth: 0 };
    await renderTree(l.tree, ctx, ROOT_MAP, slideEl);
  }
  // All content is already in the document, so measuring computes layout synchronously (text autofit);
  // don't wait for requestAnimationFrame, which may not fire in background tabs
  if (sr.after.length && alive()) {
    for (const f of sr.after) {
      try {
        f();
      } catch {
        // Leave as is when measuring fails
      }
    }
  }
}

export async function renderPptx(buf: ArrayBuffer, root: HTMLElement): Promise<RenderResult> {
  root.replaceChildren();
  const pkg = await OoxmlPackage.open(buf);
  const pres = await loadPres(pkg);
  let disposed = false;
  const alive = () => !disposed;

  const W = pres.cx / 9525;
  const H = pres.cy / 9525;
  const deck = h("div", { class: C.deck });
  const style = h("style", null, CSS);
  root.append(style, deck);

  const frames: HTMLElement[] = [];
  const slides: HTMLElement[] = [];
  const done = new Set<number>();
  pres.slides.forEach((_, i) => {
    const slide = h("div", { class: C.slide, style: css({ width: `${W}px`, height: `${H}px` }) });
    const frame = h("div", { class: C.frame, "data-slide": i + 1 }, slide);
    frames.push(frame);
    slides.push(slide);
    deck.append(frame);
  });

  // Scaling
  const layout = () => {
    const avail = Math.max(0, deck.clientWidth - 32);
    const scale = avail > 0 ? Math.min(avail / W, MAX_SCALE) : 1;
    for (let i = 0; i < frames.length; i++) {
      frames[i].style.width = `${Math.round(W * scale)}px`;
      frames[i].style.height = `${Math.round(H * scale)}px`;
      slides[i].style.transform = `scale(${scale})`;
    }
  };
  layout();
  const ro = new ResizeObserver(() => layout());
  ro.observe(root);

  const draw = (i: number) => {
    if (done.has(i) || disposed) return Promise.resolve();
    done.add(i);
    return renderSlide(pres, i, slides[i], alive).catch((e) => console.warn("pptx slide", i + 1, e));
  };

  // Render only when near the viewport
  const io = new IntersectionObserver(
    (entries) => {
      for (const e of entries) {
        if (!e.isIntersecting) continue;
        const i = Number((e.target as HTMLElement).dataset.slide) - 1;
        if (i >= 0 && i < frames.length) {
          io.unobserve(e.target);
          void draw(i);
        }
      }
    },
    { rootMargin: "150% 0px" },
  );
  frames.forEach((f, i) => i >= EAGER && io.observe(f));
  await Promise.all(frames.slice(0, EAGER).map((_, i) => draw(i)));

  return {
    dispose() {
      disposed = true;
      ro.disconnect();
      io.disconnect();
      pkg.dispose();
    },
  };
}
