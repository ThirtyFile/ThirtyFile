//! Smart folders in the interface (lib/smartFolders.ts): the dialog that saves a search as one or changes what it looks
//! for, the line that says what it looks for, and deleting one. The same in every interface style.

import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { Loader2Icon } from "lucide-react";
import { api, type SmartFolder, type SmartQuery } from "@/api";
import { keys } from "@/api/queryKeys";
import { ErrorText, errorProps } from "@/components/dialogs";
import { TagDot } from "@/components/tags";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect } from "@/components/ui/native-select";
import { confirm } from "@/lib/confirm";
import { useDrives } from "@/lib/drives";
import { t } from "@/lib/i18n";
import { SEARCH_DATES, SEARCH_SIZES, SEARCH_TYPES } from "@/lib/searchFilters";
import { MAX_SMART_NAME, createSmartFolder, deleteSmartFolder, formOf, queryOf, scopeValue, smartDialog, updateSmartFolder, type SmartForm } from "@/lib/smartFolders";
import { useStore } from "@/lib/store";
import { useTags } from "@/lib/tags";
import { useSubmit } from "@/lib/useSubmit";
import { errorMessage, formatBytes, formatDate } from "@/lib/utils";

/** The name of the folder a smart folder looks in, as the person can open it; null when they can't (any more) */
function useFolderName(id: string | undefined) {
  const q = useQuery({ queryKey: keys.node(id), queryFn: () => api.node(id!), enabled: !!id, retry: false });
  if (!id) return { name: undefined, missing: false };
  const name = q.data ? (q.data.is_root ? q.data.drive.name : q.data.node.name) : undefined;
  return { name, missing: !!q.error };
}

/** Where a smart folder looks, in words */
function useScopeLabel(query: SmartQuery) {
  const drives = useDrives();
  const folder = useFolderName(query.scope.kind === "folder" ? query.scope.id : undefined);
  const scope = query.scope;
  if (scope.kind === "all") return { label: t("Everywhere I have access"), missing: false };
  if (scope.kind === "space") {
    const drive = drives.data?.find((d) => d.id === scope.id);
    return { label: drive?.name ?? (drives.isLoading ? "…" : t("A space you can no longer open")), missing: !drives.isLoading && !drive };
  }
  if (folder.missing) return { label: t("A folder you can no longer open"), missing: true };
  return { label: t("{name} and its subfolders", { name: folder.name ?? "…" }), missing: false };
}

/** What a smart folder looks for, in words: each part of its query */
export function QuerySummary({ query }: { query: SmartQuery }) {
  const { byId } = useTags(!!query.tags?.length);
  const scope = useScopeLabel(query);
  const f = formOf("", query);
  const parts: ReactNode[] = [];
  if (f.term) parts.push(t('Name contains "{term}"', { term: f.term }));
  if (f.type === "file") parts.push(t("Files"));
  else if (f.type === "custom") parts.push(t("File types: {types}", { types: f.ext }));
  else if (f.type) parts.push(SEARCH_TYPES.find((x) => x.id === f.type)?.label());
  if (f.date === "days") parts.push(t("Modified in the last {n} days", { n: f.days }));
  else if (f.date === "range")
    parts.push(
      query.modified_from !== undefined && query.modified_to !== undefined
        ? t("Modified {from} to {to}", { from: formatDate(query.modified_from), to: formatDate(query.modified_to - 1) })
        : query.modified_from !== undefined
          ? t("Modified since {date}", { date: formatDate(query.modified_from) })
          : t("Modified before {date}", { date: formatDate(query.modified_to!) }),
    );
  else if (f.date) parts.push(t("Modified: {when}", { when: SEARCH_DATES.find((x) => x.id === f.date)?.label() ?? "" }));
  if (f.size === "range")
    parts.push(
      query.min_size !== undefined && query.max_size !== undefined
        ? t("Size {min} to {max}", { min: formatBytes(query.min_size), max: formatBytes(query.max_size) })
        : query.min_size !== undefined
          ? t("At least {size}", { size: formatBytes(query.min_size) })
          : t("At most {size}", { size: formatBytes(query.max_size!) }),
    );
  else if (f.size) parts.push(SEARCH_SIZES.find((x) => x.id === f.size)?.label());
  for (const id of query.tags ?? []) {
    const tag = byId.get(id);
    parts.push(
      tag ? (
        <span className="inline-flex items-center gap-1">
          <TagDot color={tag.color} />
          {tag.name}
        </span>
      ) : (
        t("A deleted tag")
      ),
    );
  }
  parts.push(<span className={scope.missing ? "text-destructive" : undefined}>{scope.label}</span>);
  return (
    <ul aria-label={t("Looks for")} className="flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-1">
      {parts.map((p, i) => (
        <li key={i} className="flex items-center gap-1.5">
          {i > 0 && <span aria-hidden>·</span>}
          {p}
        </li>
      ))}
    </ul>
  );
}

