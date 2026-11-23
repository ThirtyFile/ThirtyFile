/** Page operations on PDFs run on the server as tasks; this starts one, follows it, and tells how it ended */
import type { QueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import type { Job, Node } from "@/api";
import { waitForJob } from "@/lib/jobs";
import { refreshFiles } from "@/lib/queries";
import { reportShown } from "@/lib/errorReport";
import { t } from "@/lib/i18n";
import { errorMessage } from "@/lib/utils";

/** The name of a file without its extension */
export function stemOf(name: string) {
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(0, dot) : name;
}

/**
 * Starts a page operation and follows it. `done` words the message for the finished task; the folder the new files
 * went into (and the file itself, when it got a new version) is refreshed either way
 */
export async function runPdfJob(qc: QueryClient, node: Pick<Node, "id" | "parent_id">, start: () => Promise<Job>, done: (job: Job) => string, failed: string) {
  const change = { folders: [node.parent_id], nodes: [node.id], contents: true, usage: true };
  try {
    const job = await waitForJob(await start());
    toast.success(done(job), { duration: 5000 });
  } catch (e) {
    toast.error(failed, { description: errorMessage(e, t("Operation failed")), duration: 10000 });
    reportShown("pdf_pages", e);
  } finally {
    void refreshFiles(qc, change);
  }
}
