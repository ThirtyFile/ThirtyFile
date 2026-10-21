import { useEffect, useEffectEvent, useRef, useState } from "react";
import { Loader2Icon } from "lucide-react";
import type { FileSource, Node } from "@/api";
import { Button } from "@/components/ui/button";
import { confirm } from "@/lib/confirm";
import { t } from "@/lib/i18n";
import { errorMessage } from "@/lib/utils";
import { reportShown } from "@/lib/errorReport";
import { mountOffice, unmountOffice, releaseOffice, type OfficeEntry } from "./sessions";

export default function OfficeWorkspace(props: {
  node: Node;
  source: FileSource;
  editable?: boolean;
  allowDownload?: boolean;
  onExit?(): void;
  onSaved?(node: Node): void;
  onDirtyChange?(dirty: boolean): void;
}) {
  const { node, source, editable = false, allowDownload = true } = props;
  const host = useRef<HTMLDivElement>(null);
  const current = useRef<OfficeEntry | undefined>(undefined);
  const [, redraw] = useState(0);
  const [failure, setFailure] = useState<unknown>(null);
  const notify = useEffectEvent(() => {
    props.onDirtyChange?.(!!current.current?.state?.dirty);
    redraw((value) => value + 1);
  });
  const saved = useEffectEvent((saved: Node) => props.onSaved?.(saved));
  useEffect(() => {
    setFailure(null);
    const entry = mountOffice(host.current!, node, source, editable, allowDownload);
    current.current = entry;
    const changed = () => notify();
    const committed = (node: Node) => saved(node);
    entry.listeners.add(changed);
    entry.savedListeners.add(committed);
    notify();
    return () => {
      entry.listeners.delete(changed);
      entry.savedListeners.delete(committed);
      unmountOffice(entry);
      current.current = undefined;
    };
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- an app refresh changes the node object; only identity, policy or source replace the live Worker
  }, [node.id, source, editable, allowDownload]);
  useEffect(() => {
    const entry = current.current;
    if (!entry || entry.disposed) return;
    // A version restored in ThirtyFile reloads a clean document; unsaved edits stay intact for conflict resolution.
    void entry.ready.then(async () => {
      if (entry.disposed || entry.storage.version === node.updated_at || entry.state?.dirty || entry.state?.saving) return;
      entry.storage.node = node;
      try {
        await entry.session?.open({ id: node.id });
      } catch (error) {
        setFailure(error);
      }
    });
  }, [node]);
  const entry = current.current;
  const state = entry?.state;
  const error = failure ?? entry?.error ?? state?.error;
  const run = async (operation: () => Promise<unknown>) => {
    setFailure(null);
    try {
      await operation();
    } catch (error) {
      setFailure(error);
      reportShown("editor", error, node.id);
    }
  };
  const reload = async () => {
    if (
      state?.dirty &&
      !(await confirm({
        title: t("Discard unsaved changes?"),
        description: t("Reloading replaces your unsaved edits with the file on the server."),
        confirmText: t("Reload latest version"),
        destructive: true,
      }))
    )
      return;
    await run(async () => {
      entry!.storage.node = node;
      await entry!.session!.open({ id: node.id });
    });
  };
  const exit = async () => {
    if (state?.dirty && !(await confirm({ title: t("Discard unsaved changes?"), confirmText: t("Discard and exit"), destructive: true }))) return;
    if (entry) releaseOffice(entry);
    props.onExit?.();
  };
  return (
    <section
      className="relative flex size-full min-h-0 min-w-0 flex-col overflow-hidden bg-background"
      data-office-document={node.id}
      data-office-ready={!!entry?.session && !!state && !state.loading && (!editable || state.mode === "edit")}
    >
      {editable && (
        <div className="flex shrink-0 flex-wrap items-center gap-2 border-b px-3 py-1.5 text-xs">
          <Button
            size="sm"
            variant="outline"
            disabled={!state || state.loading || state.saving || !state.revision || state.error?.code === "SAVE_OUTCOME_UNKNOWN"}
            onClick={() => void run(() => entry!.session!.save())}
          >
            {t("Save")}
          </Button>
          {state?.error?.code === "SAVE_OUTCOME_UNKNOWN" && (
            <Button size="sm" variant="outline" onClick={() => void run(() => entry!.session!.retrySave())}>
              {t("Retry save")}
            </Button>
          )}
          <Button size="sm" variant="ghost" disabled={!state || state.loading || state.saving} onClick={() => void reload()}>
            {t("Reload latest version")}
          </Button>
          <span role="status">
            {!state ? t("Loading…") : state.saving ? t("Saving…") : !state.revision ? t("Local document: download to keep this file") : state.dirty ? t("Unsaved changes") : t("Saved")}
          </span>
          <span className="flex-1" />
          {props.onExit && (
            <Button size="sm" variant="ghost" disabled={state?.saving} onClick={() => void exit()}>
              {error && !state ? t("Back to preview") : t("Done editing")}
            </Button>
          )}
        </div>
      )}
      {error ? (
        <div className={state ? "flex shrink-0 justify-center p-3" : "flex min-h-0 flex-1 items-center justify-center p-6"}>
          <div role="alert" className="max-w-md rounded-xl border bg-background px-6 py-4 text-center text-sm text-destructive">
            {errorMessage(error, t("Couldn't open this file"))}
          </div>
        </div>
      ) : null}
      <div ref={host} className={error && !state ? "hidden" : "relative min-h-0 min-w-0 flex-1 overflow-hidden"} />
      {!state && !error && (
        <div className="pointer-events-none absolute inset-0 flex items-center justify-center">
          <Loader2Icon className="size-6 animate-spin text-muted-foreground" />
        </div>
      )}
    </section>
  );
}
