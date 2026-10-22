// What the menus offer, as the server allows it: contributors change only what they added, and managing access needs a
// manager who may share (the server's tree::allows and drives::can_manage_node)
import { describe, expect, test } from "vite-plus/test";
import type { Me } from "@/api";
import { capsFor, capsOf } from "@/lib/drives";

const me = (fields: Partial<Me> = {}) => ({ role: "user", username: "amy", can_write: true, can_delete: true, can_share: true, guest: false, ...fields }) as Me;
const mine = { owner_name: "amy" };
const theirs = { owner_name: "ben" };
const found = { owner_name: "" };

describe("capabilities", () => {
  test("a contributor adds, and changes or deletes only what they added", () => {
    const caps = capsOf("contributor", me());
    expect(caps).toMatchObject({ add: true, write: false, del: false, ownWrite: true, ownDel: true });
    expect(capsFor(caps, [mine], me())).toMatchObject({ write: true, del: true });
    expect(capsFor(caps, [mine, theirs], me())).toMatchObject({ write: false, del: false });
    // Found on a folder space's disk: nobody added it
    expect(capsFor(caps, [found], me())).toMatchObject({ write: false, del: false });
    expect(capsFor(caps, [], me())).toMatchObject({ write: false, del: false });
  });

  test("a contributor whose account may not write or delete gets neither, theirs or not", () => {
    const caps = capsOf("contributor", me({ can_write: false, can_delete: false }));
    expect(capsFor(caps, [mine], me())).toMatchObject({ add: false, write: false, del: false });
  });

  test("a read-only space offers no changes to anyone", () => {
    expect(capsOf("owner", me(), true)).toMatchObject({ add: false, write: false, del: false, ownWrite: false, ownDel: false });
    expect(capsOf("contributor", me(), true)).toMatchObject({ ownWrite: false, ownDel: false });
  });

  test("managing access takes a manager who may share, and never a guest", () => {
    expect(capsOf("manager", me()).manage).toBe(true);
    expect(capsOf("editor", me()).manage).toBe(false);
    expect(capsOf("manager", me({ can_share: false })).manage).toBe(false);
    expect(capsOf("manager", me({ role: "admin", can_share: false })).manage).toBe(true);
    expect(capsOf("manager", me({ guest: true })).manage).toBe(false);
  });
});
