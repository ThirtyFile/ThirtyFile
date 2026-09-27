/** Types shared by the file explorer */
import type { Located, Node } from "@/api";

export type Item = Node & Partial<Pick<Located, "location">>;

export type DialogState =
  | { t: "rename"; node: Pick<Node, "id" | "name"> }
  | { t: "move" | "copy"; ids: string[] }
  | { t: "trash"; ids: string[] }
  | { t: "share"; node: Node }
  | { t: "access"; nodeId: string };

/** Don't handle shortcuts while focus is in an input */
export function isTyping(target: EventTarget | null) {
  const el = target as HTMLElement | null;
  return !!el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable);
}
