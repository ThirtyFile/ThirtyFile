// What a browser keeps of the previous person after a session ended without signing out (lib/signOut.ts)
import { beforeEach, describe, expect, test, vi } from "vitest";

vi.mock("@/uploads", () => ({ cancelAll: vi.fn<() => void>() }));
vi.mock("@/downloads", () => ({ clearDownloads: vi.fn<() => void>() }));

/** The module as a freshly loaded page has it */
async function freshPage() {
  vi.resetModules();
  return import("@/lib/signOut");
}

describe("after a session expired", () => {
  beforeEach(() => {
    localStorage.clear();
    sessionStorage.clear();
    // Amy (user 1) used this browser: her tabs (file and folder names), upload resume records, expanded folders and
    // the widths she gave the Columns view's columns (by folder)
    localStorage.setItem("tf-signed-in-user", "1");
    localStorage.setItem("tf-tabs-1", '{"tabs":[{"title":"Salaries.xlsx"}]}');
    sessionStorage.setItem("tf-tabs-1", '{"tabs":[{"title":"Salaries.xlsx"}]}');
    localStorage.setItem("tf-tree-expanded-1", '["a"]');
    localStorage.setItem("tf-column-widths-1", '{"a":300}');
    localStorage.setItem("tus::tf|0123456789abcdef|r1::1", JSON.stringify({ uploadUrl: "/api/upload/x", metadata: { filename: "Salaries.xlsx" }, creationTime: new Date().toString() }));
    // Interrupted uploads that can be continued after a reload: Amy's own, one through a share link, and Ben's
    localStorage.setItem("tf-upload-tasks-u1", '[{"name":"Salaries.xlsx"}]');
    localStorage.setItem("tf-upload-tasks-s0123456789abcdef", '[{"name":"Dropped.pdf"}]');
    localStorage.setItem("tf-upload-tasks-u3", '[{"name":"Other.docx"}]');
    localStorage.setItem("tf-theme", "dark");
  });

  test("someone else signing in finds nothing of the previous person", async () => {
    const { noteSignedIn } = await freshPage();
    noteSignedIn(2);
    expect(localStorage.getItem("tf-tabs-1")).toBeNull();
    expect(sessionStorage.getItem("tf-tabs-1")).toBeNull();
    expect(localStorage.getItem("tf-tree-expanded-1")).toBeNull();
    expect(localStorage.getItem("tf-column-widths-1")).toBeNull();
    expect(Object.keys(localStorage).some((k) => k.startsWith("tus::"))).toBe(false);
    expect(Object.keys(localStorage).some((k) => k.startsWith("tf-upload-tasks-"))).toBe(false);
    // Settings of the browser itself stay
    expect(localStorage.getItem("tf-theme")).toBe("dark");
    expect(localStorage.getItem("tf-signed-in-user")).toBe("2");
  });

  test("the same person signing in again keeps their tabs and unfinished uploads", async () => {
    const { noteSignedIn } = await freshPage();
    noteSignedIn(1);
    expect(localStorage.getItem("tf-tabs-1")).not.toBeNull();
    expect(localStorage.getItem("tf-tree-expanded-1")).not.toBeNull();
    expect(localStorage.getItem("tf-column-widths-1")).not.toBeNull();
    expect(Object.keys(localStorage).some((k) => k.startsWith("tus::"))).toBe(true);
    // Their own interrupted uploads and the share link's stay; nobody else's
    expect(localStorage.getItem("tf-upload-tasks-u1")).not.toBeNull();
    expect(localStorage.getItem("tf-upload-tasks-s0123456789abcdef")).not.toBeNull();
    expect(localStorage.getItem("tf-upload-tasks-u3")).toBeNull();
  });

  test("signing out leaves no upload records at all", async () => {
    const { leaveAfterSignOut } = await freshPage();
    const assign = vi.fn<(url: string) => void>();
    vi.stubGlobal("location", { ...window.location, assign });
    localStorage.setItem("tf-last-user", "amy");
    leaveAfterSignOut(1);
    expect(Object.keys(localStorage).filter((k) => k.startsWith("tf-upload-tasks-") || k.startsWith("tus::"))).toEqual([]);
    // Nor what she kept of the folders she visited
    expect(localStorage.getItem("tf-tree-expanded-1")).toBeNull();
    expect(localStorage.getItem("tf-column-widths-1")).toBeNull();
    // The sign-in screen asks for the account again
    expect(localStorage.getItem("tf-last-user")).toBeNull();
    expect(assign).toHaveBeenCalledWith("/login");
    vi.unstubAllGlobals();
  });
});
