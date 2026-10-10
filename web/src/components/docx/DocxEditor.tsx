/**
 * Editing the text of a Word document: the editing view runs in the sandboxed preview frame (ooxml/docx/editor.ts), so
 * the document's content can't reach this site's sign-in state or API; this page loads the file, saves it, and keeps
 * unsaved edits when switching tabs (session.ts).
 */
import { useEffect, useEffectEvent, useRef, useState } from "react";
import { Loader2Icon, SaveIcon, XIcon } from "lucide-react";
import { toast } from "sonner";
import { ApiError, type FileSource, type Node } from "@/api";
import { Button } from "@/components/ui/button";
import { ConfirmDialog } from "@/components/dialogs";
import { frameDocument, loadFrameScript } from "@/components/officeFrame";
import { getDraft, setDraft } from "@/lib/drafts";
import { t } from "@/lib/i18n";
import { shortcut } from "@/lib/keys";
import { cancellable } from "@/lib/cancellable";
import { officeErrorMessage } from "@/lib/officeErrors";
import type { Caret } from "@/ooxml/docx/editor";
import { TOO_LARGE } from "@/ooxml/core/package";
import { dropSession, isDirty, keepSession, openSession, releaseIfClean, reusableSession, saveSession, type DocxSession } from "./session";

/** The frame's errors are the renderer's, in English: said in words here */
const openError = (message: string | undefined) =>
  message === TOO_LARGE ? t("The file's content is too large to preview. Download it and open it in Office.") : t("Couldn't open this document");

/** Time limits for the frame to start, and for the document to open in it */
const READY_TIMEOUT = 10_000;
const OPEN_TIMEOUT = 60_000;

export default function DocxEditor(props: { node: Node; source: FileSource; onSaved?(n: Node): void; onExit(): void }) {
  const { node } = props;
  const [session, setSession] = useState<DocxSession | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reload, setReload] = useState(0);

  useEffect(() => {
    const existing = reusableSession(node);
    if (existing) {
      setSession(existing);
      return;
    }
    setSession(null);
    setError(null);
    // Closing the editor (or moving to another file) while the document downloads stops the download
    return cancellable(
      (signal) => openSession(node, props.source, signal),
      (s) => {
        keepSession(node.id, s);
        setSession(s);
      },
      (e) => setError(officeErrorMessage(e, t("Couldn't open this document"))),
    );
    // Load only when the file changes or a reload is requested; updated_at changing after a save doesn't require reloading
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- the node object is new after every save
  }, [node.id, reload]);

  // The file got other content while it is open (an earlier version restored, say), and there are no edits here to
  // keep: what it has now is loaded, so the next save doesn't take it for someone else's
  useEffect(() => {
    if (!session || session.base === node.updated_at) return;
    if (isDirty(session) && getDraft(node.id, "docx")) return;
    dropSession(node.id);
    setReload((x) => x + 1);
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- when the file's version changes
  }, [node.updated_at]);

  if (error)
    return (
      <div className="flex size-full flex-col items-center justify-center gap-3 p-6 text-center text-sm text-destructive">
        {error}
        <Button variant="outline" size="sm" onClick={props.onExit}>
          {t("Back to preview")}
        </Button>
      </div>
    );
  if (!session)
    return (
      <div className="flex size-full items-center justify-center text-muted-foreground">
        <Loader2Icon className="size-6 animate-spin" />
      </div>
    );
  return (
    <Workspace
      key={`${node.id}-${reload}`}
      node={node}
      session={session}
      onSaved={props.onSaved}
      onExit={props.onExit}
      onReload={() => {
        dropSession(node.id);
        setDraft(node.id, null);
        setReload((x) => x + 1);
      }}
    />
  );
}

/** What the frame answers when asked for the edited document */
interface Collected {
  path: string;
  xml: string | null;
  caret: Caret | null;
  scroll: number;
}

