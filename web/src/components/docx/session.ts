/**
 * Word editing session: the opened file and its edits not saved yet. Switching tabs and coming back reuses it, so
 * unsaved edits aren't lost (the editing frame is made again, opening the edited document.xml).
 */
import JSZip from "jszip";
import { api, fetchOffice, type FileSource, type Node } from "@/api";
import { getDraft, onDraftRemoved } from "@/lib/drafts";
import { cloneZip, readEntry } from "@/ooxml/core/package";
import type { Caret } from "@/ooxml/docx/editor";
import { t } from "@/lib/i18n";

export interface DocxSession {
  /** The file as opened: saving puts the edited document.xml into a copy of it, every other part stays as it was */
  zip: JSZip;
  /** Its bytes, for the editing frame (each frame gets a copy) */
  buffer: ArrayBuffer;
  /** updated_at when opened or last saved, sent when saving so a newer version isn't overwritten */
  base: number;
  /** Path of document.xml (known once the frame has answered) */
  path: string | null;
  /** document.xml as edited, and as last saved here: null is as it was in the file when opened */
  current: string | null;
  saved: string | null;
  caret: Caret | null;
  scroll: number;
}

const sessions = new Map<string, DocxSession>();

export const isDirty = (s: DocxSession) => s.current !== s.saved;

// Draft removed while the session still has unsaved edits = the edits were discarded (closing the tab, say)
onDraftRemoved((nodeId) => {
  const s = sessions.get(nodeId);
  if (s && isDirty(s)) sessions.delete(nodeId);
});

/** The kept session of this file, when it can be used: it has unsaved edits, or the file hasn't changed since */
export function reusableSession(node: Node): DocxSession | undefined {
  const s = sessions.get(node.id);
  if (!s) return undefined;
  if (isDirty(s) ? !!getDraft(node.id, "docx") : s.base === node.updated_at) return s;
  sessions.delete(node.id);
  return undefined;
}

export async function openSession(node: Node, source: FileSource, signal?: AbortSignal): Promise<DocxSession> {
  const buffer = await fetchOffice(source.contentUrl(node), signal);
  const zip = await JSZip.loadAsync(buffer.slice(0));
  return { zip, buffer, base: node.updated_at, path: null, current: null, saved: null, caret: null, scroll: 0 };
}

export function keepSession(nodeId: string, s: DocxSession) {
  sessions.set(nodeId, s);
}

export function releaseIfClean(nodeId: string, s: DocxSession) {
  if (!isDirty(s) && sessions.get(nodeId) === s) sessions.delete(nodeId);
}

export function dropSession(nodeId: string) {
  sessions.delete(nodeId);
}

const DOCX_TYPE = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

/**
 * Saves `xml` (null: document.xml as it was when opened) into the file. Only document.xml changes: the archive's other
 * entries are written as they are.
 */
export async function saveSession(s: DocxSession, nodeId: string, path: string, xml: string | null): Promise<Node> {
  const text = xml ?? (await readEntry(s.zip, path, "string"));
  if (text === null) throw new Error(t("Couldn't save"));
  const work = cloneZip(s.zip);
  work.file(path, text, { createFolders: false });
  // Folder entries aren't written (only the entries themselves are dropped, not the files under them)
  for (const [p, entry] of Object.entries(work.files)) if (entry.dir) delete work.files[p];
  const blob = await work.generateAsync({ type: "blob", compression: "DEFLATE", mimeType: DOCX_TYPE });
  const node = await api.saveContent(nodeId, blob, s.base);
  s.base = node.updated_at;
  s.saved = xml;
  return node;
}
