/**
 * Math (OMML, m:oMath): lays out common structures in HTML (fractions, sub/superscripts, radicals, delimiters, n-ary operators, matrices).
 */

import { attr, css, h, kid, kids } from "@/lib/office/ooxml";

type Out = HTMLElement;

const span = (cls: string, ...c: (Node | string)[]) => h("span", { class: cls }, ...c);

function chr(el: Element | null, name: string, def: string) {
  const v = attr(kid(el, name), "val");
  return v === null ? def : v;
}

function seq(el: Element | null): Out {
  const out = span("tf-docx-m-row");
  for (const c of kids(el)) {
    const n = node(c);
    if (n) out.append(n);
  }
  return out;
}

function node(el: Element): Out | null {
  switch (el.localName) {
    case "r": {
      const text = kids(el, "t")
        .map((t) => t.textContent ?? "")
        .join("");
      const sty = attr(kid(kid(el, "rPr"), "sty"), "val");
      const plain = kid(kid(el, "rPr"), "nor") || sty === "p" || !/^[a-zA-Z]+$/.test(text);
      return h("span", { class: plain ? "tf-docx-m-t" : "tf-docx-m-t tf-docx-m-i" }, text);
    }
    case "f": {
      const pr = kid(el, "fPr");
      const type = attr(kid(pr, "type"), "val");
      if (type === "lin") return span("tf-docx-m-row", seq(kid(el, "num")), "/", seq(kid(el, "den")));
      return span("tf-docx-m-frac", span("tf-docx-m-num", seq(kid(el, "num"))), span("tf-docx-m-den", seq(kid(el, "den"))));
    }
    case "sSup":
      return span("tf-docx-m-row", seq(kid(el, "e")), h("sup", null, seq(kid(el, "sup"))));
    case "sSub":
      return span("tf-docx-m-row", seq(kid(el, "e")), h("sub", null, seq(kid(el, "sub"))));
    case "sSubSup":
      return span("tf-docx-m-row", seq(kid(el, "e")), span("tf-docx-m-scripts", h("sup", null, seq(kid(el, "sup"))), h("sub", null, seq(kid(el, "sub")))));
    case "sPre":
      return span("tf-docx-m-row", span("tf-docx-m-scripts", h("sup", null, seq(kid(el, "sup"))), h("sub", null, seq(kid(el, "sub")))), seq(kid(el, "e")));
    case "rad": {
      const deg = kid(el, "deg");
      const hide = attr(kid(kid(el, "radPr"), "degHide"), "val");
      const d = deg && kids(deg).length && hide !== "1" && hide !== "on" ? h("sup", { class: "tf-docx-m-deg" }, seq(deg)) : null;
      return span("tf-docx-m-row", ...(d ? [d] : []), "√", span("tf-docx-m-rad", seq(kid(el, "e"))));
    }
    case "d": {
      const pr = kid(el, "dPr");
      const beg = chr(pr, "begChr", "(");
      const end = chr(pr, "endChr", ")");
      const sep = chr(pr, "sepChr", "|");
      const out = span("tf-docx-m-row", span("tf-docx-m-fence", beg));
      kids(el, "e").forEach((e, i) => {
        if (i) out.append(span("tf-docx-m-t", sep));
        out.append(seq(e));
      });
      out.append(span("tf-docx-m-fence", end));
      return out;
    }
    case "nary": {
      const pr = kid(el, "naryPr");
      const op = chr(pr, "chr", "∫");
      const sub = kid(el, "sub");
      const sup = kid(el, "sup");
      const limLoc = attr(kid(pr, "limLoc"), "val");
      const opEl = span("tf-docx-m-op", op);
      const lims = (sub && kids(sub).length) || (sup && kids(sup).length);
      if (!lims) return span("tf-docx-m-row", opEl, seq(kid(el, "e")));
      if (limLoc === "undOvr" || (limLoc === null && op !== "∫")) {
        return span("tf-docx-m-row", span("tf-docx-m-under", span("tf-docx-m-small", seq(sup)), opEl, span("tf-docx-m-small", seq(sub))), seq(kid(el, "e")));
      }
      return span("tf-docx-m-row", opEl, span("tf-docx-m-scripts", h("sup", null, seq(sup)), h("sub", null, seq(sub))), seq(kid(el, "e")));
    }
    case "func":
      return span("tf-docx-m-row", seq(kid(el, "fName")), "\u2009", seq(kid(el, "e")));
    case "limLow":
      return span("tf-docx-m-under", seq(kid(el, "e")), span("tf-docx-m-small", seq(kid(el, "lim"))));
    case "limUpp":
      return span("tf-docx-m-under", span("tf-docx-m-small", seq(kid(el, "lim"))), seq(kid(el, "e")));
    case "acc": {
      const c = chr(kid(el, "accPr"), "chr", "\u0302");
      return span("tf-docx-m-under", span("tf-docx-m-small", c.trim() ? c : "^"), seq(kid(el, "e")));
    }
    case "bar":
      return h("span", { class: "tf-docx-m-row", style: css({ "text-decoration": attr(kid(kid(el, "barPr"), "pos"), "val") === "bot" ? "underline" : "overline" }) }, seq(kid(el, "e")));
    case "groupChr":
      return span("tf-docx-m-under", seq(kid(el, "e")), span("tf-docx-m-small", chr(kid(el, "groupChrPr"), "chr", "⏟")));
    case "box":
    case "borderBox":
      return h("span", { class: "tf-docx-m-row", style: el.localName === "borderBox" ? "border:1px solid currentColor;padding:0 2px" : undefined }, seq(kid(el, "e")));
    case "m": {
      const table = h("span", { class: "tf-docx-m-mat" });
      for (const mr of kids(el, "mr")) {
        const row = h("span", { class: "tf-docx-m-mr" });
        for (const e of kids(mr, "e")) row.append(h("span", { class: "tf-docx-m-mc" }, seq(e)));
        table.append(row);
      }
      return table;
    }
    case "eqArr": {
      const col = h("span", { class: "tf-docx-m-mat" });
      for (const e of kids(el, "e")) col.append(h("span", { class: "tf-docx-m-mr" }, h("span", { class: "tf-docx-m-mc" }, seq(e))));
      return col;
    }
    case "e":
    case "oMath":
      return seq(el);
    case "oMathPara":
      return span("tf-docx-m-para", ...kids(el, "oMath").map(seq));
    default:
      return null;
  }
}

export function renderMath(el: Element, sizePt: number): HTMLElement {
  const out = h("span", { class: el.localName === "oMathPara" ? "tf-docx-math tf-docx-math-para" : "tf-docx-math", style: `font-size:${Math.round(sizePt * 100) / 100}pt` });
  const n = node(el);
  if (n) out.append(n);
  return out;
}
