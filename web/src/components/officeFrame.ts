/**
 * The sandboxed preview frame shared by Word / PowerPoint previews and the drawing layer of Excel previews.
 * The renderer (dist/office-frame.js) is downloaded once and embedded inline into every frame's srcdoc, so the frame needs no network access.
 */
import { htmlLang, t } from "@/lib/i18n";

let frameScript: Promise<string> | null = null;
export function loadFrameScript() {
  frameScript ??= fetch("/office-frame.js").then((r) => {
    if (!r.ok) {
      frameScript = null;
      throw new Error(t("Couldn't load the previewer"));
    }
    return r.text();
  });
  return frameScript;
}

/**
 * Contents of the sandbox iframe: the renderer is embedded inline, and CSP blocks all network connections.
 * Even if a script carried by the document runs, it can't connect out to send the content, let alone reach this site's sign-in state.
 * `overlay` frames (Excel drawings) are transparent and don't scroll themselves; the host moves the layer.
 */
export function frameDocument(script: string, overlay = false) {
  const csp = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src blob: data:; font-src blob: data:; media-src blob: data:";
  const rootCss = overlay ? "#root{position:relative;height:100%;overflow:hidden}" : "#root{height:100%;overflow:auto}";
  // Text without a font of its own: the page's fonts for its language (style.css, --font-cjk)
  const cjk = typeof document === "undefined" ? "" : getComputedStyle(document.documentElement).getPropertyValue("--font-cjk").trim();
  return `<!doctype html><html lang="${htmlLang}"><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${csp}">
<style>html,body{margin:0;height:100%;background:transparent;overflow:hidden;font-family:${cjk ? `${cjk},` : ""}sans-serif}
${rootCss}</style>
</head><body><div id="root"></div><script>${script.replace(/<\/(script)/gi, "<\\/$1")}</script></body></html>`;
}
