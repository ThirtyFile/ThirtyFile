import { CheckIcon } from "lucide-react";
import { shortcut } from "@/lib/keys";

/** Shortcut hint on the right of a menu item (with ⌘ for Ctrl on macOS) */
export function Kbd({ children }: { children: string }) {
  return <span className="ml-auto pl-4 text-[11px] text-muted-foreground">{shortcut(children)}</span>;
}

/** Check mark in a menu (reserves the space when unchecked, for alignment) */
export function Check({ on }: { on: boolean }) {
  return <CheckIcon className={on ? "" : "invisible"} />;
}
