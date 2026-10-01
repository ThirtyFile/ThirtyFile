import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { ChevronRightIcon, FileIcon, FolderIcon, Loader2Icon } from "lucide-react";
import { toast } from "sonner";
import { api, type BackupSet, type RestoreRequest } from "@/api";
import { keys } from "@/api/queryKeys";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { NativeSelect } from "@/components/ui/native-select";
import { ErrorText } from "@/components/dialogs";
import { backupSpaceLabel } from "@/admin/storage/backups";
import { t, tServer } from "@/lib/i18n";
import { formatBytes, formatDateTime, errorMessage } from "@/lib/utils";
import { useSubmit } from "@/lib/useSubmit";

/** "2026-10-01 14.05" in the browser's time zone: a name can't hold "/" or ":", which dates in some languages have */
function nameDate(t: number) {
  const d = new Date(t * 1000);
  const two = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${two(d.getMonth() + 1)}-${two(d.getDate())} ${two(d.getHours())}.${two(d.getMinutes())}`;
}

/**
 * Restoring from a copy or backup: a restore point, a space of it, and for company and team spaces a folder or some
 * items of it. They go into a new folder, or back into their place with a choice for names already taken. A personal
 * space is restored whole into its owner's personal space: administrators don't see its files.
 */
export function RestoreDialog({ set, onClose, onDone }: { set: BackupSet; onClose(): void; onDone(): void }) {
  const points = set.snapshots.filter((s) => s.state === "complete");
  const [snapshotId, setSnapshotId] = useState(points[0]?.id ?? "");
  const snapshot = points.find((s) => s.id === snapshotId) ?? points[0];
  const [space, setSpace] = useState(snapshot?.spaces[0]?.id ?? "");
  const chosen = snapshot?.spaces.find((s) => s.id === space);
  const personal = chosen?.kind === "personal";
  const [choose, setChoose] = useState(false);
  const [folder, setFolder] = useState<string | null>(null);
  const [items, setItems] = useState<string[]>([]);
  const [mode, setMode] = useState<"new_folder" | "original">("new_folder");
  const [onConflict, setOnConflict] = useState<"skip" | "keep" | "replace">("skip");
  const [target, setTarget] = useState<string | null>(null);
  const [trash, setTrash] = useState(false);
  const tz = new Date().getTimezoneOffset();
  const folderName = chosen && snapshot?.cutoff ? t("Restored {name} {date}", { name: chosen.name, date: nameDate(snapshot.cutoff) }) : "";
  const browsing = choose && !personal;
  const page = useQuery({
    queryKey: keys.snapshotBrowse(snapshotId, space, folder),
    queryFn: () => api.browseSnapshot(snapshotId, space, folder),
    enabled: browsing && !!snapshotId && !!space,
  });
  const req: RestoreRequest = {
    space,
    folder: browsing ? (folder ?? page.data?.path[0]?.[0] ?? null) : null,
    items: browsing && items.length ? items : null,
    target_drive: mode === "original" ? null : target,
    mode,
    on_conflict: onConflict,
    trash,
    tz,
    folder_name: folderName,
  };
  const preview = useQuery({
    queryKey: keys.restorePreview(snapshotId, JSON.stringify(req)),
    queryFn: () => api.restorePreview(snapshotId, req),
    enabled: !!snapshotId && !!space && (!browsing || !!page.data),
    retry: false,
  });
  const p = preview.data;
  const reset = () => {
    setFolder(null);
    setItems([]);
    setTarget(null);
  };
  const { busy, error, run } = useSubmit(async () => {
    if (!p || p.problem) return;
    await api.restoreBackup(snapshotId, { ...req, target_drive: mode === "original" ? null : p.target_drive });
    toast.success(t('"{name}" is being restored in the background', { name: chosen ? backupSpaceLabel(chosen) : "" }));
    onDone();
    onClose();
  }, t("Couldn't make the change"));
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-xl">
        <form className="grid gap-4" onSubmit={run}>
          <DialogHeader>
            <DialogTitle>{t('Restore from "{name}"', { name: set.name })}</DialogTitle>
            <DialogDescription>
              {t("Files are copied back from the backup and checked against their fingerprints. Nothing already there is replaced unless you choose to.")}
            </DialogDescription>
          </DialogHeader>
          {points.length > 1 && (
            <div className="grid gap-2">
              <Label htmlFor="restore-point">{t("Restore point")}</Label>
              <NativeSelect
                id="restore-point"
                size="lg"
                value={snapshotId}
                onChange={(e) => {
                  setSnapshotId(e.target.value);
                  reset();
                }}
              >
                {points.map((s) => (
                  <option key={s.id} value={s.id}>
                    {s.cutoff ? t("As of {time}", { time: formatDateTime(s.cutoff) }) : s.id}
                  </option>
                ))}
              </NativeSelect>
            </div>
          )}
          {points.length === 1 && snapshot?.cutoff && <p className="text-sm">{t("The copy shows the spaces as they were on {time}.", { time: formatDateTime(snapshot.cutoff) })}</p>}
          <div className="grid gap-2">
            <Label htmlFor="restore-space">{t("Space to restore")}</Label>
            <NativeSelect
              id="restore-space"
              size="lg"
              value={space}
              onChange={(e) => {
                setSpace(e.target.value);
                reset();
              }}
            >
              {(snapshot?.spaces ?? []).map((s) => (
                <option key={s.id} value={s.id}>
                  {backupSpaceLabel(s)} · {t("{n} file|{n} files", { n: s.files })}
                </option>
              ))}
            </NativeSelect>
          </div>
          {!personal && (
            <fieldset className="grid gap-2 text-sm">
              <legend className="mb-1 font-medium">{t("What")}</legend>
              <label className="flex items-center gap-2">
                <input type="radio" name="restore-what" checked={!choose} onChange={() => setChoose(false)} />
                {t("The whole space")}
              </label>
              <label className="flex items-center gap-2">
                <input type="radio" name="restore-what" checked={choose} onChange={() => setChoose(true)} />
                {t("A folder, or some of its items")}
              </label>
              {browsing && (
                <div className="grid gap-1 rounded-md border">
                  <div className="flex flex-wrap items-center gap-0.5 border-b px-2 py-1.5 text-xs">
                    {(page.data?.path ?? []).map(([id, name], i) => (
                      <span key={id} className="flex items-center gap-0.5">
                        {i > 0 && <ChevronRightIcon className="size-3 text-muted-foreground" />}
                        <button
                          type="button"
                          className="rounded px-1 hover:bg-muted"
                          onClick={() => {
                            setFolder(id);
                            setItems([]);
                          }}
                        >
                          {i === 0 ? (chosen?.name ?? name) : name}
                        </button>
                      </span>
                    ))}
                  </div>
                  {page.isLoading && <Loader2Icon className="m-2 size-4 animate-spin" />}
                  <ul className="max-h-48 divide-y overflow-y-auto text-xs">
                    {(page.data?.items ?? [])
                      .filter((i) => trash || !i.trashed)
                      .map((i) => (
                        <li key={i.id} className="flex items-center gap-2 px-2 py-1">
                          <Checkbox
                            aria-label={i.name}
                            checked={items.includes(i.id)}
                            onCheckedChange={(v) => setItems((cur) => (v === true ? [...cur, i.id] : cur.filter((x) => x !== i.id)))}
                          />
                          {i.kind === "folder" ? <FolderIcon className="size-3.5 shrink-0 text-amber-500" /> : <FileIcon className="size-3.5 shrink-0 text-muted-foreground" />}
                          {i.kind === "folder" ? (
                            <button
                              type="button"
                              className="min-w-0 flex-1 truncate text-left hover:underline"
                              onClick={() => {
                                setFolder(i.id);
                                setItems([]);
                              }}
                            >
                              {i.name}
                            </button>
                          ) : (
                            <span className="min-w-0 flex-1 truncate">{i.name}</span>
                          )}
                          {i.trashed && <span className="text-muted-foreground">{t("In the trash")}</span>}
                          {i.kind === "file" && <span className="text-muted-foreground tabular-nums">{formatBytes(i.size)}</span>}
                        </li>
                      ))}
                  </ul>
                  <p className="border-t px-2 py-1.5 text-xs text-muted-foreground">
                    {items.length ? t("{n} item chosen|{n} items chosen", { n: items.length }) : t("Nothing ticked: everything in this folder")}
                  </p>
                </div>
              )}
            </fieldset>
          )}
          <fieldset className="grid gap-2 text-sm">
            <legend className="mb-1 font-medium">{t("Where")}</legend>
            <label className="flex items-center gap-2">
              <input type="radio" name="restore-where" checked={mode === "new_folder"} onChange={() => setMode("new_folder")} />
              {t('Into a new folder, "{folder}"', { folder: folderName })}
            </label>
            <label className="flex items-center gap-2">
              <input type="radio" name="restore-where" disabled={!p?.original} checked={mode === "original"} onChange={() => setMode("original")} />
              {t("Back where it was")}
              {p && !p.original && <span className="text-xs text-muted-foreground">{t("(the space is no longer there)")}</span>}
            </label>
            {mode === "new_folder" && !personal && p && (
              <NativeSelect aria-label={t("Restore into")} value={p.target_drive ?? ""} onChange={(e) => setTarget(e.target.value)}>
                <option value="" disabled>
                  {t("Choose a space")}
                </option>
                {p.targets.map((d) => (
                  <option key={d.id} value={d.id}>
                    {d.name}
                    {d.id === p.space.id ? ` (${t("the same space")})` : ""}
                  </option>
                ))}
              </NativeSelect>
            )}
            {mode === "new_folder" && personal && p?.target_name && (
              <p className="text-xs text-muted-foreground">{t("It goes back into {name}'s personal space.", { name: p.space.owner })}</p>
            )}
            {mode === "original" && (
              <div className="grid gap-1 pl-6">
                <p className="text-xs text-muted-foreground">{t("Folders still there are used; missing ones are made again. For a file whose name is taken:")}</p>
                {(
                  [
                    ["skip", t("Skip it: keep the file that is there")],
                    ["keep", t("Keep both: the restored one gets a number")],
                    ["replace", t("Replace its content: what it has now is kept as an earlier version")],
                  ] as const
                ).map(([value, label]) => (
                  <label key={value} className="flex items-center gap-2">
                    <input type="radio" name="restore-conflict" checked={onConflict === value} onChange={() => setOnConflict(value)} />
                    {label}
                  </label>
                ))}
              </div>
            )}
          </fieldset>
          <label className="flex items-center gap-2 text-sm">
            <Checkbox checked={trash} onCheckedChange={(v) => setTrash(v === true)} />
            {t("Also restore what was in the trash")}
          </label>
          {preview.isLoading && (
            <div className="flex items-center gap-2 text-xs text-muted-foreground">
              <Loader2Icon className="size-3.5 animate-spin" /> {t("Loading…")}
            </div>
          )}
          {preview.error && <ErrorText>{errorMessage(preview.error, t("Operation failed"))}</ErrorText>}
          {p && !p.problem && (
            <div className="grid gap-1 rounded-md bg-muted/60 px-3 py-2 text-xs">
              <p>{t("{n} file to restore, {size}.|{n} files to restore, {size}.", { n: p.files, size: formatBytes(p.bytes) })}</p>
              {p.conflicts !== null &&
                (p.conflicts > 0 ? (
                  <p className="text-amber-700 dark:text-amber-300">
                    {p.checked < p.files
                      ? t("Of the first {checked}, {n} have an item where they go.", { checked: p.checked, n: p.conflicts })
                      : t("{n} of them have an item where they go.", { n: p.conflicts })}
                  </p>
                ) : (
                  <p>{t("Nothing is in their way.")}</p>
                ))}
              <p className="text-muted-foreground">{t("Restored items take the permissions of where they go. Earlier versions and the access recorded in the backup aren't restored.")}</p>
              {personal && <p className="text-muted-foreground">{t("Administrators don't see the files of personal spaces: the whole space is restored, and only its owner sees it.")}</p>}
            </div>
          )}
          {p?.problem && <ErrorText>{tServer(p.problem)}</ErrorText>}
          <ErrorText>{error}</ErrorText>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={busy || !p || !!p.problem || (mode === "new_folder" && !p.target_drive)}>
              {busy && <Loader2Icon className="animate-spin" />}
              {t("Restore")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