/** Asks, then deletes a smart folder (the items it lists stay where they are); leaves its page when it is the one shown */
export async function askToDeleteSmartFolder(qc: QueryClient, folder: SmartFolder, leave?: () => void) {
  const ok = await confirm({
    title: t('Delete the smart folder "{name}"?', { name: folder.name }),
    description: t("Only the saved search is deleted. The items it shows stay where they are."),
    confirmText: t("Delete"),
    destructive: true,
  });
  if (!ok) return;
  try {
    await deleteSmartFolder(qc, folder.id);
    leave?.();
  } catch (e) {
    toast.error(errorMessage(e, t("Couldn't delete the smart folder")));
  }
}

const OTHER_DAYS = "days";
const RANGE = "range";

/** Saves a search as a smart folder (`draft`), or changes `folder` */
function SmartFolderDialog({ folder, draft, onDone }: { folder?: SmartFolder; draft?: { name: string; query: SmartQuery }; onDone(f: SmartFolder | null): void }) {
  const qc = useQueryClient();
  const [form, setForm] = useState<SmartForm>(() => formOf(folder?.name ?? draft?.name ?? "", folder?.query ?? draft?.query ?? { scope: { kind: "all" } }));
  const set = <K extends keyof SmartForm>(key: K, value: SmartForm[K]) => setForm((f) => ({ ...f, [key]: value }));
  const { tags } = useTags();
  const drives = useDrives();
  const initialScope = folder?.query.scope ?? draft?.query.scope ?? { kind: "all" };
  const lookedIn = useFolderName(initialScope.kind === "folder" ? initialScope.id : undefined);
  const ref = useRef<HTMLInputElement>(null);
  const ids = { name: useId(), term: useId(), scope: useId(), type: useId(), ext: useId(), date: useId(), size: useId(), error: useId() };
  const { busy, error, run } = useSubmit(async () => {
    const query = queryOf(form);
    const saved = folder ? await updateSmartFolder(qc, folder.id, { name: form.name.trim(), query }) : await createSmartFolder(qc, form.name.trim(), query);
    onDone(saved);
  });
  useEffect(() => {
    const timer = setTimeout(() => ref.current?.select(), 50);
    return () => clearTimeout(timer);
  }, []);
  const spaces = (drives.data ?? []).filter((d) => !d.disabled);
  const sizeField = "w-24";
  return (
    <Dialog open onOpenChange={(o) => !o && onDone(null)}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-lg">
        <form onSubmit={run} className="grid gap-4">
          <DialogHeader>
            <DialogTitle>{folder ? t("Edit smart folder") : t("New smart folder")}</DialogTitle>
            <DialogDescription>{t("A smart folder shows what matches whenever you open it. Only you see your smart folders.")}</DialogDescription>
          </DialogHeader>
          <div className="grid gap-2">
            <Label htmlFor={ids.name}>{t("Name")}</Label>
            <Input id={ids.name} ref={ref} value={form.name} maxLength={MAX_SMART_NAME} onChange={(e) => set("name", e.target.value)} autoComplete="off" {...errorProps(error, ids.error)} />
          </div>
          <fieldset className="grid gap-3">
            <legend className="mb-1 text-sm font-medium">{t("Show items that match")}</legend>
            <div className="grid gap-1.5">
              <Label htmlFor={ids.term}>{t("Name contains")}</Label>
              <Input id={ids.term} value={form.term} onChange={(e) => set("term", e.target.value)} autoComplete="off" />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor={ids.scope}>{t("Look in")}</Label>
              <NativeSelect id={ids.scope} value={form.scope} onChange={(e) => set("scope", e.target.value)}>
                <option value="all">{t("Everywhere I have access")}</option>
                {initialScope.kind === "folder" && (
                  <option value={scopeValue(initialScope)}>{lookedIn.missing ? t("A folder you can no longer open") : t("{name} and its subfolders", { name: lookedIn.name ?? "…" })}</option>
                )}
                {initialScope.kind === "space" && !spaces.some((d) => d.id === initialScope.id) && (
                  <option value={scopeValue(initialScope)}>{drives.isLoading ? "…" : t("A space you can no longer open")}</option>
                )}
                {spaces.map((d) => (
                  <option key={d.id} value={`space:${d.id}`}>
                    {d.name}
                  </option>
                ))}
              </NativeSelect>
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor={ids.type}>{t("Type")}</Label>
              <NativeSelect id={ids.type} value={form.type} onChange={(e) => set("type", e.target.value)}>
                <option value="">{t("Any")}</option>
                <option value="file">{t("Files")}</option>
                {SEARCH_TYPES.map((x) => (
                  <option key={x.id} value={x.id}>
                    {x.label()}
                  </option>
                ))}
                <option value="custom">{t("Other file types…")}</option>
              </NativeSelect>
              {form.type === "custom" && (
                <>
                  <Label htmlFor={ids.ext} className="sr-only">
                    {t("File types")}
                  </Label>
                  <Input id={ids.ext} value={form.ext} onChange={(e) => set("ext", e.target.value)} placeholder={t("Extensions, such as pdf, docx")} autoComplete="off" />
                </>
              )}
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor={ids.date}>{t("Date modified")}</Label>
              <NativeSelect id={ids.date} value={form.date} onChange={(e) => set("date", e.target.value)}>
                <option value="">{t("Any")}</option>
                {SEARCH_DATES.map((x) => (
                  <option key={x.id} value={x.id}>
                    {x.label()}
                  </option>
                ))}
                {form.date === OTHER_DAYS && <option value={OTHER_DAYS}>{t("Modified in the last {n} days", { n: form.days })}</option>}
                <option value={RANGE}>{t("Between dates…")}</option>
              </NativeSelect>
              {form.date === RANGE && (
                <div className="flex flex-wrap items-center gap-2 text-sm">
                  <label className="flex items-center gap-1.5">
                    {t("From")}
                    <Input type="date" className="w-auto" value={form.from} max={form.to || undefined} onChange={(e) => set("from", e.target.value)} />
                  </label>
                  <label className="flex items-center gap-1.5">
                    {t("To")}
                    <Input type="date" className="w-auto" value={form.to} min={form.from || undefined} onChange={(e) => set("to", e.target.value)} />
                  </label>
                </div>
              )}
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor={ids.size}>{t("Size")}</Label>
              <NativeSelect id={ids.size} value={form.size} onChange={(e) => set("size", e.target.value)}>
                <option value="">{t("Any")}</option>
                {SEARCH_SIZES.map((x) => (
                  <option key={x.id} value={x.id}>
                    {x.label()}
                  </option>
                ))}
                <option value={RANGE}>{t("Between sizes…")}</option>
              </NativeSelect>
              {form.size === RANGE && (
                <div className="flex flex-wrap items-center gap-2 text-sm">
                  <label className="flex items-center gap-1.5">
                    {t("From")}
                    <Input type="number" min={0} step="any" inputMode="decimal" className={sizeField} value={form.minMb} onChange={(e) => set("minMb", e.target.value)} />
                    MB
                  </label>
                  <label className="flex items-center gap-1.5">
                    {t("To")}
                    <Input type="number" min={0} step="any" inputMode="decimal" className={sizeField} value={form.maxMb} onChange={(e) => set("maxMb", e.target.value)} />
                    MB
                  </label>
                </div>
              )}
            </div>
            {tags.length > 0 && (
              <fieldset className="grid gap-1.5">
                <legend className="mb-1 text-sm font-medium">{t("With all of these tags")}</legend>
                <div className="flex flex-wrap gap-x-4 gap-y-1.5">
                  {tags.map((tag) => (
                    <label key={tag.id} className="flex cursor-pointer items-center gap-1.5 text-sm">
                      <input
                        type="checkbox"
                        className="size-4 accent-primary"
                        checked={form.tags.includes(tag.id)}
                        onChange={(e) => set("tags", e.target.checked ? [...form.tags, tag.id] : form.tags.filter((x) => x !== tag.id))}
                      />
                      <TagDot color={tag.color} />
                      {tag.name}
                    </label>
                  ))}
                </div>
              </fieldset>
            )}
          </fieldset>
          <ErrorText id={ids.error}>{error}</ErrorText>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onDone(null)}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={busy || !form.name.trim()}>
              {busy && <Loader2Icon className="animate-spin" />}
              {folder ? t("Save") : t("Create")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** Shows the dialog asked for with editSmartFolder() (lib/smartFolders.ts); mounted once for the signed-in app */
export function SmartFolderDialogHost() {
  const req = useStore(smartDialog);
  if (!req) return null;
  return (
    <SmartFolderDialog
      key={req.folder?.id ?? "new"}
      folder={req.folder}
      draft={req.draft}
      onDone={(folder) => {
        if (smartDialog.get() === req) smartDialog.set(null);
        req.resolve(folder);
      }}
    />
  );
}