function Workspace({ node, session, onSaved, onExit, onReload }: { node: Node; session: DocxSession; onSaved?(n: Node): void; onExit(): void; onReload(): void }) {
  const frame = useRef<HTMLIFrameElement>(null);
  const [srcDoc, setSrcDoc] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [dirty, setDirtyState] = useState(isDirty(session));
  const [saving, setSaving] = useState(false);
  /** Saving in progress (live value: Ctrl+S may be pressed again before the page updates) */
  const savingRef = useRef(false);
  const [confirmExit, setConfirmExit] = useState(false);
  /** document.xml the frame opened (null: the file's): what it holds while the frame reports no change */
  const opened = useRef(session.current);
  const waiting = useRef(new Map<number, { resolve(c: Collected): void; reject(e: Error): void }>());
  const nextId = useRef(0);

  /** The tab is flagged, and closing the browser warns, while there are unsaved edits */
  const showDirty = (d: boolean) => {
    setDirtyState(d);
    setDraft(node.id, d ? { kind: "docx", base: session.base } : null);
  };

  /** What the frame holds becomes the session's */
  const absorb = (c: Collected) => {
    session.path = c.path;
    session.current = c.xml ?? opened.current;
    session.caret = c.caret;
    session.scroll = c.scroll;
  };

  const collect = () =>
    new Promise<Collected>((resolve, reject) => {
      const win = frame.current?.contentWindow;
      if (!win) {
        reject(new Error(t("Couldn't open this document")));
        return;
      }
      const id = ++nextId.current;
      waiting.current.set(id, { resolve, reject });
      win.postMessage({ type: "collect", id }, "*");
    });

  const save = async () => {
    if (savingRef.current || loading || error) return;
    savingRef.current = true;
    setSaving(true);
    try {
      const c = await collect();
      absorb(c);
      if (!isDirty(session)) return;
      const n = await saveSession(session, node.id, c.path, session.current);
      toast.success(t("Saved"));
      onSaved?.(n);
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        toast.error(e.message, { duration: 10000, action: { label: t("Reload (discard changes)"), onClick: onReload } });
      } else toast.error(officeErrorMessage(e, t("Couldn't save")));
    } finally {
      savingRef.current = false;
      setSaving(false);
      // Edits typed while the save was on its way stay unsaved
      showDirty(isDirty(session));
    }
  };

  const exit = () => {
    if (isDirty(session) || dirty) setConfirmExit(true);
    else {
      dropSession(node.id);
      setDraft(node.id, null);
      onExit();
    }
  };

  // Messages from the frame see the current save and exit functions without subscribing again
  const onMessage = useEffectEvent((e: MessageEvent) => {
    if (e.source !== frame.current?.contentWindow) return;
    const msg = e.data as { type?: string; id?: number; message?: string } & Partial<Collected>;
    switch (msg.type) {
      case "input":
        showDirty(true);
        break;
      case "changed":
        absorb(msg as Collected);
        showDirty(isDirty(session));
        break;
      case "save":
        void save();
        break;
      case "collected":
      case "collect-error": {
        const w = waiting.current.get(msg.id ?? -1);
        if (!w) break;
        waiting.current.delete(msg.id!);
        if (msg.type === "collected") w.resolve(msg as Collected);
        else w.reject(new Error(t("Couldn't save")));
        break;
      }
    }
  });

  useEffect(() => {
    loadFrameScript().then(
      (js) => setSrcDoc(frameDocument(js)),
      (e) => setError(officeErrorMessage(e, t("Couldn't load the previewer"))),
    );
  }, []);

  useEffect(() => {
    if (!srcDoc) return;
    const pending = waiting.current;
    let ready = false;
    let cancelled = false;
    let openTimer = 0;
    const readyTimer = window.setTimeout(() => {
      if (cancelled || ready) return;
      setError(t("Couldn't show the preview. Reload the page."));
      setLoading(false);
    }, READY_TIMEOUT);
    const listen = (e: MessageEvent) => {
      if (e.source !== frame.current?.contentWindow || cancelled) return;
      const msg = e.data as { type?: string; message?: string };
      if (msg.type === "ready") {
        ready = true;
        window.clearTimeout(readyTimer);
        const buffer = session.buffer.slice(0);
        const texts = {
          label: t("Document text"),
          locked: t("This part can't be changed here. It's kept as it is."),
          table: t("The text in this table's cells can be changed. Its rows and columns are kept as they are."),
        };
        // "*": the sandboxed frame has an opaque origin, which no target origin can name; only that frame receives it
        frame.current?.contentWindow?.postMessage({ type: "edit", kind: "docx", buffer, xml: session.current, texts, caret: session.caret, scroll: session.scroll }, "*", [buffer]);
        openTimer = window.setTimeout(() => {
          if (cancelled) return;
          setError(t("Laying out this document took too long, so the preview was stopped. Download it and open it in Office."));
          setLoading(false);
        }, OPEN_TIMEOUT);
      } else if (msg.type === "done") {
        window.clearTimeout(openTimer);
        setLoading(false);
      } else if (msg.type === "error") {
        window.clearTimeout(openTimer);
        setError(openError(msg.message));
        setLoading(false);
      } else onMessage(e);
    };
    window.addEventListener("message", listen);
    return () => {
      cancelled = true;
      window.clearTimeout(readyTimer);
      window.clearTimeout(openTimer);
      window.removeEventListener("message", listen);
      for (const w of pending.values()) w.reject(new Error(t("Couldn't save")));
      pending.clear();
    };
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- the frame is set up once per session
  }, [srcDoc, session]);

  // Ctrl+S on this page (the frame handles its own)
  const onKey = useEffectEvent((e: KeyboardEvent) => {
    if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === "s") {
      e.preventDefault();
      void save();
    }
  });
  useEffect(() => {
    const listen = (e: KeyboardEvent) => onKey(e);
    window.addEventListener("keydown", listen);
    return () => window.removeEventListener("keydown", listen);
  }, []);

  // On unmount, release the session if it has no unsaved edits (keep it otherwise, to continue when switching back)
  useEffect(() => () => releaseIfClean(node.id, session), [node.id, session]);

  return (
    <div className="flex size-full flex-col bg-background text-foreground">
      <div className="flex min-h-11 shrink-0 flex-wrap items-center gap-2 border-b px-2 py-1">
        <p className="min-w-0 flex-1 text-xs text-muted-foreground">{t("Only the text can be changed here, in tables too. Pictures and other parts are kept as they are.")}</p>
        {dirty && <span className="shrink-0 text-xs text-amber-600 dark:text-amber-400">{t("Unsaved changes")}</span>}
        <div className="flex shrink-0 items-center gap-1">
          <Button size="sm" disabled={!dirty || saving || loading || !!error} onClick={() => void save()}>
            {saving ? <Loader2Icon className="animate-spin" /> : <SaveIcon />}
            {t("Save")}
            <kbd className="ml-1 text-[10px] opacity-60 max-md:hidden">{shortcut("Ctrl+S")}</kbd>
          </Button>
          <Button variant="outline" size="sm" onClick={exit}>
            <XIcon /> {t("Done editing")}
          </Button>
        </div>
      </div>
      <div className="relative min-h-0 flex-1 bg-neutral-200 dark:bg-neutral-800">
        {error ? (
          <div className="flex size-full items-center justify-center p-6">
            <p className="max-w-md rounded-xl border bg-background px-6 py-4 text-center text-sm text-destructive">{error}</p>
          </div>
        ) : (
          srcDoc && (
            <iframe
              ref={frame}
              srcDoc={srcDoc}
              title={node.name}
              // Only allow scripts: no allow-same-origin (a separate opaque origin), can't open new windows or navigate this page
              sandbox="allow-scripts"
              className="size-full border-0"
            />
          )
        )}
        {loading && !error && (
          <div className="absolute inset-0 flex items-center justify-center text-muted-foreground">
            <Loader2Icon className="size-6 animate-spin" />
          </div>
        )}
      </div>
      {confirmExit && (
        <ConfirmDialog
          title={t("Discard unsaved changes?")}
          description={t("This document has unsaved changes. They'll be lost if you stop editing.")}
          confirmText={t("Discard and exit")}
          destructive
          onClose={() => setConfirmExit(false)}
          onConfirm={async () => {
            dropSession(node.id);
            setDraft(node.id, null);
            onExit();
          }}
        />
      )}
    </div>
  );
}
