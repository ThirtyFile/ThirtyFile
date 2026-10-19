// A tab's history: a page that replaces the one before it (a search refined as it is typed) takes its place, so Back
// goes to where the search started in one step
import { expect, test } from "vitest";
import { activeTab, loadTabs, syncLocation } from "@/tabs";

test("new tabs start at the first page shown, without their unseen home page before it", () => {
  loadTabs(9000);
  syncLocation("/search?q=report");
  expect(activeTab().entries).toEqual(["/search?q=report"]);
  expect(activeTab().index).toBe(0);
});

test("a replaced page takes the place of the one before it; a new one is added after it", () => {
  loadTabs(9001);
  syncLocation("/files");
  syncLocation("/files/folder-1");
  syncLocation("/search?q=a&in=folder-1");
  syncLocation("/search?q=ab&in=folder-1", true);
  syncLocation("/search?q=abc&in=folder-1&type=doc", true);
  expect(activeTab().entries).toEqual(["/files", "/files/folder-1", "/search?q=abc&in=folder-1&type=doc"]);
  expect(activeTab().index).toBe(2);
  syncLocation("/files/folder-2");
  expect(activeTab().entries.at(-1)).toBe("/files/folder-2");
  expect(activeTab().index).toBe(3);
});
