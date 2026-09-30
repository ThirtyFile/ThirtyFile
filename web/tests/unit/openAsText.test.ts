// Which files the preview screen offers to open in the text editor (components/FileIcon.tsx, mayOpenAsText)
import { expect, test } from "vitest";
import type { Node } from "@/api";
import { MAX_TEXT_BYTES, isTextLike, mayOpenAsText } from "@/components/FileIcon";

const file = (name: string, size = 100, mime = "application/octet-stream"): Node => ({
  id: name,
  parent_id: null,
  kind: "file",
  name,
  size,
  mime,
  created_at: 0,
  updated_at: 0,
  trashed_at: null,
  drive_id: null,
  owner_name: "",
  is_favorite: false,
});

test("files of an unknown kind can be opened as text", () => {
  const names = ["Dockerfile", "README", "notes.customext", "Makefile", "go.sum", "go.mod"];
  expect(names.filter((name) => isTextLike(file(name)))).toEqual([]);
  expect(names.filter((name) => !mayOpenAsText(file(name)))).toEqual([]);
});

test("text too large to open by itself is offered, and then told it is too large", () => {
  expect(mayOpenAsText(file("server.log", MAX_TEXT_BYTES + 1, "text/plain"))).toBe(true);
});

test("known binary kinds, folders and files that already open as text aren't offered", () => {
  expect(mayOpenAsText(file("photo.jpg", 100, "image/jpeg"))).toBe(false);
  expect(mayOpenAsText(file("movie.mp4", 100, "video/mp4"))).toBe(false);
  expect(mayOpenAsText(file("archive.zip", 100, "application/zip"))).toBe(false);
  expect(mayOpenAsText(file("report.docx"))).toBe(false);
  expect(mayOpenAsText(file("paper.pdf", 100, "application/pdf"))).toBe(false);
  expect(mayOpenAsText(file("main.rs", 100, "text/x-rust"))).toBe(false);
  expect(mayOpenAsText(file("empty.bin", 0))).toBe(false);
  expect(mayOpenAsText({ ...file("Folder"), kind: "folder" })).toBe(false);
});
