import { useEffect, useEffectEvent, useMemo, useRef, useState, type ReactNode } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { LanguageDescription, type LanguageSupport } from "@codemirror/language";
import { languages } from "@codemirror/language-data";
import { EditorView, keymap } from "@codemirror/view";
import { Loader2Icon, SaveIcon } from "lucide-react";
import { toast } from "sonner";
import { ApiError, api, fetchOk, type FileSource, type Node } from "@/api";
import { Button } from "@/components/ui/button";
import { getDraft, setDraft } from "@/lib/drafts";
import { t } from "@/lib/i18n";
import { decodeText, encodeText, lineEnding, normalizeLines, type TextEncodingName } from "@/lib/textEncoding";
import { useTheme } from "@/lib/theme";

export default function TextEditor(props: {
  node: Node;
  source: FileSource;
  editable: boolean;
  onSaved?(n: Node): void;
  onDirtyChange?(dirty: boolean): void;
  /** Embedded in a tab (fills the whole area); otherwise floating-window style */
  embedded?: boolean;
  /** Shown first in the bar above the text (the Markdown view's Preview / Edit switch) */
  toolbar?: ReactNode;
}) {
  const { dark } = useTheme();
  const [original, setOriginal] = useState<string | null>(null);
  /** null: the encoding wasn't recognised, so the file is read-only to avoid damaging it */
  const [encoding, setEncoding] = useState<TextEncodingName | null>("UTF-8");
  /** Line ending of the file, restored when saving */
  const eol = useRef<"\r\n" | "\n">("\n");
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [lang, setLang] = useState<LanguageSupport | null>(null);
  const [saving, setSaving] = useState(false);
  const editable = props.editable && encoding !== null;
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
    // The editor stays mounted when moving to another file: forget the previous file's language
    setLang(null);
    fetchOk(props.source.contentUrl(props.node))
      .then((r) => {
        // The version of the content just received (the node the parent holds may be older, e.g. after a conflict)
        const version = Number(r.headers.get("x-version")) || props.node.updated_at;
        return r.arrayBuffer().then((buf) => ({ buf, version }));
      })
      .then(({ buf, version }) => {
        if (cancelled) return;
        base.current = version;
        loaded.current = { id: props.node.id, reload };
        const decoded = decodeText(buf);
        eol.current = lineEnding(decoded.text);
        const d = { ...decoded, text: normalizeLines(decoded.text) };
        // Restore unsaved content when switching back to the tab
        const draft = props.editable && d.encoding !== null ? getDraft(props.node.id, "text") : undefined;
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
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- the node object is new after every refresh of the list: its id and date say when the file changed
  }, [props.node.id, props.node.updated_at, props.source, reload]);

  // Reported when it changes; the parent's callback may be new on every render
  const reportDirty = useEffectEvent((d: boolean) => props.onDirtyChange?.(d));
  useEffect(() => reportDirty(dirty), [dirty]);

  const change = (value: string) => {
    setText(value);
    if (original !== null && editable) setDraft(props.node.id, value === original ? null : { kind: "text", text: value, base: original, version: base.current });
  };

  saveRef.current = async () => {
    if (!editable || !dirty || saving || encoding === null) return;
    setSaving(true);
    try {
      const out = encodeText(eol.current === "\n" ? text : text.replace(/\n/g, eol.current), encoding);
      const n = await api.saveContent(props.node.id, out.body, base.current);
      base.current = n.updated_at;
      setDraft(props.node.id, null);
      setOriginal(text);
      setEncoding(out.encoding);
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
        {props.toolbar}
        {encoding === null ? (
          <span>{t("Unknown encoding: opened read-only so the file isn't damaged")}</span>
        ) : (
          <span>{encoding}</span>
        )}
        {encoding === "Big5" && editable && <span>· {t("Will be converted to UTF-8 when saved")}</span>}
        {!props.editable && <span>· {t("Read-only")}</span>}
        <span className="ml-auto">{dirty ? t("Unsaved changes") : ""}</span>
        {editable && (
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
          editable={editable}
          readOnly={!editable}
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
