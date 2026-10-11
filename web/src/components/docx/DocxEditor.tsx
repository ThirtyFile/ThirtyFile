/**
 * Editing the text of a Word document: the editing view runs in the sandboxed preview frame (ooxml/docx/editor.ts), so
 * the document's content can't reach this site's sign-in state or API; this page loads the file, saves it, and keeps
 * unsaved edits when switching tabs (session.ts).
 */
import { useEffect, useEffectEvent, useRef, useState } from "react";
import {
  AlignCenterIcon,
  AlignJustifyIcon,
  AlignLeftIcon,
  AlignRightIcon,
  BaselineIcon,
  BetweenHorizontalEndIcon,
  BetweenHorizontalStartIcon,
  BetweenVerticalEndIcon,
  BetweenVerticalStartIcon,
  BoldIcon,
  ChevronDownIcon,
  Columns3Icon,
  HighlighterIcon,
  IndentDecreaseIcon,
  IndentIncreaseIcon,
  ItalicIcon,
  ListChevronsUpDownIcon,
  ListIcon,
  ListOrderedIcon,
  Grid3x3Icon,
  Loader2Icon,
  PaintBucketIcon,
  PanelBottomIcon,
  PanelsTopBottomIcon,
  Rows3Icon,
  SaveIcon,
  SquareDashedIcon,
  SquareIcon,
  StrikethroughIcon,
  TableCellsMergeIcon,
  TableCellsSplitIcon,
  UnderlineIcon,
  XIcon,
} from "lucide-react";
import { NativeSelect } from "@/components/ui/native-select";
import { toast } from "sonner";
import { ApiError, type FileSource, type Node } from "@/api";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { ColorMenu } from "@/components/ColorMenu";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ConfirmDialog } from "@/components/dialogs";
import { frameDocument, loadFrameScript } from "@/components/officeFrame";
import { getDraft, setDraft } from "@/lib/drafts";
import { t } from "@/lib/i18n";
import { shortcut } from "@/lib/keys";
import { cancellable } from "@/lib/cancellable";
import { officeErrorMessage } from "@/lib/officeErrors";
import { FONT_NAME, type Caret, type CaretFormat, type FontChoice, type ParagraphStyle } from "@/ooxml/docx/editor";
import { HIGHLIGHTS } from "@/ooxml/docx/formatOps";
import type { Story, StoryKind } from "@/ooxml/docx/hfOps";
import { MAX_SPLIT, type BorderKind, type TableOp } from "@/ooxml/docx/tableOps";
import { TOO_LARGE } from "@/ooxml/core/package";
import { dropSession, isDirty, keepSession, openSession, releaseIfClean, reusableSession, saveSession, type DocxSession, type Parts } from "./session";

/** Word's usual font sizes (points) */
const SIZES = [8, 9, 10, 10.5, 11, 12, 14, 16, 18, 20, 22, 24, 26, 28, 36, 48, 72];

/** Common fonts: Word's Latin ones, then East Asian ones (by their English names) */
const FONTS: FontChoice[] = [
  ...["Aptos", "Arial", "Calibri", "Cambria", "Consolas", "Courier New", "Georgia", "Segoe UI", "Tahoma", "Times New Roman", "Verdana"].map((name) => ({ name, eastAsia: false })),
  ...["DengXian", "DFKai-SB", "Malgun Gothic", "Meiryo", "Microsoft JhengHei", "Microsoft YaHei", "MS Mincho", "PMingLiU", "SimSun", "Yu Gothic"].map((name) => ({
    name,
    eastAsia: true,
  })),
];

/** Word's line spacing choices, in 240ths of a line */
const LINE_SPACINGS: [number, string][] = [
  [240, "1.0"],
  [276, "1.15"],
  [360, "1.5"],
  [480, "2.0"],
  [600, "2.5"],
  [720, "3.0"],
];

/** A header's or footer's name, as Word names them; with its section's number after the first section */
function storyLabel(s: Story): string {
  const names: Record<StoryKind, Record<Story["type"], string>> = {
    header: { default: t("Header"), first: t("First page header"), even: t("Even page header") },
    footer: { default: t("Footer"), first: t("First page footer"), even: t("Even page footer") },
  };
  const name = names[s.kind][s.type];
  return s.section > 0 ? t("{name} (section {n})", { name, n: s.section + 1 }) : name;
}

/** Word's highlight colors, as Word lists them */
const HIGHLIGHT_COLORS = Object.keys(HIGHLIGHTS).map((hex) => `#${hex}`);

