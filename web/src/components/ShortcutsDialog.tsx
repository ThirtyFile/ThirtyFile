/**
 * The keyboard shortcuts of the file explorer, opened with "?" (Shift+/) or from the "See more" menu: those of the
 * style in use, from its keyboard map (components/style)
 */
import { Fragment } from "react";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { useStyleKit } from "@/components/style";
import { t } from "@/lib/i18n";
import { shortcut } from "@/lib/keys";
import { keysOf } from "@/lib/style/keymap";
import { createStore, useStore } from "@/lib/store";

const open = createStore(false);

export function openShortcuts() {
  open.set(true);
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

/** Mounted once in the page frame */
export function ShortcutsHost() {
  const shown = useStore(open);
  const kit = useStyleKit();
  if (!shown) return null;
  const { note, groups } = kit.shortcuts();
  return (
    <Dialog open onOpenChange={(o) => !o && open.set(false)}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("Keyboard shortcuts")}</DialogTitle>
          <DialogDescription>{note}</DialogDescription>
        </DialogHeader>
        <div className="-mx-1 grid max-h-[60vh] gap-4 overflow-y-auto px-1">
          {groups.map(({ title, rows }) => (
            <section key={title}>
              <h3 className="mb-1.5 text-xs font-medium text-muted-foreground">{title}</h3>
              <table className="w-full text-sm">
                <tbody>
                  {rows.map(({ actions, label }) => {
                    const keys = keysOf(kit.keys, actions);
                    return (
                      <tr key={keys} className="border-t first:border-t-0">
                        <td className="w-[45%] py-1.5 pr-3 align-top">
                          <Keys keys={keys} />
                        </td>
                        <td className="py-1.5 align-top">{label}</td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </section>
          ))}
        </div>
      </DialogContent>
    </Dialog>
  );
}
