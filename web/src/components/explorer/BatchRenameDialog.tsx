/** Renaming several items at once: how their names change, with every old and new name shown before anything does */
import { useEffect, useId, useMemo, useRef, useState, type Ref } from "react";
import { useQuery } from "@tanstack/react-query";
import { ArrowRightIcon, Loader2Icon } from "lucide-react";
import { api } from "@/api";
import { ErrorText } from "@/components/dialogs";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect } from "@/components/ui/native-select";
import { type NameProblem, newName, nameProblems, type RenameRule, takenKey } from "@/lib/batchRename";
import { reportShown } from "@/lib/errorReport";
import { t, tServer } from "@/lib/i18n";
import { useSettled } from "@/lib/useSettled";
import { useSubmit } from "@/lib/useSubmit";
import { cn, errorMessage } from "@/lib/utils";
import type { Item } from "./types";

interface FieldProps {
  id: string;
  label: string;
  value: string;
  onChange(value: string): void;
  ref?: Ref<HTMLInputElement>;
  className?: string;
  type?: string;
  min?: number;
  max?: number;
}

export function BatchRenameDialog(props: {
  /** How many items are selected */
  count: number;
  /** The items selected, in the order the list shows them */
  load(signal: AbortSignal): Promise<Item[]>;
  /** Renames them; `changes` are the items whose names change */
  onRename(items: Item[], changes: { id: string; name: string }[]): Promise<void>;
  onClose(): void;
}) {
  // As selected when the dialog opened: the list may load again behind it
  const [load] = useState(() => props.load);
  const [items, setItems] = useState<Item[] | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  useEffect(() => {
    const abort = new AbortController();
    load(abort.signal).then(
      (found) => !abort.signal.aborted && setItems(found),
      (e) => {
        if (abort.signal.aborted) return;
        setFailed(errorMessage(e, t("Couldn't rename")));
        reportShown("rename", e);
      },
    );
    return () => abort.abort();
  }, [load]);

  const [mode, setMode] = useState<RenameRule["mode"]>("replace");
  const [find, setFind] = useState("");
  const [replace, setReplace] = useState("");
  const [matchCase, setMatchCase] = useState(false);
  const [text, setText] = useState("");
  const [before, setBefore] = useState(false);
  const [base, setBase] = useState("");
  const [start, setStart] = useState("1");
  const [digits, setDigits] = useState("1");
  const [extensions, setExtensions] = useState(false);

  const names = useMemo(() => {
    const whole = (value: string, min: number, max: number) => Math.min(max, Math.max(min, Math.floor(Number(value)) || 0));
    const rule: RenameRule =
      mode === "replace"
        ? { mode, find, replace, matchCase, extensions }
        : mode === "add"
          ? { mode, text, before, extensions }
          : { mode, base, start: whole(start, 0, 1_000_000_000), digits: whole(digits, 1, 10) };
    return (items ?? []).map((n, i) => newName(n, i, rule));
  }, [items, mode, find, replace, matchCase, text, before, base, start, digits, extensions]);
  const changes = useMemo(() => (items ?? []).flatMap((n, i) => (names[i] === n.name ? [] : [{ id: n.id, name: names[i] }])), [items, names]);

  // The new names that items keeping theirs have, asked once typing pauses (the server checks again when renaming)
  const wanted = useMemo(() => {
    const folders = new Map<string, string[]>();
    (items ?? []).forEach((n, i) => {
      if (n.parent_id && names[i] !== n.name) folders.set(n.parent_id, [...(folders.get(n.parent_id) ?? []), names[i]]);
    });
    return JSON.stringify([...folders]);
  }, [items, names]);
  const settled = useSettled(wanted, 300);
  const changing = useMemo(() => JSON.stringify(changes.map((c) => c.id)), [changes]);
  const { data: taken } = useQuery({
    queryKey: ["rename-taken", settled, changing],
    enabled: settled !== "[]",
    gcTime: 0,
    placeholderData: (previous: Set<string> | undefined) => previous,
    queryFn: async () => {
      const free = new Set(JSON.parse(changing) as string[]);
      const asked = JSON.parse(settled) as [string, string[]][];
      const found = await Promise.all(
        asked.map(async ([folder, list]) => {
          // A folder that can't be looked into (an item shared on its own): the server says when renaming
          const conflicts = await api.conflicts({ dest_id: folder, names: list }).catch(() => []);
          return conflicts.filter((c) => !free.has(c.existing.id)).map((c) => takenKey(folder, c.name));
        }),
      );
      return new Set(found.flat());
    },
  });
  const problems = useMemo(() => nameProblems(items ?? [], names, taken ?? new Set()), [items, names, taken]);
  const wrong = problems.filter(Boolean).length;

  const { busy, error, run } = useSubmit(() => props.onRename(items!, changes));
  const id = useId();
  const field = (name: string) => `${id}-${name}`;
  const hasFiles = !!items?.some((n) => n.kind !== "folder");
  // Typing goes into the text looked for as soon as the dialog opens
  const first = useRef<HTMLInputElement>(null);

  return (
    <Dialog open onOpenChange={(o) => !o && props.onClose()}>
      <DialogContent className="sm:max-w-xl" initialFocus={first}>
        <form onSubmit={run} className="grid min-w-0 gap-4">
          <DialogHeader>
            <DialogTitle>{t("Rename {n} item|Rename {n} items", { n: items?.length ?? props.count })}</DialogTitle>
          </DialogHeader>
          <div className="grid gap-2">
            <Label htmlFor={field("mode")}>{t("Method")}</Label>
            <NativeSelect id={field("mode")} value={mode} onChange={(e) => setMode(e.target.value as RenameRule["mode"])}>
              <option value="replace">{t("Replace text")}</option>
              <option value="add">{t("Add text")}</option>
              <option value="number">{t("Number the items")}</option>
            </NativeSelect>
          </div>
          {mode === "replace" && (
            <div className="grid gap-3 sm:grid-cols-2">
              <Field ref={first} id={field("find")} label={t("Find")} value={find} onChange={setFind} />
              <Field id={field("replace")} label={t("Replace with")} value={replace} onChange={setReplace} />
              <Check label={t("Match case")} checked={matchCase} onChange={setMatchCase} />
            </div>
          )}
          {mode === "add" && (
            <div className="grid gap-3 sm:grid-cols-2">
              <Field id={field("text")} label={t("Text to add")} value={text} onChange={setText} />
              <div className="grid gap-2">
                <Label htmlFor={field("where")}>{t("Place")}</Label>
                <NativeSelect id={field("where")} value={before ? "before" : "after"} onChange={(e) => setBefore(e.target.value === "before")}>
                  <option value="before">{t("Before the name")}</option>
                  <option value="after">{t("After the name")}</option>
                </NativeSelect>
              </div>
            </div>
          )}
          {mode === "number" && (
            <div className="grid grid-cols-2 gap-3 sm:grid-cols-[1fr_7rem_5rem]">
              <Field id={field("base")} label={t("Base name")} value={base} onChange={setBase} className="col-span-2 sm:col-span-1" />
              <Field id={field("start")} label={t("Start at")} value={start} onChange={setStart} type="number" min={0} />
              <Field id={field("digits")} label={t("Digits")} value={digits} onChange={setDigits} type="number" min={1} max={10} />
            </div>
          )}
          {mode !== "number" && hasFiles && <Check label={t("Also change file extensions")} checked={extensions} onChange={setExtensions} />}

          {items ? (
            <ul aria-label={t("Preview")} tabIndex={0} className="max-h-56 min-h-20 overflow-y-auto rounded-md border text-xs">
              {items.map((n, i) => (
                <li key={n.id} className="grid grid-cols-[1fr_auto_1fr] items-start gap-x-2 border-b px-2 py-1 last:border-b-0">
                  <span className="min-w-0 break-all text-muted-foreground">{n.name}</span>
                  <ArrowRightIcon aria-hidden className="mt-0.5 size-3 text-muted-foreground" />
                  <span className={cn("min-w-0 break-all whitespace-pre-wrap", names[i] === n.name && "text-muted-foreground", problems[i] && "text-destructive")}>
                    {names[i]}
                    {problems[i] && <span className="block">{problemText(problems[i])}</span>}
                  </span>
                </li>
              ))}
            </ul>
          ) : (
            !failed && (
              <p role="status" className="flex items-center gap-2 text-muted-foreground">
                <Loader2Icon className="size-4 animate-spin" /> {t("Loading…")}
              </p>
            )
          )}
          {items && (
            <p role="status" className={cn("text-xs", wrong ? "text-destructive" : "text-muted-foreground")}>
              {wrong
                ? t("{n} name needs fixing|{n} names need fixing", { n: wrong })
                : t("{n} item of {total} will be renamed|{n} items of {total} will be renamed", { n: changes.length, total: items.length })}
            </p>
          )}
          <ErrorText>{failed ?? error}</ErrorText>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={props.onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={busy || !changes.length || wrong > 0}>
              {busy && <Loader2Icon className="animate-spin" />}
              {t("Rename")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function problemText(problem: NameProblem) {
  if (problem.type === "invalid") return tServer(problem.message);
  return problem.type === "repeated" ? t("Several items would get this name") : t("Another item here has this name");
}

function Field({ label, value, onChange, className, ...input }: FieldProps) {
  return (
    <div className={cn("grid content-start gap-2", className)}>
      <Label htmlFor={input.id}>{label}</Label>
      <Input value={value} onChange={(e) => onChange(e.target.value)} autoComplete="off" spellCheck={false} {...input} />
    </div>
  );
}

function Check({ label, checked, onChange }: { label: string; checked: boolean; onChange(checked: boolean): void }) {
  return (
    <Label className="flex items-center gap-2 font-normal">
      <Checkbox checked={checked} onCheckedChange={(on) => onChange(on === true)} />
      {label}
    </Label>
  );
}
