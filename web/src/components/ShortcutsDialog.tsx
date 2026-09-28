/** The keyboard shortcuts of the file explorer, opened with "?" (Shift+/) or from the "See more" menu */
import { Fragment, useSyncExternalStore } from "react";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { t } from "@/lib/i18n";
import { shortcut } from "@/lib/keys";

let open = false;
const listeners = new Set<() => void>();
function setOpen(next: boolean) {
  open = next;
  listeners.forEach((l) => l());
}

export function openShortcuts() {
  setOpen(true);
}

/** Keys joined by "+" are pressed together; "/" separates alternatives */
function Keys({ keys }: { keys: string }) {
  return (
    <span className="flex flex-wrap items-center gap-1">
      {keys.split(" / ").map((combo, i) => (
        <Fragment key={i}>
          {i > 0 && <span className="text-muted-foreground">/</span>}
          <kbd className="rounded border bg-muted px-1.5 py-0.5 font-mono text-[11px] whitespace-nowrap">{shortcut(combo)}</kbd>
        </Fragment>
      ))}
    </span>
  );
}

const groups = (): [string, [string, string][]][] => [
  [
    t("Getting around"),
    [
      ["Alt+↑", t("Up one folder")],
      ["Alt+← / Backspace", t("Back")],
      ["Alt+→", t("Forward")],
      ["Ctrl+L / Alt+D", t("Go to the address bar")],
      ["Ctrl+F / F3", t("Go to the search box")],
      ["F5", t("Refresh the list")],
      ["← / →", t("In an open file: the previous or next file of its folder")],
    ],
  ],
  [
    t("Selecting"),
    [
      ["↑ / ↓ / Home / End", t("Move through the list (also ← and → in the icon view); hold Shift to select as you go")],
      ["Space", t("Select the item with the focus")],
      ["Ctrl+Space", t("Add the item with the focus to the selection, or remove it")],
      ["Ctrl+A", t("Select everything")],
      ["A–Z", t("Go to the next item whose name starts with the letters typed")],
      ["Esc", t("Clear the selection")],
    ],
  ],
  [
    t("Working with items"),
    [
      ["Enter", t("Open")],
      ["F2", t("Rename")],
      ["Ctrl+X / Ctrl+C / Ctrl+V", t("Cut, copy, paste")],
      ["Ctrl+Z", t("Undo the last move, rename or delete")],
      ["Delete", t("Move to trash")],
      ["Shift+Delete", t("Delete permanently")],
      ["Ctrl+Shift+N", t("New folder")],
      ["Alt+Enter", t("Show details")],
      ["?", t("Show these shortcuts")],
    ],
  ],
];

/** Mounted once in the page frame */
export function ShortcutsHost() {
  const shown = useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => open,
  );
  if (!shown) return null;
  return (
    <Dialog open onOpenChange={(o) => !o && setOpen(false)}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("Keyboard shortcuts")}</DialogTitle>
          <DialogDescription>{t("Like File Explorer. Shortcuts don't apply while you're typing in a box.")}</DialogDescription>
        </DialogHeader>
        <div className="-mx-1 grid max-h-[60vh] gap-4 overflow-y-auto px-1">
          {groups().map(([title, rows]) => (
            <section key={title}>
              <h3 className="mb-1.5 text-xs font-medium text-muted-foreground">{title}</h3>
              <table className="w-full text-sm">
                <tbody>
                  {rows.map(([keys, action]) => (
                    <tr key={keys} className="border-t first:border-t-0">
                      <td className="w-[45%] py-1.5 pr-3 align-top">
                        <Keys keys={keys} />
                      </td>
                      <td className="py-1.5 align-top">{action}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </section>
          ))}
        </div>
      </DialogContent>
    </Dialog>
  );
}