/** A paragraph style's name: Word's own styles in the person's language, the document's own as it names them */
function styleLabel(name: string): string {
  const n = name.toLowerCase();
  const heading = /^heading (\d)$/.exec(n);
  if (heading) return t("Heading {n}", { n: heading[1] });
  const known: Record<string, string> = {
    normal: t("Normal"),
    title: t("Title"),
    subtitle: t("Subtitle"),
    quote: t("Quote"),
    "list paragraph": t("List paragraph"),
    "no spacing": t("No spacing"),
    header: t("Header"),
    footer: t("Footer"),
  };
  return known[n] ?? name;
}

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

/** What the frame answers when asked for the edited document: the parts it changed (by path), the caret, the scroll */
interface Collected {
  parts: Parts;
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
  /** Whether the caret is in a table cell: the Insert and Delete menus change its table */
  const [inCell, setInCell] = useState(false);
  /** How many cells are selected together (dragging across them, or Shift+click): two or more can be merged */
  const [picked, setPicked] = useState(0);
  const [splitting, setSplitting] = useState(false);
  /** The formatting where the caret is, and the document's paragraph styles */
  const [fmt, setFmt] = useState<CaretFormat | null>(null);
  const [styles, setStyles] = useState<ParagraphStyle[]>([]);
  /** The fonts to choose from: the common ones and those the document's font table names */
  const [fonts, setFonts] = useState<FontChoice[]>(FONTS);
  /** The headers and footers the document has, the one open in the view (null: the body), and those that can be added */
  const [stories, setStories] = useState<Story[]>([]);
  const [story, setStory] = useState<string | null>(null);
  const [canAdd, setCanAdd] = useState<Record<StoryKind, boolean>>({ header: false, footer: false });
  const waiting = useRef(new Map<number, { resolve(c: Collected): void; reject(e: Error): void }>());
  const nextId = useRef(0);

  /** The tab is flagged, and closing the browser warns, while there are unsaved edits */
  const showDirty = (d: boolean) => {
    setDirtyState(d);
    setDraft(node.id, d ? { kind: "docx", base: session.base } : null);
  };

