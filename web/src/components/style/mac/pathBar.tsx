/**
 * The Mac style's path bar, at the bottom of the window: the open folder's path, each part a link that takes dropped
 * items and files (the address bar's parts, as in the Windows style). There is no box to type a path into: "Go to
 * folder" (the style's key, or the Actions menu) asks for one.
 */
import { useLayoutEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { CrumbItem, crumbPath, useGoToPath } from "@/components/frame/AddressBar";
import type { FramePlace } from "../types";
import { t } from "@/lib/i18n";
import { createStore, useStore } from "@/lib/store";

export function PathBar({ place }: { place: FramePlace }) {
  const { crumbs, icon: Icon } = place;
  const trail = useRef<HTMLElement>(null);
  // A long path shows its end, the folder open, scrolling back for the rest; the keyboard can reach it to scroll it
  const [scrolls, setScrolls] = useState(false);
  const key = crumbs.map((c) => c.label).join("/");
  useLayoutEffect(() => {
    const el = trail.current;
    if (!el) return;
    const toEnd = () => {
      el.scrollLeft = el.scrollWidth;
      setScrolls(el.scrollWidth > el.clientWidth);
    };
    toEnd();
    const ro = new ResizeObserver(toEnd);
    ro.observe(el);
    return () => ro.disconnect();
  }, [key]);
  return (
    <div className="flex h-7 shrink-0 items-center gap-1.5 border-t px-2 text-xs">
      <Icon aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
      <nav
        ref={trail}
        aria-label={t("Path bar")}
        tabIndex={scrolls ? 0 : undefined}
        // The first part has no arrow before it
        className="flex min-w-0 flex-1 items-center overflow-x-auto rounded-sm whitespace-nowrap outline-none [scrollbar-width:none] focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset [&::-webkit-scrollbar]:hidden [&>span:first-child>svg:first-child]:hidden"
      >
        {crumbs.map((c, i) => (
          <CrumbItem key={i} crumb={c} last={i === crumbs.length - 1} path={crumbPath(crumbs.slice(0, i + 1))} />
        ))}
      </nav>
    </div>
  );
}

const asking = createStore(false);

/** Opens "Go to folder" (the frame shows it) */
export function openGoToFolder() {
  asking.set(true);
}

/** "Go to folder": a path typed or pasted, as the Windows style's address bar takes them, or a link to a page here */
export function GoToFolder({ path }: { path: string }) {
  const open = useStore(asking);
  if (!open) return null;
  return <GoToFolderDialog path={path} onClose={() => asking.set(false)} />;
}

function GoToFolderDialog({ path, onClose }: { path: string; onClose(): void }) {
  const [text, setText] = useState(path);
  const { finding, go } = useGoToPath();
  const input = useRef<HTMLInputElement>(null);
  const submit = async () => {
    const typed = text.trim();
    if (!typed || typed === path) return onClose();
    if (await go(typed)) onClose();
    else input.current?.select();
  };
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-md">
        <form
          className="grid gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <DialogHeader>
            <DialogTitle>{t("Go to folder")}</DialogTitle>
            <DialogDescription>{t("Type or paste a path, then press Enter")}</DialogDescription>
          </DialogHeader>
          <Input
            ref={input}
            aria-label={t("Full path")}
            value={text}
            onChange={(e) => setText(e.target.value)}
            readOnly={finding}
            autoFocus
            spellCheck={false}
            autoComplete="off"
            onFocus={(e) => e.currentTarget.select()}
          />
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={finding}>
              {t("Go")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
