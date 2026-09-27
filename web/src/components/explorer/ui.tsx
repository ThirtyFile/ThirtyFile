import type { ReactNode } from "react";
import { CheckIcon } from "lucide-react";

/** Shortcut hint on the right of a menu item */
export function Kbd({ children }: { children: ReactNode }) {
  return <span className="ml-auto pl-4 text-[11px] text-muted-foreground">{children}</span>;
}

/** Check mark in a menu (reserves the space when unchecked, for alignment) */
export function Check({ on }: { on: boolean }) {
  return <CheckIcon className={on ? "" : "invisible"} />;
}
