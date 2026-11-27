/**
 * Renaming several items at once: the new name each gets, and what keeps one from being used. The names are worked out
 * here, so the preview is exactly what the server is asked for.
 */

/** Characters a name can't have (as on Windows; the server refuses them too) */
export const FORBIDDEN = [...'\\/:*?"<>|'];

export type RenameRule =
  /** Every occurrence of `find` becomes `replace` */
  | { mode: "replace"; find: string; replace: string; matchCase: boolean; extensions: boolean }
  /** `text` before or after the name */
  | { mode: "add"; text: string; before: boolean; extensions: boolean }
  /** `base` and a number counting up from `start`, in the order of the items */
  | { mode: "number"; base: string; start: number; digits: number };

export interface Renamable {
  id: string;
  parent_id: string | null;
  kind: string;
  name: string;
}

/** The name and its extension (with the dot); folders and names such as ".env" have none */
export function splitName(name: string, folder: boolean): [string, string] {
  const dot = folder ? -1 : name.lastIndexOf(".");
  return dot > 0 ? [name.slice(0, dot), name.slice(dot)] : [name, ""];
}

/** The name the `index`th item (from 0) gets; its own when the rule changes nothing */
export function newName(item: Pick<Renamable, "name" | "kind">, index: number, rule: RenameRule): string {
  const keep = rule.mode === "number" || !rule.extensions;
  const [stem, ext] = keep ? splitName(item.name, item.kind === "folder") : [item.name, ""];
  let next = stem;
  if (rule.mode === "replace") {
    if (rule.find) {
      const find = rule.find.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      next = stem.replace(new RegExp(find, rule.matchCase ? "gu" : "giu"), () => rule.replace);
    }
  } else if (rule.mode === "add") {
    next = rule.before ? rule.text + stem : stem + rule.text;
  } else {
    next = rule.base + String(rule.start + index).padStart(rule.digits, "0");
  }
  // (the server trims a name too)
  return next === stem ? item.name : (next + ext).trim();
}

/** Why the server would refuse this name, in its words (`tServer` translates them); null when it is fine */
export function nameError(name: string): string | null {
  if (!name) return "Name can't be blank";
  if (new TextEncoder().encode(name).length > 255) return "Name is too long";
  if (name === "." || name === "..") return "Invalid name";
  const bad = [...name].find((c) => FORBIDDEN.includes(c) || /\p{Cc}/u.test(c));
  if (bad) return `Name can't contain ${/\p{Cc}/u.test(bad) ? "control characters" : bad}`;
  if (name.endsWith(".")) return "Name can't end with a period";
  if (/^(con|prn|aux|nul|(com|lpt)[1-9])(\.|$)/i.test(name)) return "This name is reserved by Windows";
  return null;
}

export type NameProblem = { type: "invalid"; message: string } | { type: "repeated" } | { type: "taken" };

/** What a name is looked up by in the names taken: the folder, and the name as asked about */
export const takenKey = (parent: string | null, name: string) => `${parent}\n${name}`;

/**
 * What keeps each item from getting its new name (null: nothing, or its name stays). `taken` holds the names
 * (`takenKey`) that items which keep theirs have.
 */
export function nameProblems(items: readonly Renamable[], names: readonly string[], taken: ReadonlySet<string>): (NameProblem | null)[] {
  const key = (i: number) => takenKey(items[i].parent_id, names[i].toLowerCase());
  const count = new Map<string, number>();
  items.forEach((_, i) => count.set(key(i), (count.get(key(i)) ?? 0) + 1));
  return items.map((item, i) => {
    if (names[i] === item.name) return null;
    const message = nameError(names[i]);
    if (message) return { type: "invalid", message };
    if (count.get(key(i))! > 1) return { type: "repeated" };
    return taken.has(takenKey(item.parent_id, names[i])) ? { type: "taken" } : null;
  });
}
