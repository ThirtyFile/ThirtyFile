/**
 * Changes that take a while run on the server as tasks (compressing, extracting, moving or copying to or from a folder
 * on the server, deleting for good, checking a folder, removing a user…): the request answers with the task, and a
 * message at the bottom shows its progress until it is done.
 */
import type { QueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { api, type Job } from "@/api";
import { t, tServer } from "@/lib/i18n";
import { FOLDER_CONTENTS, invalidateFiles } from "@/lib/queries";
import { reportShown } from "@/lib/errorReport";

/** How often a running task is asked for its progress */
const POLL_MS = 700;

/** Percent done, 0–100 (a task with nothing counted yet counts as just started) */
export function jobPercent(job: Pick<Job, "done" | "total">) {
  return job.total > 0 ? Math.min(100, Math.floor((job.done / job.total) * 100)) : 0;
}

function Progress({ percent }: { percent: number }) {
  return (
    <span className="mt-1.5 block h-1.5 w-full overflow-hidden rounded-full bg-muted" role="progressbar" aria-valuenow={percent} aria-valuemin={0} aria-valuemax={100}>
      <span className="block h-full rounded-full bg-primary transition-[width]" style={{ width: `${percent}%` }} />
    </span>
  );
}

/** What the progress message says while a task of this kind runs */
function runningText(kind: Job["kind"]) {
  switch (kind) {
    case "compress":
      return t("Compressing to ZIP…");
    case "extract":
      return t("Extracting…");
    case "move":
      return t("Moving…");
    case "copy":
      return t("Copying…");
    case "delete":
      return t("Deleting permanently…");
    case "empty_trash":
      return t("Emptying the trash…");
    case "scan":
      return t("Checking the folder for changes…");
    case "delete_user":
      return t("Deleting the user…");
    case "remove_personal":
      return t("Removing \"My files\"…");
    default:
      return t("Working…");
  }
}

/**
 * Follows a task the server started until it is done, showing its progress meanwhile (a task already done returns at
 * once). Resolves with the finished task; throws, with the server's reason, when it failed, so callers report both as
 * they would for any other request.
 */
export async function waitForJob(job: Job): Promise<Job> {
  if (job.state !== "running") return finished(job);
  const id = `job-${job.id}`;
  const title = runningText(job.kind);
  let failures = 0;
  try {
    while (job.state === "running") {
      const percent = jobPercent(job);
      toast.loading(`${title} ${percent}%`, { id, description: <Progress percent={percent} />, duration: Infinity });
      await new Promise((r) => setTimeout(r, POLL_MS));
      try {
        job = await api.job(job.id);
        failures = 0;
      } catch (e) {
        // A dropped connection: keep asking for a while before giving up on following it (the task goes on)
        if (++failures >= 10) throw e;
      }
    }
  } finally {
    toast.dismiss(id);
  }
  return finished(job);
}

function finished(job: Job): Job {
  if (job.state === "failed") throw new Error(tServer(job.error) || t("Operation failed"));
  return job;
}

/**
 * Follows a task after the dialog that started it has closed: a message says when it is done (`done`) or why it
 * failed, and `after` refreshes what it changed either way
 */
export async function followJob(job: Job, done: string, after: () => void) {
  try {
    await waitForJob(job);
    toast.success(done);
  } catch (e) {
    toast.error(e instanceof Error ? e.message : t("Operation failed"));
    reportShown(job.kind, e, job.id);
  } finally {
    after();
  }
}

/** Starts a compress or extract task and follows it to the end; the file list is refreshed when it is done */
export async function runJob(qc: QueryClient, start: () => Promise<Job>) {
  let kind: Job["kind"] | null = null;
  try {
    const started = await start();
    kind = started.kind;
    const job = await waitForJob(started);
    const name = job.name ?? "";
    toast.success(job.kind === "compress" ? t("Created \"{name}\"", { name }) : t("Extracted to \"{name}\"", { name }), { duration: 5000 });
    void invalidateFiles(qc, FOLDER_CONTENTS);
  } catch (e) {
    const message = e instanceof Error ? e.message : t("Operation failed");
    if (!kind) toast.error(message);
    else toast.error(kind === "compress" ? t("Couldn't compress to ZIP") : t("Couldn't extract"), { description: message, duration: 10000 });
    reportShown(kind ?? "job", e);
  }
}
