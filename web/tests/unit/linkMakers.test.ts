import { describe, expect, test } from "vite-plus/test";
import type { LinkMakers, Me, Role } from "@/api";
import { capsOf, linkAllowed } from "@/lib/drives";

const me = (fields: Partial<Me> = {}) => ({ role: "user", can_write: true, can_delete: true, can_share: true, ...fields }) as Me;

describe("who may make share links (as the server's tree::may_link decides)", () => {
  test("each choice of the space, for each role", () => {
    const roles: Role[] = ["viewer", "editor", "manager", "owner"];
    const allowed = (makers: LinkMakers) => roles.filter((r) => linkAllowed(r, makers));
    expect(allowed("editors")).toEqual(["editor", "manager", "owner"]);
    expect(allowed("managers")).toEqual(["manager", "owner"]);
    expect(allowed("nobody")).toEqual([]);
    // A space that doesn't say (lists that mix places) is as before
    expect(linkAllowed("editor")).toBe(true);
  });

  test("the account's own permission still applies, and the rest of the capabilities don't change", () => {
    expect(capsOf("editor", me(), false, "managers").share).toBe(false);
    expect(capsOf("manager", me(), false, "managers").share).toBe(true);
    expect(capsOf("manager", me({ can_share: false }), false, "editors").share).toBe(false);
    expect(capsOf("manager", me({ role: "admin", can_share: false }), false, "managers").share).toBe(true);
    expect(capsOf("owner", me({ role: "admin" }), false, "nobody").share).toBe(false);
    expect(capsOf("editor", me(), false, "nobody")).toMatchObject({ write: true, del: true, manage: false });
  });
});
