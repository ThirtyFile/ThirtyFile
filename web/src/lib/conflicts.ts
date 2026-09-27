/** Name clashes before uploading, moving, copying or restoring: which items clash, and applying the answers */

/** The answer to "the destination already has an item with this name" (the server's `Resolution`) */
export type Resolution = "replace" | "skip" | "keep";

/** One item whose name the destination already has */
export interface Clash {
  /** The item's id (move, copy, restore) or, for uploads, its top-level name */
  key: string;
  name: string;
  kind: "file" | "folder";
  /** What is known about the incoming item (a picked folder has no single size) */
  size?: number;
  modified?: number;
  existing: { kind: "file" | "folder"; size: number; updated_at: number };
}

/** An answer, and whether it applies to every clash still to come ("Do this for all") */
export interface Answer {
  choice: Resolution;
  forAll: boolean;
}

/** The files and folders a pick or drop puts directly in the destination: a folder's files all count as that folder */
export function topLevel(files: readonly { file: File; relativePath: string }[]): { name: string; kind: "file" | "folder"; size?: number; modified?: number }[] {
  const seen = new Map<string, { name: string; kind: "file" | "folder"; size?: number; modified?: number }>();
  for (const { file, relativePath } of files) {
    const folder = relativePath.split("/").find(Boolean);
    const name = folder ?? file.name;
    if (seen.has(name)) continue;
    seen.set(name, folder ? { name, kind: "folder" } : { name, kind: "file", size: file.size, modified: Math.floor(file.lastModified / 1000) });
  }
  return [...seen.values()];
}

/**
 * Asks about each clash in turn; an answer "for all" is used for the rest without asking. Null when a question is
 * cancelled (nothing should happen then).
 */
export async function resolveAll(clashes: readonly Clash[], ask: (clash: Clash, remaining: number) => Promise<Answer | null>): Promise<Map<string, Resolution> | null> {
  const answers = new Map<string, Resolution>();
  let all: Resolution | undefined;
  for (let i = 0; i < clashes.length; i++) {
    const clash = clashes[i];
    if (all === undefined) {
      const answer = await ask(clash, clashes.length - i - 1);
      if (!answer) return null;
      if (answer.forAll) all = answer.choice;
      answers.set(clash.key, answer.choice);
    } else {
      answers.set(clash.key, all);
    }
  }
  return answers;
}

/**
 * The files to upload after the answers: skipped names are left out, the rest carry what the server does when their
 * name is taken. A folder answered "replace" is merged into the one there, replacing the files it has too.
 */
export function applyToUpload<T extends { file: File; relativePath: string }>(files: readonly T[], answers: ReadonlyMap<string, Resolution>): (T & { onConflict: "replace" | "keep" })[] {
  const out: (T & { onConflict: "replace" | "keep" })[] = [];
  for (const f of files) {
    const top = f.relativePath.split("/").find(Boolean) ?? f.file.name;
    const answer = answers.get(top);
    if (answer === "skip") continue;
    out.push({ ...f, onConflict: answer === "replace" ? "replace" : "keep" });
  }
  return out;
}

/** The name the server gives a kept copy first, "Report (1).docx" (a folder's name has no extension) */
export function numberedName(name: string, isFolder: boolean): string {
  const dot = name.lastIndexOf(".");
  if (isFolder || dot <= 0) return `${name} (1)`;
  return `${name.slice(0, dot)} (1)${name.slice(dot)}`;
}
