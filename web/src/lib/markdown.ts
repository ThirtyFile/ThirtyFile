/**
 * Markdown to safe HTML for the rendered view (loaded only when a Markdown file is shown). The file is someone's
 * upload, so nothing in it may run on this site: the HTML marked produces is sanitised with DOMPurify (no scripts,
 * event handlers, frames, forms, styles or javascript: links), links open in a new tab without access to this page,
 * and pictures are only shown when they are embedded in the file (data:), since other addresses can't be loaded here
 * and would tell another site that the file was opened. Headings get ids as on GitHub, with a prefix of their own, so
 * a table of contents ("[Install](#install)") goes to them.
 */
import { Marked } from "marked";
import DOMPurify from "dompurify";

const marked = new Marked({ gfm: true, breaks: false, async: false });

/** Link addresses that may stay: web, mail and in-page links */
const SAFE_LINK = /^(https?:|mailto:|#)/i;

let purify: ReturnType<typeof DOMPurify> | null = null;

/** Ids of the headings of the file being rendered: a name seen again gets a number, as on GitHub */
const slugs = new Map<string, number>();

/** A heading's id as GitHub makes it ("Getting started!" → "getting-started"), behind a prefix that no id of the app has */
function headingId(text: string): string {
  const base = text
    .trim()
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\s_-]/gu, "")
    .replace(/\s/g, "-");
  const seen = slugs.get(base) ?? 0;
  slugs.set(base, seen + 1);
  return `md-${seen ? `${base}-${seen}` : base}`;
}

function sanitiser() {
  if (purify) return purify;
  const p = DOMPurify(window);
  p.addHook("afterSanitizeAttributes", (el) => {
    if (el.tagName === "A") {
      const href = el.getAttribute("href") ?? "";
      if (!SAFE_LINK.test(href.trim())) {
        // A relative link points into the file's own folder structure, which isn't a page here: keep the text only
        el.removeAttribute("href");
      } else if (href.startsWith("#")) {
        // To a heading of the file (headings carry the prefix)
        let target = href.slice(1);
        try {
          target = decodeURIComponent(target);
        } catch {
          // Kept as written
        }
        el.setAttribute("href", `#md-${target.toLowerCase()}`);
      } else {
        el.setAttribute("target", "_blank");
        el.setAttribute("rel", "noopener noreferrer nofollow");
      }
    }
    if (/^H[1-6]$/.test(el.tagName)) el.setAttribute("id", headingId(el.textContent ?? ""));
    if (el.tagName === "IMG") {
      const src = el.getAttribute("src") ?? "";
      if (!/^data:image\/(png|jpe?g|gif|webp);/i.test(src)) {
        el.removeAttribute("src");
        el.removeAttribute("srcset");
      }
      el.setAttribute("loading", "lazy");
    }
    // Inputs only come from task lists ("- [x] done"): read-only checkboxes
    if (el.tagName === "INPUT") {
      for (const a of [...el.attributes]) if (a.name !== "checked") el.removeAttribute(a.name);
      el.setAttribute("type", "checkbox");
      el.setAttribute("disabled", "");
    }
  });
  purify = p;
  return p;
}

export function renderMarkdown(text: string): string {
  const html = marked.parse(text) as string;
  slugs.clear();
  return sanitiser().sanitize(html, {
    USE_PROFILES: { html: true },
    // Image maps would lay links over an embedded picture; classes and ids would give the file the app's own styles
    // (a box that looks like the app's dialogs, over the whole page). Headings get ids of their own (`headingId`).
    FORBID_TAGS: ["style", "form", "button", "textarea", "select", "iframe", "object", "embed", "video", "audio", "source", "base", "meta", "link", "map", "area"],
    FORBID_ATTR: ["style", "srcset", "class", "id", "name", "usemap"],
  }) as string;
}
