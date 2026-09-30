/** Types shared by the file explorer */
import type { Located, Node } from "@/api";
import type { Picked } from "@/lib/span";

export type Item = Node & Partial<Pick<Located, "location">>;

export type DialogState =
  | { t: "rename"; node: Pick<Node, "id" | "name"> }
  | { t: "move" | "copy"; picked: Picked }
  | { t: "trash"; picked: Picked }
  | { t: "share"; node: Node }
  | { t: "access"; nodeId: string };

/** Inputs that take no typing: shortcuts keep working while one has the focus */
const NOT_TYPED = new Set(["checkbox", "radio", "button", "submit", "reset", "range", "color", "file"]);

/** Don't handle shortcuts while focus is in a text box */
export function isTyping(target: EventTarget | null) {
  const el = target as HTMLElement | null;
  if (!el) return false;
  if (el.tagName === "INPUT") return !NOT_TYPED.has((el as HTMLInputElement).type);
  return el.tagName === "TEXTAREA" || el.isContentEditable;
}
