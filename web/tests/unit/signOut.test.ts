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
    // Amy (user 1) used this browser: her tabs (file and folder names), upload resume records and expanded folders
    localStorage.setItem("tf-signed-in-user", "1");
    localStorage.setItem("tf-tabs-1", '{"tabs":[{"title":"Salaries.xlsx"}]}');
    sessionStorage.setItem("tf-tabs-1", '{"tabs":[{"title":"Salaries.xlsx"}]}');
    localStorage.setItem("tf-tree-expanded-1", '["a"]');
    localStorage.setItem("tus::tus-br::Salaries.xlsx::1", '{"uploadUrl":"/api/upload/x"}');
    localStorage.setItem("tf-theme", "dark");
  });

  test("someone else signing in finds nothing of the previous person", async () => {
    const { noteSignedIn } = await freshPage();
    noteSignedIn(2);
    expect(localStorage.getItem("tf-tabs-1")).toBeNull();
    expect(sessionStorage.getItem("tf-tabs-1")).toBeNull();
    expect(localStorage.getItem("tf-tree-expanded-1")).toBeNull();
    expect(Object.keys(localStorage).some((k) => k.startsWith("tus::"))).toBe(false);
    // Settings of the browser itself stay
    expect(localStorage.getItem("tf-theme")).toBe("dark");
    expect(localStorage.getItem("tf-signed-in-user")).toBe("2");
  });

  test("the same person signing in again keeps their tabs and unfinished uploads", async () => {
    const { noteSignedIn } = await freshPage();
    noteSignedIn(1);
    expect(localStorage.getItem("tf-tabs-1")).not.toBeNull();
    expect(localStorage.getItem("tf-tree-expanded-1")).not.toBeNull();
    expect(Object.keys(localStorage).some((k) => k.startsWith("tus::"))).toBe(true);
  });
});
