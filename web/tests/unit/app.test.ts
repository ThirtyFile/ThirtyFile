// Small pieces of the app that decide what is sent to the server or saved in the browser
import { describe, expect, test } from "vitest";
import { enc, privateSource, shareSource, type Node } from "@/api";
import { frameDocument } from "@/components/officeFrame";
import { filenameFrom } from "@/downloads";
import { keepOnly, validTabs } from "@/tabs";

const response = (disposition?: string) => new Response(null, { headers: disposition ? { "Content-Disposition": disposition } : {} });

describe("download file names", () => {
  test("filename* wins and is decoded", () => {
    expect(filenameFrom(response("attachment; filename=\"a.txt\"; filename*=UTF-8''%E5%A0%B1%E5%91%8A%20v2.pdf"), "x")).toBe("\u5831\u544a v2.pdf");
  });
  test("plain filename, with or without quotes", () => {
    expect(filenameFrom(response('attachment; filename="report.pdf"'), "x")).toBe("report.pdf");
    expect(filenameFrom(response("attachment; filename=report.pdf"), "x")).toBe("report.pdf");
  });
  test("malformed filename* falls back to filename, missing header to the fallback", () => {
    expect(filenameFrom(response("attachment; filename*=UTF-8''%E5%ZZ; filename=\"b.txt\""), "x")).toBe("b.txt");
    expect(filenameFrom(response(), "download")).toBe("download");
  });
});

describe("saved tabs", () => {
  const tab = (id: string, entries = ["/files"], index = 0) => ({ id, entries, index, title: "" });
  test("a valid state is kept", () => {
    const state = { tabs: [tab("a"), tab("b", ["/files", "/view/x"], 1)], active: "b" };
    expect(validTabs(state)).toEqual(state);
  });
  test.each([
    ["not an object", "tabs"],
    ["no tabs", { tabs: [], active: "a" }],
    ["unknown active tab", { tabs: [tab("a")], active: "b" }],
    ["duplicate ids", { tabs: [tab("a"), tab("a")], active: "a" }],
    ["index out of range", { tabs: [tab("a", ["/files"], 1)], active: "a" }],
    ["entry that isn't a path", { tabs: [tab("a", ["https://example.com"])], active: "a" }],
    ["missing title", { tabs: [{ id: "a", entries: ["/files"], index: 0 }], active: "a" }],
    ["too many tabs", { tabs: Array.from({ length: 21 }, (_, i) => tab(`t${i}`)), active: "t0" }],
  ])("%s is rejected", (_, raw) => expect(validTabs(raw)).toBeNull());

  describe("closing other tabs", () => {
    const files = tab("files", ["/files", "/files/folder-1"], 1);
    const editor = tab("editor", ["/files", "/view/f1"], 1);
    const other = tab("other", ["/view/f2"]);

    test("from a tab that isn't shown, that tab's page is shown", () => {
      const plan = keepOnly({ tabs: [files, editor, other], active: "editor" }, "files", ["f1", "f2"]);
      expect(plan).toEqual({ next: { tabs: [files], active: "files" }, show: "/files/folder-1", discard: ["f1", "f2"] });
    });

    test("from the tab shown, the page stays", () => {
      const plan = keepOnly({ tabs: [files, editor, other], active: "editor" }, "editor", ["f2"]);
      expect(plan).toEqual({ next: { tabs: [editor], active: "editor" }, show: null, discard: ["f2"] });
    });

    test("the kept tab's own file keeps its draft", () => {
      const again = tab("again", ["/view/f1"]);
      expect(keepOnly({ tabs: [editor, again], active: "again" }, "editor", ["f1"])?.discard).toEqual([]);
    });

    test("a tab closed meanwhile closes nothing", () => {
      expect(keepOnly({ tabs: [files], active: "files" }, "editor", ["f1"])).toBeNull();
    });
  });
});

describe("API paths", () => {
  test("interpolated parts are encoded", () => {
    expect(enc`/public/shares/${"abc?x=1"}/unlock`).toBe("/public/shares/abc%3Fx%3D1/unlock");
    expect(enc`/nodes/${"../admin"}/children`).toBe("/nodes/..%2Fadmin/children");
    expect(enc`/admin/users/${42}`).toBe("/admin/users/42");
  });
  test("file sources", async () => {
    const node = { id: "n/1", updated_at: 5 } as Node;
    expect(privateSource.contentUrl(node, true)).toBe("/api/files/n%2F1/content?download=1");
    // A single item downloads from a plain URL (several go through a short-lived link made with a POST)
    await expect(privateSource.downloadLink(["a/b"])).resolves.toMatch(/^\/api\/download\?ids=a%2Fb&tz=-?\d+$/);
    const share = shareSource("t#k");
    expect(share.thumbUrl(node)).toBe("/api/public/shares/t%23k/nodes/n%2F1/thumbnail?v=5");
  });
});

describe("preview frame", () => {
  // Must give the same text as the server's escape_script_end (server/src/web.rs), which hashes it for the page's CSP
  test("</script is escaped like the server does", () => {
    const doc = frameDocument("a</script>b</SCRIPT>c");
    expect(doc).toContain("<script>a<\\/script>b<\\/SCRIPT>c</script>");
    expect(frameDocument("no tags")).toContain("<script>no tags</script>");
  });
  test("the frame can't connect anywhere", () => {
    expect(frameDocument("")).toMatch(/Content-Security-Policy" content="default-src 'none';/);
  });
});
