/**
 * VML (w:pict, w:object): legacy-document images (v:imagedata), text boxes, horizontal rules, watermarks (v:textpath), simple shapes and groups.
 */

import { attr, css, h, kid, kids, s } from "@/lib/office/ooxml";
import { fillBlocks } from "@/lib/office/docx/blocks";
import { childFlow, type Flow } from "@/lib/office/docx/context";
import { loadImage, type ObjOut } from "@/lib/office/docx/drawing";
import { cleanFace } from "@/lib/office/docx/fonts";
import { contentHeight } from "@/lib/office/docx/section";
import { flatKids } from "@/lib/office/docx/xml";

const r2 = (n: number) => Math.round(n * 100) / 100;

/** style="position:absolute;margin-left:10pt;width:100pt" → object */
function parseStyle(v: string | null): Record<string, string> {
  const out: Record<string, string> = {};
  for (const part of (v ?? "").split(";")) {
    const i = part.indexOf(":");
    if (i > 0) out[part.slice(0, i).trim().toLowerCase()] = part.slice(i + 1).trim();
  }
  return out;
}

/** VML length → px (unitless values are treated as px) */
function len(v: string | undefined, def = 0): number {
  if (!v) return def;
  const m = /^(-?[\d.]+)\s*(pt|px|in|cm|mm|pc|em)?$/i.exec(v.trim());
  if (!m) return def;
  const n = Number(m[1]);
  switch ((m[2] ?? "px").toLowerCase()) {
    case "pt":
      return (n * 96) / 72;
    case "in":
      return n * 96;
    case "cm":
      return (n * 96) / 2.54;
    case "mm":
      return (n * 96) / 25.4;
    case "pc":
      return n * 16;
    case "em":
      return n * 16;
    default:
      return n;
  }
}

const NAMED: Record<string, string> = {
  black: "#000000", white: "#ffffff", red: "#ff0000", green: "#008000", blue: "#0000ff", yellow: "#ffff00", silver: "#c0c0c0",
  gray: "#808080", grey: "#808080", navy: "#000080", maroon: "#800000", purple: "#800080", teal: "#008080", olive: "#808000",
  lime: "#00ff00", aqua: "#00ffff", fuchsia: "#ff00ff", windowText: "#000000", window: "#ffffff",
};