  /** What the frame holds becomes the session's */
  const absorb = (c: Collected) => {
    // The frame says every part it changed (none: the file as opened)
    const parts = c.parts && typeof c.parts === "object" ? c.parts : {};
    session.current = Object.keys(parts).length ? parts : null;
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
      const n = await saveSession(session, node.id, session.current);
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

  /** A row or column of the caret's table added or deleted, in the frame; the caret stays there */
  const table = (op: TableOp, change?: { cols?: number; rows?: number; border?: BorderKind; color?: string | null }) => {
    frame.current?.contentWindow?.postMessage({ type: "table", op, ...change }, "*");
    frame.current?.focus();
  };
  const border = (kind: BorderKind) => table("border", { border: kind });
  /** Formatting of the selection, in the frame; the caret stays there */
  const format = (change: {
    cmd?: string;
    value?: string;
    size?: number;
    style?: string;
    isDefault?: boolean;
    align?: string;
    list?: string;
    level?: number;
    font?: string;
    eastAsia?: boolean;
    line?: number;
    spaceBefore?: boolean;
    spaceAfter?: boolean;
    field?: "PAGE" | "NUMPAGES";
  }) => {
    frame.current?.contentWindow?.postMessage({ type: "format", ...change }, "*");
    frame.current?.focus();
  };
  /** A header or footer in the view (null: the body), or one added to the first section and opened */
  const openStory = (change: { path?: string | null; add?: StoryKind }) => {
    frame.current?.contentWindow?.postMessage({ type: "story", ...change }, "*");
    frame.current?.focus();
  };
  const current = stories.find((x) => x.path === story) ?? null;
  const formatOff = !fmt || loading || !!error;
  const tableOff = !inCell || loading || !!error;

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
      case "place": {
        const place = msg as { cell?: unknown; cells?: unknown; fmt?: CaretFormat | null };
        setInCell(!!place.cell);
        setPicked(Number(place.cells) || 0);
        setFmt(place.fmt ?? null);
        break;
      }
      case "styles": {
        const m = msg as { styles?: ParagraphStyle[]; fonts?: FontChoice[] };
        setStyles((m.styles ?? []).filter((x) => typeof x?.id === "string" && typeof x?.name === "string"));
        const own = (m.fonts ?? []).filter((x) => typeof x?.name === "string" && FONT_NAME.test(x.name)).map((x) => ({ name: x.name, eastAsia: !!x.eastAsia }));
        const byName = new Map([...FONTS, ...own].map((x) => [x.name.toLowerCase(), x]));
        setFonts([...byName.values()].sort((a, b) => Number(a.eastAsia) - Number(b.eastAsia) || a.name.localeCompare(b.name)));
        break;
      }
      case "stories": {
        const m = msg as { stories?: Story[]; story?: unknown; canAdd?: Partial<Record<StoryKind, unknown>> };
        const list = (m.stories ?? []).filter(
          (x) => typeof x?.path === "string" && (x.kind === "header" || x.kind === "footer") && ["default", "first", "even"].includes(x.type) && Number.isInteger(x.section),
        );
        setStories(list);
        setStory(typeof m.story === "string" && list.some((x) => x.path === m.story) ? m.story : null);
        setCanAdd({ header: !!m.canAdd?.header, footer: !!m.canAdd?.footer });
        break;
      }
      case "field-refused":
        toast.error(t("Put the cursor in the text first."));
        break;
      case "story-refused":
        toast.error(t("This can't be added here."));
        break;
      case "format-refused":
        toast.error(t("Select some text first."));
        break;
      case "table-refused":
        toast.error(t("This table can't be changed that way here."));
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
        frame.current?.contentWindow?.postMessage({ type: "edit", kind: "docx", buffer, parts: session.current ?? {}, texts, caret: session.caret, scroll: session.scroll }, "*", [buffer]);
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
        <p className="min-w-0 flex-1 text-xs text-muted-foreground">
          {current
            ? t("{name}: it shows on every page that uses it. Its text, formatting and tables can be changed here.", { name: storyLabel(current) })
            : t("Text, its formatting and tables can be changed here. Pictures and other parts are kept as they are.")}
        </p>
        {dirty && <span className="shrink-0 text-xs text-amber-600 dark:text-amber-400">{t("Unsaved changes")}</span>}
        <div className="flex shrink-0 items-center gap-1">
          <DropdownMenu>
            <DropdownMenuTrigger
              render={<Button variant={current ? "secondary" : "ghost"} size="sm" className="gap-1" disabled={loading || !!error} onMouseDown={(e) => e.preventDefault()} />}
            >
              <PanelsTopBottomIcon /> {current ? storyLabel(current) : t("Header and footer")}
              <ChevronDownIcon className="opacity-60" />
            </DropdownMenuTrigger>
            <DropdownMenuContent className="w-auto">
              <DropdownMenuRadioGroup value={story ?? ""} onValueChange={(v) => openStory({ path: v || null })}>
                <DropdownMenuRadioItem value="" closeOnClick>
                  {t("Document body")}
                </DropdownMenuRadioItem>
                {stories.map((x) => (
                  <DropdownMenuRadioItem key={x.path} value={x.path} closeOnClick>
                    {storyLabel(x)}
                  </DropdownMenuRadioItem>
                ))}
              </DropdownMenuRadioGroup>
              {(canAdd.header || canAdd.footer) && <DropdownMenuSeparator />}
              {canAdd.header && <DropdownMenuItem onClick={() => openStory({ add: "header" })}>{t("Add header")}</DropdownMenuItem>}
              {canAdd.footer && <DropdownMenuItem onClick={() => openStory({ add: "footer" })}>{t("Add footer")}</DropdownMenuItem>}
              <DropdownMenuSeparator />
              <DropdownMenuItem disabled={formatOff} onClick={() => format({ field: "PAGE" })}>
                {t("Insert page number")}
              </DropdownMenuItem>
              <DropdownMenuItem disabled={formatOff} onClick={() => format({ field: "NUMPAGES" })}>
                {t("Insert number of pages")}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
          {current && (
            <Button variant="outline" size="sm" disabled={loading || !!error} onClick={() => openStory({ path: null })}>
              {t("Back to document")}
            </Button>
          )}
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
      {/* Formatting of the selected text, then the table under the cursor */}
      <div className="flex min-h-10 shrink-0 flex-wrap items-center gap-0.5 border-b px-2 py-1">
        <NativeSelect
          aria-label={t("Paragraph style")}
          title={t("Paragraph style")}
          size="xs"
          className="w-36 shrink-0 rounded px-1"
          disabled={formatOff || !styles.length}
          value={fmt?.style ?? styles.find((x) => x.isDefault)?.id ?? ""}
          onChange={(e) => {
            const style = styles.find((x) => x.id === e.target.value);
            if (style) format({ style: style.id, isDefault: style.isDefault });
          }}
        >
          {fmt?.style && !styles.some((x) => x.id === fmt.style) && <option value={fmt.style}>{fmt.style}</option>}
          {styles.map((x) => (
            <option key={x.id} value={x.id}>
              {styleLabel(x.name)}
            </option>
          ))}
        </NativeSelect>
        <NativeSelect
          aria-label={t("Font")}
          title={t("Font")}
          size="xs"
          className="w-36 shrink-0 rounded px-1"
          disabled={formatOff}
          value={fmt?.font ?? ""}
          onChange={(e) => {
            const font = fonts.find((x) => x.name === e.target.value);
            if (font) format({ font: font.name, eastAsia: font.eastAsia });
          }}
        >
          {fmt?.font && !fonts.some((x) => x.name === fmt.font) && <option value={fmt.font}>{fmt.font}</option>}
          {fonts.map((x) => (
            <option key={x.name} value={x.name} style={{ fontFamily: `"${x.name}"` }}>
              {x.name}
            </option>
          ))}
        </NativeSelect>
        <NativeSelect
          aria-label={t("Font size")}
          title={t("Font size")}
          size="xs"
          className="w-16 shrink-0 rounded px-1"
          disabled={formatOff}
          value={fmt?.size ?? 11}
          onChange={(e) => format({ size: Number(e.target.value) })}
        >
          {[...new Set([...SIZES, fmt?.size ?? 11])]
            .sort((a, b) => a - b)
            .map((n) => (
              <option key={n} value={n}>
                {n}
              </option>
            ))}
        </NativeSelect>
        <span className="mx-1 h-5 shrink-0 border-l" />
        <FormatButton label={t("Bold")} keys="Ctrl+B" active={fmt?.b} disabled={formatOff} onClick={() => format({ cmd: "bold" })}>
          <BoldIcon />
        </FormatButton>
        <FormatButton label={t("Italic")} keys="Ctrl+I" active={fmt?.i} disabled={formatOff} onClick={() => format({ cmd: "italic" })}>
          <ItalicIcon />
        </FormatButton>
        <FormatButton label={t("Underline")} keys="Ctrl+U" active={fmt?.u} disabled={formatOff} onClick={() => format({ cmd: "underline" })}>
          <UnderlineIcon />
        </FormatButton>
        <FormatButton label={t("Strikethrough")} active={fmt?.s} disabled={formatOff} onClick={() => format({ cmd: "strikeThrough" })}>
          <StrikethroughIcon />
        </FormatButton>
        <ColorMenu
          label={t("Font color")}
          icon={<BaselineIcon className="size-4" />}
          noneLabel={t("Automatic")}
          disabled={formatOff}
          onPick={(c) => format({ cmd: "foreColor", value: c ?? "#000000" })}
        />
        <ColorMenu
          label={t("Highlight")}
          icon={<HighlighterIcon className="size-4" />}
          noneLabel={t("No highlight")}
          colors={HIGHLIGHT_COLORS}
          disabled={formatOff}
          onPick={(c) => format({ cmd: "hiliteColor", value: c ?? "transparent" })}
        />
        <span className="mx-1 h-5 shrink-0 border-l" />
        <FormatButton label={t("Bulleted list")} active={fmt?.list === "bullet"} disabled={formatOff} onClick={() => format({ list: "bullet" })}>
          <ListIcon />
        </FormatButton>
        <FormatButton label={t("Numbered list")} active={fmt?.list === "number"} disabled={formatOff} onClick={() => format({ list: "number" })}>
          <ListOrderedIcon />
        </FormatButton>
        <FormatButton label={t("Decrease indent")} keys="Shift+Tab" disabled={formatOff} onClick={() => format({ level: -1 })}>
          <IndentDecreaseIcon />
        </FormatButton>
        <FormatButton label={t("Increase indent")} keys="Tab" disabled={formatOff} onClick={() => format({ level: 1 })}>
          <IndentIncreaseIcon />
        </FormatButton>
        <span className="mx-1 h-5 shrink-0 border-l" />
        <FormatButton label={t("Align left")} active={fmt?.align === "left"} disabled={formatOff} onClick={() => format({ align: "left" })}>
          <AlignLeftIcon />
        </FormatButton>
        <FormatButton label={t("Center")} active={fmt?.align === "center"} disabled={formatOff} onClick={() => format({ align: "center" })}>
          <AlignCenterIcon />
        </FormatButton>
        <FormatButton label={t("Align right")} active={fmt?.align === "right"} disabled={formatOff} onClick={() => format({ align: "right" })}>
          <AlignRightIcon />
        </FormatButton>
        <FormatButton label={t("Justify")} active={fmt?.align === "both"} disabled={formatOff} onClick={() => format({ align: "both" })}>
          <AlignJustifyIcon />
        </FormatButton>
        <DropdownMenu>
          <DropdownMenuTrigger
            render={
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label={t("Line and paragraph spacing")}
                title={t("Line and paragraph spacing")}
                disabled={formatOff}
                onMouseDown={(e) => e.preventDefault()}
              />
            }
          >
            <ListChevronsUpDownIcon />
          </DropdownMenuTrigger>
          <DropdownMenuContent className="w-auto">
            <DropdownMenuRadioGroup value={fmt?.line != null ? String(fmt.line) : ""} onValueChange={(v) => format({ line: Number(v) })}>
              {LINE_SPACINGS.map(([line, label]) => (
                <DropdownMenuRadioItem key={line} value={String(line)} closeOnClick>
                  {label}
                </DropdownMenuRadioItem>
              ))}
            </DropdownMenuRadioGroup>
            <DropdownMenuSeparator />
            <DropdownMenuItem onClick={() => format({ spaceBefore: !fmt?.before })}>{fmt?.before ? t("Remove space before paragraph") : t("Add space before paragraph")}</DropdownMenuItem>
            <DropdownMenuItem onClick={() => format({ spaceAfter: !fmt?.after })}>{fmt?.after ? t("Remove space after paragraph") : t("Add space after paragraph")}</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
        <span className="mx-1 h-5 shrink-0 border-l" />
        <DropdownMenu>
          <DropdownMenuTrigger
            render={
              <Button
                variant="ghost"
                size="sm"
                title={tableOff ? t("Put the cursor in a table to change it") : t("Insert")}
                disabled={tableOff}
                className="gap-1"
                onMouseDown={(e) => e.preventDefault()}
              />
            }
          >
            <BetweenHorizontalStartIcon /> {t("Insert")}
            <ChevronDownIcon className="size-3 opacity-60" />
          </DropdownMenuTrigger>
          <DropdownMenuContent className="w-auto">
            <DropdownMenuItem onClick={() => table("rowAbove")}>
              <BetweenHorizontalStartIcon /> {t("Insert row above")}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => table("rowBelow")}>
              <BetweenHorizontalEndIcon /> {t("Insert row below")}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => table("colLeft")}>
              <BetweenVerticalStartIcon /> {t("Insert column left")}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => table("colRight")}>
              <BetweenVerticalEndIcon /> {t("Insert column right")}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
        <DropdownMenu>
          <DropdownMenuTrigger
            render={
              <Button
                variant="ghost"
                size="sm"
                title={tableOff ? t("Put the cursor in a table to change it") : t("Delete")}
                disabled={tableOff}
                className="gap-1"
                onMouseDown={(e) => e.preventDefault()}
              />
            }
          >
            <Rows3Icon /> {t("Delete")}
            <ChevronDownIcon className="size-3 opacity-60" />
          </DropdownMenuTrigger>
          <DropdownMenuContent className="w-auto">
            <DropdownMenuItem variant="destructive" onClick={() => table("deleteRow")}>
              <Rows3Icon /> {t("Delete row")}
            </DropdownMenuItem>
            <DropdownMenuItem variant="destructive" onClick={() => table("deleteCol")}>
              <Columns3Icon /> {t("Delete column")}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
        <DropdownMenu>
          <DropdownMenuTrigger
            render={
              <Button
                variant="ghost"
                size="sm"
                title={tableOff ? t("Put the cursor in a table to change it") : t("Cells")}
                disabled={tableOff}
                className="gap-1"
                onMouseDown={(e) => e.preventDefault()}
              />
            }
          >
            <TableCellsMergeIcon /> {t("Cells")}
            <ChevronDownIcon className="size-3 opacity-60" />
          </DropdownMenuTrigger>
          <DropdownMenuContent className="w-auto">
            <DropdownMenuItem disabled={picked < 2} onClick={() => table("merge")}>
              <TableCellsMergeIcon /> {t("Merge cells")}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => setSplitting(true)}>
              <TableCellsSplitIcon /> {t("Split cell…")}
            </DropdownMenuItem>
            {picked < 2 && <p className="max-w-64 px-2 py-1.5 text-xs text-muted-foreground">{t("To merge cells, drag across them or Shift+click another cell first.")}</p>}
          </DropdownMenuContent>
        </DropdownMenu>
        {/* Borders and shading: of the cells selected together, or of the cell under the cursor */}
        <DropdownMenu>
          <DropdownMenuTrigger
            render={
              <Button
                variant="ghost"
                size="sm"
                title={tableOff ? t("Put the cursor in a table to change it") : t("Borders")}
                aria-label={t("Borders")}
                disabled={tableOff}
                className="gap-0.5 px-1.5"
                onMouseDown={(e) => e.preventDefault()}
              />
            }
          >
            <Grid3x3Icon />
            <ChevronDownIcon className="size-3 opacity-60" />
          </DropdownMenuTrigger>
          <DropdownMenuContent className="w-auto">
            <DropdownMenuItem onClick={() => border("all")}>
              <Grid3x3Icon /> {t("All borders")}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => border("outside")}>
              <SquareIcon /> {t("Outside borders")}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => border("inside")}>
              <Grid3x3Icon className="opacity-70" /> {t("Inside borders")}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => border("thickOutside")}>
              <SquareIcon className="stroke-[3]" /> {t("Thick outside borders")}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => border("bottom")}>
              <PanelBottomIcon /> {t("Bottom border")}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => border("top")}>
              <PanelBottomIcon className="rotate-180" /> {t("Top border")}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => border("left")}>
              <PanelBottomIcon className="rotate-90" /> {t("Left border")}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => border("right")}>
              <PanelBottomIcon className="-rotate-90" /> {t("Right border")}
            </DropdownMenuItem>
            <DropdownMenuSeparator />
            <DropdownMenuItem onClick={() => border("none")}>
              <SquareDashedIcon /> {t("No border")}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
        <ColorMenu label={t("Shading")} icon={<PaintBucketIcon className="size-4" />} noneLabel={t("No color")} disabled={tableOff} onPick={(c) => table("shade", { color: c ?? null })} />
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
      {splitting && (
        <SplitDialog
          onClose={() => setSplitting(false)}
          onSplit={(split) => {
            setSplitting(false);
            table("split", split);
          }}
        />
      )}
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

