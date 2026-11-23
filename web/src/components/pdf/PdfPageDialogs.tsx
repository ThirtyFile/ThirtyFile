/** Small dialogs for the other page operations: take pages by range, split into files, and combine PDFs */
import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { ArrowDownIcon, ArrowUpIcon } from "lucide-react";
import { api, type Node } from "@/api";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { runPdfJob, stemOf } from "@/lib/pdfPages";
import { t } from "@/lib/i18n";

function Shell({
  title,
  description,
  children,
  onClose,
  onSubmit,
  confirm,
  disabled,
}: {
  title: string;
  description: string;
  children: React.ReactNode;
  onClose(): void;
  onSubmit(): void;
  confirm: string;
  disabled: boolean;
}) {
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-md">
        <form
          className="flex flex-col gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            if (!disabled) onSubmit();
          }}
        >
          <DialogHeader>
            <DialogTitle>{title}</DialogTitle>
            <DialogDescription className="truncate">{description}</DialogDescription>
          </DialogHeader>
          {children}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={disabled}>
              {confirm}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** The dialog for what was asked in the menu */
export function PdfPageDialogs({ dialog, onClose }: { dialog: { t: "pdf-extract"; node: Node } | { t: "pdf-split"; node: Node } | { t: "pdf-combine"; nodes: Node[] }; onClose(): void }) {
  if (dialog.t === "pdf-extract") return <ExtractPagesDialog node={dialog.node} onClose={onClose} />;
  if (dialog.t === "pdf-split") return <SplitPdfDialog node={dialog.node} onClose={onClose} />;
  return <CombinePdfsDialog nodes={dialog.nodes} onClose={onClose} />;
}

/** Pages written like "1-3, 5" go into a new PDF, in the order written */
export function ExtractPagesDialog({ node, onClose }: { node: Node; onClose(): void }) {
  const qc = useQueryClient();
  const [ranges, setRanges] = useState("");
  const [name, setName] = useState(() => t("{name} (pages).pdf", { name: stemOf(node.name) }));
  return (
    <Shell
      title={t("Extract pages")}
      description={node.name}
      onClose={onClose}
      confirm={t("Extract")}
      disabled={!ranges.trim() || !name.trim()}
      onSubmit={() => {
        onClose();
        void runPdfJob(
          qc,
          node,
          () => api.pdfPages({ id: node.id, ranges: ranges.trim(), save: "new", name: name.trim() }),
          (job) => t('Created "{name}"', { name: job.name ?? "" }),
          t("Couldn't extract the pages"),
        );
      }}
    >
      <label className="flex flex-col gap-1.5 text-sm">
        {t("Pages to take")}
        <Input autoFocus value={ranges} onChange={(e) => setRanges(e.target.value)} placeholder={t("For example 1-3, 5")} />
      </label>
      <label className="flex flex-col gap-1.5 text-sm">
        {t("File name")}
        <Input value={name} onChange={(e) => setName(e.target.value)} maxLength={200} />
      </label>
    </Shell>
  );
}

/** One file per page, or one per range */
export function SplitPdfDialog({ node, onClose }: { node: Node; onClose(): void }) {
  const qc = useQueryClient();
  const [each, setEach] = useState(true);
  const [ranges, setRanges] = useState("");
  return (
    <Shell
      title={t("Split PDF")}
      description={node.name}
      onClose={onClose}
      confirm={t("Split")}
      disabled={!each && !ranges.trim()}
      onSubmit={() => {
        onClose();
        void runPdfJob(
          qc,
          node,
          () => api.pdfSplit(node.id, each ? undefined : ranges.trim()),
          (job) => {
            const files = (job.result as { files?: number } | undefined)?.files ?? 1;
            return t("Split into {n} file|Split into {n} files", { n: files });
          },
          t("Couldn't split the PDF"),
        );
      }}
    >
      <label className="flex items-center gap-2 text-sm">
        <input type="radio" name="split" checked={each} onChange={() => setEach(true)} /> {t("One file for each page")}
      </label>
      <label className="flex items-center gap-2 text-sm">
        <input type="radio" name="split" checked={!each} onChange={() => setEach(false)} /> {t("One file for each range of pages")}
      </label>
      {!each && (
        <label className="flex flex-col gap-1.5 text-sm">
          {t("Page ranges")}
          <Input autoFocus value={ranges} onChange={(e) => setRanges(e.target.value)} placeholder={t("For example 1-3, 4-6")} />
        </label>
      )}
      <p className="text-xs text-muted-foreground">{t("The new files are saved next to the original.")}</p>
    </Shell>
  );
}

/** The selected PDFs, in an order that can be changed, put into one new PDF next to the first */
export function CombinePdfsDialog({ nodes, onClose }: { nodes: Node[]; onClose(): void }) {
  const qc = useQueryClient();
  const [order, setOrder] = useState(nodes);
  const [name, setName] = useState(() => `${t("Combined")}.pdf`);
  const move = (i: number, dir: -1 | 1) =>
    setOrder((o) => {
      const next = [...o];
      [next[i], next[i + dir]] = [next[i + dir], next[i]];
      return next;
    });
  return (
    <Shell
      title={t("Combine PDFs")}
      description={t("{n} file|{n} files", { n: order.length })}
      onClose={onClose}
      confirm={t("Combine")}
      disabled={order.length < 2 || !name.trim()}
      onSubmit={() => {
        onClose();
        void runPdfJob(
          qc,
          order[0],
          () =>
            api.pdfCombine(
              order.map((n) => n.id),
              name.trim(),
            ),
          (job) => t('Created "{name}"', { name: job.name ?? "" }),
          t("Couldn't combine the PDFs"),
        );
      }}
    >
      <ol className="flex max-h-64 flex-col gap-1 overflow-y-auto rounded-lg border p-1" aria-label={t("Order of the files")}>
        {order.map((n, i) => (
          <li key={n.id} className="flex items-center gap-1 rounded px-2 py-1 text-sm">
            <span className="w-5 shrink-0 text-muted-foreground">{i + 1}</span>
            <span className="min-w-0 flex-1 truncate">{n.name}</span>
            <Button type="button" variant="ghost" size="icon-sm" disabled={i === 0} aria-label={t("Move {name} up", { name: n.name })} onClick={() => move(i, -1)}>
              <ArrowUpIcon />
            </Button>
            <Button type="button" variant="ghost" size="icon-sm" disabled={i === order.length - 1} aria-label={t("Move {name} down", { name: n.name })} onClick={() => move(i, 1)}>
              <ArrowDownIcon />
            </Button>
          </li>
        ))}
      </ol>
      <label className="flex flex-col gap-1.5 text-sm">
        {t("File name")}
        <Input value={name} onChange={(e) => setName(e.target.value)} maxLength={200} />
      </label>
    </Shell>
  );
}
