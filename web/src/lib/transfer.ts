/** Moving and copying what is selected into a folder: from the list's commands, the clipboard and dragging */
import type { QueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { api } from "@/api";
import { askBeforeTransfer } from "@/lib/conflicts";
import { reportShown } from "@/lib/errorReport";
import { t } from "@/lib/i18n";
import { waitForJob } from "@/lib/jobs";
import { refreshFiles } from "@/lib/queries";
import { eachBatch, type Picked } from "@/lib/span";
import { type Origins, moveBack, movedBack, originsOf, toastWithUndo } from "@/lib/undo";
import { errorMessage } from "@/lib/utils";

const CANCELLED = new Error("cancelled");

/**
 * Moves or copies the selection into `dest`, a batch at a time (a span of a large folder is asked of the server as it
 * goes), asking first about names the folder already has. `done` words the message for the number of items that went;
 * a move of items picked one by one can be undone from it (`origins`, or where `items` says they are). False when it
 * was cancelled or failed (the reason is shown); what went before that shows too.
 */
export async function transferItems(
  qc: QueryClient,
  mode: "move" | "copy",
  picked: Picked,
  dest: string,
  o: { done(n: number): string; fallback: string; origins?: Origins; items?: readonly { id: string; parent_id: string | null }[] },
): Promise<boolean> {
  const sent: string[] = [];
  let count = 0;
  const span = picked.span;
  // The folders the items came from (those known), and the span's
  const from = [span?.folder, ...(o.items ?? []).filter((n) => picked.ids.includes(n.id)).map((n) => n.parent_id)];
  try {
    await eachBatch(picked, mode === "move" ? t("Moving…") : t("Copying…"), async (batch) => {
      const ids = batch.filter((id) => id !== dest);
      if (!ids.length) return;
      const resolutions = await askBeforeTransfer(mode, ids, dest);
      if (!resolutions) throw CANCELLED;
      const go = ids.filter((id) => resolutions[id] !== "skip");
      if (!go.length) return;
      await waitForJob(await (mode === "move" ? api.move(go, dest, resolutions) : api.copy(go, dest, resolutions)));
      count += go.length;
      // The ids of a span aren't kept: the lists it was in load again instead
      if (!span) sent.push(...go);
    });
  } catch (e) {
    if (e !== CANCELLED) {
      toast.error(errorMessage(e, o.fallback));
      reportShown(mode, e, dest);
      void refreshFiles(qc, { folders: [dest, ...from], contents: true, usage: true });
      return false;
    }
    if (!count) return false;
  }
  if (count) {
    const origins =
      mode === "move" && !span ? new Map([...(o.origins ?? originsOf(o.items ?? [], sent, dest))].filter(([id, parent]) => sent.includes(id) && parent !== dest)) : new Map<string, string>();
    if (origins.size) toastWithUndo(o.done(count), { undo: () => moveBack(origins), undoneText: t("Moved back"), after: () => refreshFiles(qc, movedBack(origins)) });
    else toast.success(o.done(count));
  }
  void refreshFiles(
    qc,
    span
      ? { folders: [dest, span.folder], contents: true, usage: true }
      : mode === "move"
        ? { moved: [{ ids: sent, to: dest }], usage: true }
        : { folders: [dest], contents: true, usage: true },
  );
  return true;
}