/** Splitting a cell: into how many columns and rows */
function SplitDialog({ onClose, onSplit }: { onClose(): void; onSplit(split: { cols: number; rows: number }): void }) {
  const [cols, setCols] = useState("2");
  const [rows, setRows] = useState("1");
  const valid = (v: string, max: number) => /^\d+$/.test(v) && Number(v) >= 1 && Number(v) <= max;
  const ok = valid(cols, MAX_SPLIT.cols) && valid(rows, MAX_SPLIT.rows) && (cols !== "1" || rows !== "1");
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-xs">
        <form
          className="grid gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            if (ok) onSplit({ cols: Number(cols), rows: Number(rows) });
          }}
        >
          <DialogHeader>
            <DialogTitle>{t("Split cell")}</DialogTitle>
          </DialogHeader>
          <div className="grid grid-cols-2 gap-3">
            <div className="grid gap-1.5">
              <Label htmlFor="split-cols">{t("Number of columns")}</Label>
              <Input id="split-cols" type="number" min={1} max={MAX_SPLIT.cols} value={cols} onChange={(e) => setCols(e.target.value)} autoFocus />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="split-rows">{t("Number of rows")}</Label>
              <Input id="split-rows" type="number" min={1} max={MAX_SPLIT.rows} value={rows} onChange={(e) => setRows(e.target.value)} />
            </div>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={!ok}>
              {t("Split")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** A formatting button: pressed while the text at the caret has it; clicking it keeps the caret where it is */
function FormatButton(props: { label: string; keys?: string; active?: boolean; disabled?: boolean; onClick(): void; children: React.ReactNode }) {
  const label = props.keys ? t("{action} ({keys})", { action: props.label, keys: shortcut(props.keys) }) : props.label;
  return (
    <Button
      variant={props.active ? "secondary" : "ghost"}
      size="icon-sm"
      title={label}
      aria-label={props.label}
      aria-pressed={!!props.active}
      disabled={props.disabled}
      className="size-7 shrink-0"
      onMouseDown={(e) => e.preventDefault()}
      onClick={props.onClick}
    >
      {props.children}
    </Button>
  );
}