/** VML color: "#ff0000", "red", "black [3213]" */
function vmlColor(v: string | null | undefined): string | null {
  if (!v) return null;
  const t = v.trim().split(/\s+/)[0];
  if (/^#[0-9a-f]{6}$/i.test(t)) return t;
  if (/^#[0-9a-f]{3}$/i.test(t)) return `#${t[1]}${t[1]}${t[2]}${t[2]}${t[3]}${t[3]}`;
  return NAMED[t] ?? NAMED[t.toLowerCase()] ?? null;
}

const off = (v: string | null) => v !== null && /^(f|false|0)$/i.test(v);

function shapeBox(el: Element, st: Record<string, string>, f: Flow): HTMLElement | null {
  const w = len(st.width);
  const hgt = len(st.height);
  const imagedata = kid(el, "imagedata");
  const textbox = kid(el, "textbox");
  const textpath = kid(el, "textpath");
  const filled = !off(attr(el, "filled"));
  const fillEl = kid(el, "fill");
  const fill = filled ? vmlColor(attr(fillEl, "color")) ?? vmlColor(attr(el, "fillcolor")) ?? (el.localName === "shape" && !attr(el, "fillcolor") ? null : "#ffffff") : null;
  const opacity = Number((attr(fillEl, "opacity") ?? "1").replace(/f$/, "")) || 1;
  const stroked = !off(attr(el, "stroked"));
  const stroke = stroked ? vmlColor(attr(kid(el, "stroke"), "color")) ?? vmlColor(attr(el, "strokecolor")) ?? "#000000" : null;
  const sw = len(attr(el, "strokeweight") ?? undefined, 1);

  if (textpath) {
    // Watermark text: font size is derived from the bounding box
    const text = attr(textpath, "string") ?? "";
    const tst = parseStyle(attr(textpath, "style"));
    const units = Array.from(text).reduce((a, c) => a + (/[\u2e80-\uffff]/.test(c) ? 1 : 0.6), 0) || 1;
    const size = Math.max(8, Math.min(hgt * 0.9, (w / units) * 0.95));
    const face = cleanFace(tst["font-family"]?.replace(/&quot;|"/g, ""));
    return h(
      "span",
      {
        class: "tf-docx-wm",
        style: css({ width: `${r2(w)}px`, height: `${r2(hgt)}px`, color: fill ?? "#c0c0c0", opacity: opacity < 1 ? String(r2(opacity)) : undefined, "font-size": `${r2(size)}px`, "font-family": face ? `"${face}",sans-serif` : undefined }),
      },
      text,
    );
  }

  const box = h("span", { class: "tf-docx-shape", style: css({ width: `${r2(w)}px`, height: `${r2(hgt)}px` }) });
  if (imagedata) {
    const img = h("img", { alt: attr(imagedata, "title") ?? "", style: "width:100%;height:100%" });
    box.append(img);
    loadImage(img, attr(imagedata, "id") ?? attr(imagedata, "relid"), f);
    return box;
  }
  if ((fill || stroke) && w > 0 && hgt > 0) {
    const svg = s("svg", { class: "tf-docx-svg", width: r2(w), height: r2(hgt), viewBox: `0 0 ${r2(w)} ${r2(hgt)}` });
    const common = { fill: fill ?? "none", "fill-opacity": opacity < 1 ? r2(opacity) : undefined, stroke: stroke ?? "none", "stroke-width": r2(sw) };
    if (el.localName === "oval") svg.append(s("ellipse", { cx: r2(w / 2), cy: r2(hgt / 2), rx: r2(w / 2), ry: r2(hgt / 2), ...common }));
    else if (el.localName === "roundrect") {
      const arc = Number((attr(el, "arcsize") ?? "0.2").replace(/f$/, "")) || 0.2;
      const rr = Math.min(w, hgt) * (arc > 1 ? arc / 65536 : arc);
      svg.append(s("rect", { x: 0, y: 0, width: r2(w), height: r2(hgt), rx: r2(rr), ...common }));
    } else svg.append(s("rect", { x: 0, y: 0, width: r2(w), height: r2(hgt), ...common }));
    box.append(svg);
  }
  const content = kid(textbox, "txbxContent");
  if (content && f.depth < 6) {
    const inset = (attr(textbox, "inset") ?? "").split(",").map((v) => len(v.trim() || undefined, NaN));
    const pad = [inset[1], inset[2], inset[3], inset[0]].map((v, i) => (Number.isFinite(v) ? v : i % 2 ? 9.6 : 4.8));
    const flow = childFlow(f, { story: "textbox", width: Math.max(10, w - pad[1] - pad[3]), depth: f.depth + 1, floats: [] });
    const tb = h("div", { class: "tf-docx-txbx", style: css({ padding: pad.map((v) => `${r2(v)}px`).join(" ") }) });
    const inner = h("div", { class: "tf-docx-txbx-in" });
    fillBlocks(inner, flatKids(content), flow);
    tb.append(inner);
    box.append(tb);
    if (/mso-fit-shape-to-text:\s*t/i.test(attr(textbox, "style") ?? "")) {
      box.style.height = "auto";
      box.style.minHeight = `${r2(hgt)}px`;
      tb.style.position = "relative";
    }
  }
  return box;
}

export function renderVml(pict: Element, f: Flow, _p: HTMLElement): ObjOut | null {
  const el = flatKids(pict).find((c) => ["shape", "rect", "roundrect", "oval", "line", "group", "image", "polyline"].includes(c.localName));
  if (!el) return null;
  const st = parseStyle(attr(el, "style"));
  if (st.visibility === "hidden" || st.display === "none") return null;

  // Horizontal rule
  if (/^(t|true)$/i.test(attr(el, "hr") ?? "") || /^(t|true)$/i.test(attr(el, "hrstd") ?? "")) {
    const pct = Number(attr(el, "hrpct") ?? "1000") / 10 || 100;
    const align = attr(el, "hralign") ?? "left";
    const color = vmlColor(attr(el, "fillcolor")) ?? "#a0a0a0";
    const hr = h("span", {
      class: "tf-docx-hr",
      style: css({ height: `${r2(Math.max(1, len(st.height, 2)))}px`, width: `${r2(pct)}%`, background: color, "margin-left": align === "center" ? "auto" : align === "right" ? "auto" : undefined, "margin-right": align === "center" ? "auto" : undefined }),
    });
    return { el: hr, mode: "block" };
  }

  let box: HTMLElement | null;
  if (el.localName === "group") {
    box = h("span", { class: "tf-docx-group", style: css({ width: `${r2(len(st.width))}px`, height: `${r2(len(st.height))}px` }) });
    const [cw, ch] = (attr(el, "coordsize") ?? "1000,1000").split(",").map(Number);
    const [ox, oy] = (attr(el, "coordorigin") ?? "0,0").split(",").map(Number);
    const sx = len(st.width) / (cw || 1000);
    const sy = len(st.height) / (ch || 1000);
    for (const c of kids(el)) {
      const cst = parseStyle(attr(c, "style"));
      const cw2 = (Number(cst.width) || 0) * sx;
      const chh = (Number(cst.height) || 0) * sy;
      const child = shapeBox(c, { ...cst, width: `${cw2}px`, height: `${chh}px` }, f);
      if (!child) continue;
      child.classList.add("tf-docx-gchild");
      child.style.left = `${r2(((Number(cst.left) || 0) - (ox || 0)) * sx)}px`;
      child.style.top = `${r2(((Number(cst.top) || 0) - (oy || 0)) * sy)}px`;
      box.append(child);
    }
  } else box = shapeBox(el, st, f);
  if (!box) return null;

  const rot = Number(st.rotation ?? 0);
  if (rot) box.style.transform = `rotate(${r2(rot)}deg)`;
  if (st.position !== "absolute") {
    const wrap = h("span", { class: "tf-docx-inl" }, box);
    return { el: wrap, mode: "inline" };
  }

  // Floating: convert the relative position into page coordinates
  const sec = f.section;
  const w = len(st.width);
  const hgt = len(st.height);
  const relH = st["mso-position-horizontal-relative"] ?? "text";
  const relV = st["mso-position-vertical-relative"] ?? "text";
  const alignH = st["mso-position-horizontal"];
  const alignV = st["mso-position-vertical"];
  const hBase = relH === "page" ? 0 : relH === "left-margin-area" ? 0 : relH === "right-margin-area" ? sec.width - sec.right : sec.left;
  const hLen = relH === "page" ? sec.width : relH === "left-margin-area" ? sec.left : relH === "right-margin-area" ? sec.right : f.width;
  let x = hBase + len(st["margin-left"] ?? st.left);
  if (alignH === "center") x = hBase + (hLen - w) / 2;
  else if (alignH === "right") x = hBase + hLen - w;
  else if (alignH === "left") x = hBase;
  const pageV = relV === "page" || relV === "margin" || relV === "top-margin-area" || relV === "bottom-margin-area";
  const vBase = relV === "page" || relV === "top-margin-area" ? 0 : relV === "margin" ? sec.top : relV === "bottom-margin-area" ? sec.height - sec.bottom : 0;
  const vLen = relV === "page" ? sec.height : relV === "margin" ? contentHeight(sec) : relV === "top-margin-area" ? sec.top : sec.bottom;
  let y = vBase + len(st["margin-top"] ?? st.top);
  if (pageV && alignV === "center") y = vBase + (vLen - hgt) / 2;
  else if (pageV && alignV === "bottom") y = vBase + vLen - hgt;
  else if (pageV && alignV === "top") y = vBase;
  const z = Number(st["z-index"] ?? 0);
  const outer = h("span", { class: "tf-docx-obj tf-docx-abs" }, box);
  if (z < 0 || box.classList.contains("tf-docx-wm")) outer.classList.add("tf-docx-behind");
  else outer.style.zIndex = String(Math.min(50, 3 + Math.max(0, z)));
  const wrapType = attr(kid(pict, "wrap") ?? kids(el).find((c) => c.localName === "wrap") ?? null, "type");
  if (wrapType === "square" || wrapType === "tight" || wrapType === "through") {
    outer.className = "tf-docx-obj tf-docx-float";
    const right = x + w / 2 > sec.left + f.width / 2;
    outer.style.float = right ? "right" : "left";
    if (!right) outer.style.marginLeft = `${r2(Math.max(0, x - sec.left))}px`;
    return { el: outer, mode: "float" };
  }
  if (pageV || f.story === "header" || f.story === "footer") {
    if (!pageV) y = (f.story === "footer" ? sec.height - sec.footer - hgt : sec.header) + y;
    outer.style.left = `${r2(x)}px`;
    outer.style.top = `${r2(y)}px`;
    return { el: outer, mode: "page" };
  }
  outer.style.left = `${r2(x - sec.left)}px`;
  outer.style.top = `${r2(y)}px`;
  return { el: outer, mode: "para" };
}
