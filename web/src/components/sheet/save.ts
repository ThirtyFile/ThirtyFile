/**
 * Saving: write the session's changes back into the original .xlsx and upload it.
 *
 * A snapshot of the data (cells, styles, insert/delete log, baseline) is frozen the moment saving starts:
 * - If the user keeps editing while the file is generated, those edits don't leak into this save
 * - On completion only the "version at start" is marked saved; edits made meanwhile stay unsaved and are written next time
 * - The file is generated on a copy of the zip (sharing the unchanged entries, still compressed); if the upload fails (e.g. version conflict) the original zip is unaffected and inserts/deletes aren't replayed twice
 */
import JSZip from "jszip";
import { api, type Node } from "@/api";
import { Calculator, isErr, toScalar } from "@/lib/sheet/formula";
import { colOf, rowOf, type Workbook } from "@/lib/sheet/model";
import { applyStructOp, cloneState } from "@/lib/sheet/ops";
import { buildXlsx, type Snapshot } from "@/lib/sheet/xlsx";
import type { Session } from "./session";

export async function saveSession(session: Session, nodeId: string): Promise<{ node: Node; cells: number }> {
  const version = session.version;
  const opsCount = session.ops.length;
  const book: Workbook = {
    sheets: session.book.sheets.map((s) => ({ ...s, ...cloneState(s), tables: s.tables.map((t) => ({ ...t })) })),
    styles: [...session.book.styles],
    xfCount: session.book.xfCount,
    styleBase: new Map(session.book.styleBase),
  };
  const snapshot: Snapshot = new Map([...session.snapshot].map(([id, st]) => [id, cloneState(st)]));
  const ops = session.ops.slice(0, opsCount);
  const calc = new Calculator(book);
  const valueOf = (si: number, r: number, c: number) => {
    const v = calc.value(si, r, c);
    // Keep Excel's original result for anything we can't compute (unsupported functions etc.)
    return isErr(v) && (v.code === "#NAME?" || v.code === "#CYCLE!") ? undefined : toScalar(v);
  };

  const work = cloneZip(session.zip);
  const { blob, cells } = await buildXlsx(work, book, snapshot, ops, valueOf);
  const node = await api.saveContent(nodeId, blob, session.base);

  // Upload succeeded: what was saved becomes the new baseline (formula cached values become the results computed this time)
  const next: Snapshot = new Map();
  book.sheets.forEach((s, si) => {
    const st = cloneState(s);
    for (const [k, cell] of st.cells) if (cell.f) st.cells.set(k, { ...cell, v: valueOf(si, rowOf(k), colOf(k)) ?? cell.v });
    next.set(s.id, st);
  });
  // Inserts/deletes made during the save: apply the same adjustment to the baseline so the next save compares correctly
  const pending = session.ops.slice(opsCount);
  for (const op of pending) applyStructOp([...next.values()], op, false);
  session.snapshot = next;
  session.ops = pending;
  session.zip = work;
  // Styles written to the file become original-file styles; styles added during the save are written next time
  session.book.xfCount = book.styles.length;
  for (const i of [...session.book.styleBase.keys()]) if (i < book.styles.length) session.book.styleBase.delete(i);
  session.base = node.updated_at;
  session.saved = version;
  return { node, cells };
}

/**
 * A copy of the archive to change without touching the original. Entries are never changed in place (writing a file
 * replaces its entry, removing one drops it from the list), so the copy can share them: nothing is unzipped or compressed,
 * and entries left unchanged are written into the saved file as they are.
 */
function cloneZip(zip: JSZip): JSZip {
  const copy = new JSZip();
  Object.assign(copy.files, zip.files);
  return copy;
}
