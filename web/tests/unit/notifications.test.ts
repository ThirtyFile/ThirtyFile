// Notifications: what the bell says for each kind, where opening one goes, and the email server's port choice
import { describe, expect, test } from "vitest";
import type { AppNotification } from "@/api";
import { notificationLink, notificationText, portForSecurity, unreadBadge } from "@/lib/notifications";

const note = (n: Partial<AppNotification>): AppNotification => ({ id: 1, kind: "shared", data: {}, node_id: "n1", created_at: 0, read: false, ...n });

describe("notifications", () => {
  test("a share says who shared what, the role, and until when", () => {
    const shared = note({ data: { by: "Amy", name: "Plans", item: "folder", role: "editor" } });
    expect(notificationText(shared)).toEqual({ title: "Amy shared “Plans” with you", detail: "Role: Editor" });
    const space = note({ data: { by: "Amy", name: "Marketing", item: "space", drive_kind: "team", role: "viewer", expires_at: 1_800_000_000 } });
    const text = notificationText(space);
    expect(text.title).toBe("Amy added you to the space “Marketing”");
    expect(text.detail).toMatch(/^Role: Viewer · Until .*20/);
    // Someone whose account is gone
    expect(notificationText(note({ data: { name: "x", item: "file", role: "viewer" } })).title).toBe("Someone shared “x” with you");
  });

  test("spaces the system named are shown by their translated name", () => {
    const full = note({ kind: "space_full", data: { name: "My files", drive_kind: "personal", used: 950 * 1024 ** 2, quota: 1024 ** 3, percent: 92 } });
    const text = notificationText(full);
    expect(text.title).toBe("The space “My files” is almost full");
    expect(text.detail).toContain("(92%)");
    const ending = note({ kind: "access_expiring", data: { name: "Plans", item: "folder", role: "manager", expires_at: 1_800_000_000 } });
    expect(notificationText(ending).title).toBe("Your access to “Plans” ends soon");
    expect(notificationText(ending).detail).toMatch(/^Manager until /);
  });

  test("opening goes to the folder, or to the file's viewer", () => {
    expect(notificationLink(note({ data: { item: "folder" } }))).toBe("/files/n1");
    expect(notificationLink(note({ kind: "space_full", data: {} }))).toBe("/files/n1");
    expect(notificationLink(note({ data: { item: "file" }, node_id: "a/b" }))).toBe("/view/a%2Fb");
    expect(notificationLink(note({ node_id: null }))).toBeNull();
  });

  test("the bell's count, and the port that follows the encryption", () => {
    expect(unreadBadge(7)).toBe("7");
    expect(unreadBadge(120)).toBe("99+");
    expect(portForSecurity(587, "tls")).toBe(465);
    expect(portForSecurity(0, "none")).toBe(25);
    expect(portForSecurity(2525, "tls")).toBe(2525);
  });
});
