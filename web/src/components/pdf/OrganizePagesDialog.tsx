/**
 * Organize the pages of a PDF: a grid of page pictures to turn, delete and drag into another order, with undo before
 * anything is saved. The result is saved as a new file next to the original, or as a new version of the same file (the
 * earlier one can be restored). The server builds the new PDF from the original's pages; nothing is re-encoded.
 */
import { useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { ArrowLeftIcon, ArrowRightIcon, Loader2Icon, RotateCcwIcon, RotateCwIcon, Trash2Icon, Undo2Icon } from "lucide-react";
import type { PDFDocumentProxy } from "pdfjs-dist/legacy/build/pdf.mjs";
import { api, privateSource, type Node } from "@/api";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { PageImage } from "@/components/pdf/PageImage";
import { UNREADABLE, documentOptions, pdfjs } from "@/lib/pdfjs";
import { changed, initialEntries, moveBefore, picks, remove, rotate, step, type Entry } from "@/lib/pdfOrganize";
import { runPdfJob, stemOf } from "@/lib/pdfPages";
import { t } from "@/lib/i18n";
import { cn } from "@/lib/utils";

/** Undo steps kept */
const HISTORY = 50;
/** Width of a page picture, in CSS pixels */
const WIDTH = 120;

type Status = { kind: "loading" } | { kind: "error"; message: string } | { kind: "ready"; doc: PDFDocumentProxy };

export default function OrganizePagesDialog({ node, canVersion, onClose }: { node: Node; canVersion: boolean; onClose(): void }) {
  const qc = useQueryClient();
  const [status, setStatus] = useState<Status>({ kind: "loading" });
  const [entries, setEntries] = useState<Entry[]>([]);
  const [past, setPast] = useState<Entry[][]>([]);
  const [name, setName] = useState(() => t("{name} (organized).pdf", { name: stemOf(node.name) }));
  const [count, setCount] = useState(0);
  const dragging = useRef<number | null>(null);
  const [over, setOver] = useState<number | null>(null);
  // When the pages were read: a version isn't saved over someone else's newer save
  const openedAt = useRef(node.updated_at);

  useEffect(() => {
    let alive = true;
    const task = pdfjs.getDocument({ ...documentOptions(), url: privateSource.contentUrl(node), disableAutoFetch: true, disableStream: true });
    // A password isn't asked for: the server can't change what it can't read
    task.onPassword = () => alive && setStatus({ kind: "error", message: t("This PDF is encrypted. Remove its password first, then try again") });
    task.promise.then(
      (doc) => {
        if (!alive) return;
        setCount(doc.numPages);
        setEntries(initialEntries(doc.numPages));
        setStatus({ kind: "ready", doc });
      },
      (e: unknown) =>
        alive &&
        setStatus({
          kind: "error",
          message: e instanceof Error && UNREADABLE.includes(e.name) ? t("This file is damaged or isn't a PDF, so it can't be shown.") : t("The PDF couldn't be loaded."),
        }),
    );
    return () => {
      alive = false;
      void task.destroy();
    };
  }, [node]);

  const edit = (next: Entry[]) => {
    setPast((p) => [...p.slice(-(HISTORY - 1)), entries]);
    setEntries(next);
  };
  const undo = () => {
    const last = past[past.length - 1];
    if (!last) return;
    setPast(past.slice(0, -1));
    setEntries(last);
  };
  const toggle = (key: number) => setEntries((es) => es.map((e) => (e.key === key ? { ...e, selected: !e.selected } : e)));
  const selected = entries.filter((e) => e.selected);
  // The pages a button acts on: those selected, else all
  const targets = new Set((selected.length ? selected : entries).map((e) => e.key));
  const dirty = status.kind === "ready" && changed(entries, count);

  const save = (mode: "new" | "version", onlySelected = false) => {
    const chosen = picks(entries, onlySelected);
    if (!chosen.length) return;
    onClose();
    const label = onlySelected ? t("Couldn't extract the pages") : t("Couldn't change the PDF");
    void runPdfJob(
      qc,
      node,
      () => api.pdfPages({ id: node.id, pages: chosen, save: mode, name: mode === "new" ? name.trim() : undefined, base_version: openedAt.current }),
      (job) => (mode === "version" ? t('Saved a new version of "{name}"', { name: job.name ?? node.name }) : t('Created "{name}"', { name: job.name ?? "" })),
      label,
    );
  };

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="flex h-[85vh] max-h-[85vh] flex-col gap-3 sm:max-w-4xl">
        <DialogHeader>
          <DialogTitle>{t("Organize pages")}</DialogTitle>
          <DialogDescription className="truncate">{node.name}</DialogDescription>
        </DialogHeader>
        {status.kind === "ready" && (
          <div className="flex flex-wrap items-center gap-2 text-sm">
            <Button variant="outline" size="sm" disabled={!past.length} onClick={undo}>
              <Undo2Icon /> {t("Undo")}
            </Button>
            <Button variant="outline" size="sm" disabled={!entries.length} onClick={() => edit(rotate(entries, targets, -1))}>
              <RotateCcwIcon /> {t("Turn left")}
            </Button>
            <Button variant="outline" size="sm" disabled={!entries.length} onClick={() => edit(rotate(entries, targets, 1))}>
              <RotateCwIcon /> {t("Turn right")}
            </Button>
            <Button variant="outline" size="sm" disabled={!selected.length} onClick={() => edit(remove(entries, new Set(selected.map((e) => e.key))))}>
              <Trash2Icon /> {t("Delete selected")}
            </Button>
            <span className="ms-auto text-muted-foreground" aria-live="polite">
              {t("{n} page of {total}|{n} pages of {total}", { n: entries.length, total: count })}
              {selected.length > 0 && ` · ${t("{n} selected", { n: selected.length })}`}
            </span>
          </div>
        )}
        <div className="min-h-0 flex-1 overflow-y-auto rounded-lg border bg-muted/30 p-3">
          {status.kind === "loading" && (
            <div className="flex h-full items-center justify-center">
              <Loader2Icon className="size-6 animate-spin text-muted-foreground" aria-label={t("Loading…")} />
            </div>
          )}
          {status.kind === "error" && (
            <p role="alert" className="flex h-full items-center justify-center text-center text-sm">
              {status.message}
            </p>
          )}
          {status.kind === "ready" && !entries.length && (
            <p className="flex h-full items-center justify-center text-sm text-muted-foreground">{t("Every page is deleted. Undo to bring them back.")}</p>
          )}
          {status.kind === "ready" && (
            <ul className="grid grid-cols-[repeat(auto-fill,minmax(9rem,1fr))] gap-3" aria-label={t("Pages")}>
              {entries.map((e, i) => (
                <li
                  key={e.key}
                  draggable
                  onDragStart={(ev) => {
                    dragging.current = e.key;
                    ev.dataTransfer.effectAllowed = "move";
                    ev.dataTransfer.setData("text/plain", String(e.key));
                  }}
                  onDragEnd={() => {
                    dragging.current = null;
                    setOver(null);
                  }}
                  onDragOver={(ev) => {
                    if (dragging.current === null) return;
                    ev.preventDefault();
                    setOver(e.key);
                  }}
                  onDrop={(ev) => {
                    ev.preventDefault();
                    if (dragging.current !== null) edit(moveBefore(entries, dragging.current, e.key));
                    dragging.current = null;
                    setOver(null);
                  }}
                  className={cn(
                    "flex flex-col items-center gap-1.5 rounded-lg p-2",
                    e.selected && "bg-primary/10 ring-2 ring-primary",
                    over === e.key && dragging.current !== e.key && "ring-2 ring-ring",
                  )}
                >
                  <div className="relative">
                    <PageImage doc={status.doc} page={e.page + 1} turn={e.turn} width={WIDTH} />
                    <Checkbox className="absolute start-1 top-1 bg-background" checked={e.selected} onCheckedChange={() => toggle(e.key)} aria-label={t("Select page {n}", { n: i + 1 })} />
                  </div>
                  <span className="text-xs text-muted-foreground">{e.page + 1 === i + 1 ? i + 1 : t("{n} (was {old})", { n: i + 1, old: e.page + 1 })}</span>
                  <div className="flex gap-0.5">
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      title={t("Turn left")}
                      aria-label={t("Turn page {n} left", { n: i + 1 })}
                      onClick={() => edit(rotate(entries, new Set([e.key]), -1))}
                    >
                      <RotateCcwIcon />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      title={t("Turn right")}
                      aria-label={t("Turn page {n} right", { n: i + 1 })}
                      onClick={() => edit(rotate(entries, new Set([e.key]), 1))}
                    >
                      <RotateCwIcon />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      title={t("Move earlier")}
                      aria-label={t("Move page {n} earlier", { n: i + 1 })}
                      disabled={i === 0}
                      onClick={() => edit(step(entries, e.key, -1))}
                    >
                      <ArrowLeftIcon className="rtl:rotate-180" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      title={t("Move later")}
                      aria-label={t("Move page {n} later", { n: i + 1 })}
                      disabled={i === entries.length - 1}
                      onClick={() => edit(step(entries, e.key, 1))}
                    >
                      <ArrowRightIcon className="rtl:rotate-180" />
                    </Button>
                    <Button variant="ghost" size="icon-sm" title={t("Delete page")} aria-label={t("Delete page {n}", { n: i + 1 })} onClick={() => edit(remove(entries, new Set([e.key])))}>
                      <Trash2Icon />
                    </Button>
                  </div>
                </li>
              ))}
            </ul>
          )}
        </div>
        <DialogFooter className="items-center gap-2 sm:justify-between">
          <label className="flex min-w-0 items-center gap-2 text-sm">
            <span className="shrink-0">{t("File name")}</span>
            <Input value={name} onChange={(e) => setName(e.target.value)} className="min-w-0 sm:w-64" maxLength={200} />
          </label>
          <div className="flex flex-wrap justify-end gap-2">
            <Button variant="outline" disabled={!selected.length || !name.trim()} onClick={() => save("new", true)}>
              {t("Extract selected pages")}
            </Button>
            <Button variant="outline" disabled={!entries.length || !name.trim()} onClick={() => save("new")}>
              {t("Save as new file")}
            </Button>
            {canVersion && (
              <Button disabled={!dirty || !entries.length} onClick={() => save("version")}>
                {t("Save as new version")}
              </Button>
            )}
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
