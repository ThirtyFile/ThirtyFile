import { shortcut } from "@/lib/keys";

/** Shortcut hint on the right of a menu item (with ⌘ for Ctrl on macOS) */
export function Kbd({ children }: { children: string | undefined }) {
  // An action the style has no key for
  if (!children) return null;
  return <span className="ml-auto pl-4 text-[11px] text-muted-foreground">{shortcut(children)}</span>;
}
