// @vitest-environment jsdom
// (jsdom: happy-dom parses HTML differently from browsers, and DOMPurify relies on the browser behaviour)
// The rendered Markdown view: someone's upload shown as HTML, so nothing in it may run or reach other sites
import { describe, expect, test } from "vitest";
import { renderMarkdown } from "@/lib/markdown";
import { canBrowserThumbnail } from "@/components/FileIcon";
import type { Node } from "@/api";

const dom = (md: string) => {
  const div = document.createElement("div");
  div.innerHTML = renderMarkdown(md);
  return div;
};

describe("markdown", () => {
  test("renders headings, lists, tables and code", () => {
    const d = dom("# Title\n\n- one\n- two\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```\ncode\n```");
    expect(d.querySelector("h1")?.textContent).toBe("Title");
    expect(d.querySelectorAll("li")).toHaveLength(2);
    expect(d.querySelector("td")?.textContent).toBe("1");
    expect(d.querySelector("pre code")?.textContent).toBe("code\n");
  });

  test("raw HTML can't run scripts", () => {
    const d = dom(
      '<script>alert(1)</script>\n\n<img src=x onerror="alert(1)">\n\n<a href="javascript:alert(1)">x</a>\n\n<iframe src="/"></iframe><div style="position:fixed" onclick="alert(1)">y</div>',
    );
    const html = d.innerHTML;
    expect(html).not.toMatch(/<script|onerror|onclick|javascript:|<iframe|style=/i);
  });

  test("raw HTML can't take on the look of the app or lay links over a picture", () => {
    const d = dom(
      '<div class="fixed inset-0 z-50 bg-background" id="root" name="x">Your session ended</div>\n\n<img src="data:image/png;base64,iVBORw0KGgo=" usemap="#m"><map name="m"><area shape="rect" coords="0,0,9,9" href="https://elsewhere.example/"></map>',
    );
    expect(d.querySelector("[class], [id], [name]")).toBeNull();
    expect(d.querySelector("map, area")).toBeNull();
    expect(d.textContent).toContain("Your session ended");
  });

  test("web links open in a new tab without access to this page; other links keep only their text", () => {
    const d = dom("[web](https://example.com) [mail](mailto:a@b.c) [here](#part) [file](other.md) [bad](javascript:alert(1)) [data](data:text/html,x)");
    const links = [...d.querySelectorAll("a")];
    const web = links.find((a) => a.textContent === "web")!;
    expect(web.getAttribute("target")).toBe("_blank");
    expect(web.getAttribute("rel")).toContain("noopener");
    expect(links.find((a) => a.textContent === "here")!.getAttribute("href")).toBe("#part");
    for (const text of ["file", "bad", "data"]) expect(links.find((a) => a.textContent === text)?.hasAttribute("href") ?? false).toBe(false);
  });

  test("only pictures embedded in the file are loaded", () => {
    const d = dom("![remote](https://tracker.example/pixel.png) ![inline](data:image/png;base64,iVBORw0KGgo=)");
    const [remote, inline] = [...d.querySelectorAll("img")];
    expect(remote.hasAttribute("src")).toBe(false);
    expect(remote.getAttribute("alt")).toBe("remote");
    expect(inline.getAttribute("src")).toMatch(/^data:image\/png;/);
  });

  test("task lists become read-only checkboxes", () => {
    const d = dom("- [x] done\n- [ ] todo");
    const boxes = [...d.querySelectorAll("input")];
    expect(boxes).toHaveLength(2);
    expect(boxes.every((b) => b.type === "checkbox" && b.disabled)).toBe(true);
    expect(boxes[0].checked).toBe(true);
  });
});

describe("thumbnails made in the browser", () => {
  const node = (name: string, mime: string, size = 1000) => ({ id: "x", kind: "file", name, mime, size }) as Node;
  test("PDFs and videos the browser can play, as the server takes them", () => {
    expect(canBrowserThumbnail(node("a.pdf", "application/pdf"))).toBe(true);
    expect(canBrowserThumbnail(node("a.mp4", "video/mp4"))).toBe(true);
    expect(canBrowserThumbnail(node("a.webm", "video/webm"))).toBe(true);
    // The browser can't play these, and the server only takes the PDF and video types
    expect(canBrowserThumbnail(node("a.mkv", "video/x-matroska"))).toBe(false);
    expect(canBrowserThumbnail(node("a.pdf", "application/octet-stream"))).toBe(false);
    expect(canBrowserThumbnail(node("a.pdf", "application/pdf", 0))).toBe(false);
    expect(canBrowserThumbnail({ ...node("a", "application/pdf"), kind: "folder" })).toBe(false);
  });
});
