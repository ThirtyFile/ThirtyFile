import { useEffect, useMemo, useRef, useState } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { LanguageDescription, type LanguageSupport } from "@codemirror/language";
import { languages } from "@codemirror/language-data";
import { EditorView, keymap } from "@codemirror/view";
import { Loader2Icon, SaveIcon } from "lucide-react";
import { toast } from "sonner";
import { ApiError, api, type FileSource, type Node } from "@/api";
import { Button } from "@/components/ui/button";
import { getDraft, setDraft } from "@/lib/drafts";
import { t, tServer } from "@/lib/i18n";
import { useTheme } from "@/lib/theme";

function decode(buf: ArrayBuffer): { text: string; encoding: string } {
  try {
    return { text: new TextDecoder("utf-8", { fatal: true }).decode(buf), encoding: "UTF-8" };
  } catch {
    // Legacy file encodings common in Taiwan
    return { text: new TextDecoder("big5").decode(buf), encoding: "Big5" };
  }
}

export default function TextEditor(props: {
  node: Node;
  source: FileSource;
  editable: boolean;
  onSaved?(n: Node): void;
  onDirtyChange?(dirty: boolean): void;
  /** Embedded in a tab (fills the whole area); otherwise floating-window style */
  embedded?: boolean;
}) {
  const { dark } = useTheme();
  const [original, setOriginal] = useState<string | null>(null);
  const [encoding, setEncoding] = useState("UTF-8");
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [lang, setLang] = useState<LanguageSupport | null>(null);
  const [saving, setSaving] = useState(false);
  const dirty = original !== null && text !== original;
  const saveRef = useRef<() => void>(() => {});
  /** Version when the file was opened; sent on save, so if someone else changed the file meanwhile the save is rejected instead of overwriting each other */
  const base = useRef(props.node.updated_at);
  const [reload, setReload] = useState(0);
  /** File and reload count the current editor content belongs to */
  const loaded = useRef<{ id: string; reload: number } | null>(null);

  useEffect(() => {
    // After our own save the parent updates the node (updated_at changes): the content is already current, so don't re-download; keep undo history and cursor position
    if (loaded.current?.id === props.node.id && loaded.current.reload === reload && props.node.updated_at <= base.current) return;
    let cancelled = false;
    setOriginal(null);
    setError(null);
    fetch(props.source.contentUrl(props.node))
      .then(async (r) => {
        if (!r.ok) {
          const msg = await r.json().then((d) => d.error as string | undefined).catch(() => undefined);
          throw new Error(msg ? tServer(msg) : t("Couldn't read the file ({status})", { status: r.status }));
        }
        // The version of the content just received (the node the parent holds may be older, e.g. after a conflict)
        const version = Number(r.headers.get("x-version")) || props.node.updated_at;
        return r.arrayBuffer().then((buf) => ({ buf, version }));
      })
      .then(({ buf, version }) => {
        if (cancelled) return;
        base.current = version;
        loaded.current = { id: props.node.id, reload };
        const d = decode(buf);
        // Restore unsaved content when switching back to the tab
        const draft = props.editable ? getDraft(props.node.id) : undefined;
        if (draft && draft.base !== d.text) {
          // Someone else changed the file while these edits were unsaved: keep the edits and the version they were
          // based on, so saving is refused (409, with a reload option) instead of overwriting the other person's work
          if (draft.version !== undefined) base.current = draft.version;
          setOriginal(draft.base);
          setText(draft.text);
          toast.warning(t("This file was changed by someone else since you opened it. Your unsaved changes are kept; reload to see the new version."), { duration: 10000 });
        } else {
          setOriginal(d.text);
          setText(draft ? draft.text : d.text);
        }
        setEncoding(d.encoding);
      })
      .catch((e) => !cancelled && setError(e.message));
    const desc = LanguageDescription.matchFilename(languages, props.node.name);
    desc?.load().then((l) => !cancelled && setLang(l));
    return () => {
      cancelled = true;
    };
  }, [props.node.id, props.node.updated_at, props.source, reload]);

  useEffect(() => props.onDirtyChange?.(dirty), [dirty]);

  const change = (value: string) => {
    setText(value);
    if (original !== null && props.editable) setDraft(props.node.id, value === original ? null : { text: value, base: original, version: base.current });
  };

  saveRef.current = async () => {
    if (!props.editable || !dirty || saving) return;
    setSaving(true);
    try {
      const n = await api.saveContent(props.node.id, text, base.current);
      base.current = n.updated_at;
      setDraft(props.node.id, null);
      setOriginal(text);
      setEncoding("UTF-8");
      toast.success(t("Saved"));
      props.onSaved?.(n);
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        toast.error(e.message, {
          duration: 10000,
          action: {
            label: t("Reload (discard changes)"),
            onClick: () => {
              setDraft(props.node.id, null);
              setReload((x) => x + 1);
            },
          },
        });
      } else toast.error(e instanceof Error ? e.message : t("Couldn't save"));
    } finally {
      setSaving(false);
    }
  };

  const extensions = useMemo(
    () => [
      EditorView.lineWrapping,
      keymap.of([{ key: "Mod-s", preventDefault: true, run: () => (saveRef.current(), true) }]),
      ...(lang ? [lang] : []),
    ],
    [lang],
  );

  const muted = props.embedded ? "text-muted-foreground" : "text-white/70";
  if (error) return <div className={`flex h-full items-center justify-center text-sm ${muted}`}>{error}</div>;
  if (original === null)
    return (
      <div className={`flex h-full items-center justify-center ${muted}`}>
        <Loader2Icon className="size-6 animate-spin" />
      </div>
    );

  return (
    <div
      className={
        props.embedded
          ? "flex h-full w-full flex-col overflow-hidden bg-background text-foreground"
          : "flex h-full w-full max-w-5xl flex-col overflow-hidden rounded-xl bg-background text-foreground shadow-2xl"
      }
    >
      <div className="flex h-10 items-center gap-2 border-b px-3 text-xs text-muted-foreground">
        <span>{encoding}</span>
        {encoding !== "UTF-8" && props.editable && <span>· {t("Will be converted to UTF-8 when saved")}</span>}
        {!props.editable && <span>· {t("Read-only")}</span>}
        <span className="ml-auto">{dirty ? t("Unsaved changes") : ""}</span>
        {props.editable && (
          <Button size="sm" disabled={!dirty || saving} onClick={() => saveRef.current()}>
            {saving ? <Loader2Icon className="animate-spin" /> : <SaveIcon />}
            {t("Save")}
            <kbd className="ml-1 text-[10px] opacity-60">Ctrl+S</kbd>
          </Button>
        )}
      </div>
      <div className="min-h-0 flex-1 overflow-auto">
        <CodeMirror
          value={text}
          onChange={change}
          editable={props.editable}
          readOnly={!props.editable}
          theme={dark ? "dark" : "light"}
          extensions={extensions}
          height="100%"
          style={{ height: "100%" }}
          autoFocus
        />
      </div>
    </div>
  );
}
