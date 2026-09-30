import type { FolderSpan } from "@/lib/span";
import { createStore, useStore } from "@/lib/store";

/** File explorer cut / copy (Ctrl+X / Ctrl+C); the actual move or copy happens on paste */
export interface FileClipboard {
  mode: "cut" | "copy";
  ids: string[];
  /** Items of a large folder selected without being loaded (lib/span) */
  span?: FolderSpan | null;
  /** How many items there are, with the span's */
  count?: number;
  /** Cut: the folder each item was in, so the move can be taken back after pasting */
  origins?: Map<string, string>;
}

const clip = createStore<FileClipboard | null>(null);

export function setClipboard(next: FileClipboard | null) {
  clip.set(next);
}

export function useClipboard() {
  return useStore(clip);
}
