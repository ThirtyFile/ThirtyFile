/** Compress and extract tasks run on the server: a message at the bottom shows their progress until they finish */
import type { QueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { api, type Job } from "@/api";
import { t, tServer } from "@/lib/i18n";
import { FOLDER_CONTENTS, invalidateFiles } from "@/lib/queries";

/** How often a running task is asked for its progress */
const POLL_MS = 700;

/** Percent done, 0–100 (a task with nothing to read yet counts as just started) */
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

/** Starts a task and follows it to the end; the file list is refreshed when it is done */
export async function runJob(qc: QueryClient, start: () => Promise<Job>) {
  let job: Job;
  try {
    job = await start();
  } catch (e) {
    toast.error(e instanceof Error ? e.message : t("Operation failed"));
    return;
  }
  const title = job.kind === "compress" ? t("Compressing to ZIP…") : t("Extracting…");
  const id = `job-${job.id}`;
  let failures = 0;
  while (job.state === "running") {
    const percent = jobPercent(job);
    toast.loading(`${title} ${percent}%`, { id, description: <Progress percent={percent} />, duration: Infinity });
    await new Promise((r) => setTimeout(r, POLL_MS));
    try {
      job = await api.job(job.id);
      failures = 0;
    } catch (e) {
      // A dropped connection: keep asking for a while before giving up on following it
      if (++failures >= 10) {
        toast.error(e instanceof Error ? e.message : t("Operation failed"), { id, description: undefined, duration: 8000 });
        return;
      }
    }
  }
  if (job.state === "done") {
    const name = job.name ?? "";
    toast.success(job.kind === "compress" ? t("Created \"{name}\"", { name }) : t("Extracted to \"{name}\"", { name }), { id, description: undefined, duration: 5000 });
    void invalidateFiles(qc, FOLDER_CONTENTS);
  } else {
    toast.error(job.kind === "compress" ? t("Couldn't compress to ZIP") : t("Couldn't extract"), {
      id,
      description: tServer(job.error),
      duration: 10000,
    });
  }
}
