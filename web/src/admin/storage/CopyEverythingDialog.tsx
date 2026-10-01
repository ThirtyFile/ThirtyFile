import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangleIcon, Loader2Icon } from "lucide-react";
import { useNavigate } from "react-router";
import { toast } from "sonner";
import { api, type StorageLocation } from "@/api";
import { keys, queries } from "@/api/queryKeys";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ErrorText } from "@/components/dialogs";
import { locationLabel } from "@/components/LocationSelect";
import { DRIVE_ICON } from "@/lib/drives";
import { t, tServer } from "@/lib/i18n";
import { formatBytes, errorMessage } from "@/lib/utils";
import { useSubmit } from "@/lib/useSubmit";
import { NativeSelect } from "@/components/ui/native-select";

/**
 * "Copy everything to…": the spaces of a location copied to another location, which keeps the copy in a folder of its
 * own. The dialog says what is copied, where it goes and how it is used, before anything starts.
 */
export function CopyEverythingDialog({ location, onClose }: { location: StorageLocation; onClose(): void }) {
  const navigate = useNavigate();
  const locations = useQuery(queries.storageLocations);
  const targets = (locations.data ?? []).filter((l) => l.id !== location.id);
  const [dest, setDest] = useState("");
  const [name, setName] = useState(() => t("Copy of {name}", { name: location.name }));
  const preview = useQuery({
    queryKey: keys.copyPreview(location.id, dest),
    queryFn: () => api.copyPreview(location.id, dest),
    enabled: !!dest,
    retry: false,
  });
  const p = preview.data;
  const tooSmall = p?.free_bytes != null && p.free_bytes < p.content_bytes;
  const destName = targets.find((l) => l.id === dest)?.name ?? "";
  const { busy, error, run } = useSubmit(async () => {
    if (!dest) return;
    await api.startCopy(location.id, dest, name.trim());
    toast.success(t('"{name}" is being made in the background', { name: name.trim() }), {
      action: { label: t("Show copies"), onClick: () => navigate("/admin/backups") },
    });
    onClose();
  }, t("Couldn't make the change"));
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <form className="grid gap-4" onSubmit={run}>
          <DialogHeader>
            <DialogTitle>{t('Copy everything on "{name}"', { name: location.name })}</DialogTitle>
            <DialogDescription>
              {t(
                "Its spaces are copied to the location you choose, with their trash and earlier versions. Nothing on {name} changes: no file is removed, the spaces stay on it, and the default location stays as it is.",
                {
                  name: location.name,
                },
              )}
            </DialogDescription>
          </DialogHeader>
          <div className="grid gap-2">
            <Label htmlFor="copy-target">{t("Copy to")}</Label>
            <NativeSelect id="copy-target" size="lg" value={dest} onChange={(e) => setDest(e.target.value)}>
              <option value="" disabled>
                {t("Choose a location")}
              </option>
              {targets.map((l) => (
                <option key={l.id} value={l.id} disabled={!l.connected}>
                  {l.disk_free_bytes != null && l.connected ? t("{location} · {free} free", { location: locationLabel(l), free: formatBytes(l.disk_free_bytes) }) : locationLabel(l)}
                </option>
              ))}
            </NativeSelect>
          </div>
          <div className="grid gap-2">
            <Label htmlFor="copy-name">{t("Name of the copy")}</Label>
            <Input id="copy-name" value={name} maxLength={200} onChange={(e) => setName(e.target.value)} />
          </div>
          {dest && preview.isLoading && (
            <div className="flex items-center gap-2 text-xs text-muted-foreground">
              <Loader2Icon className="size-3.5 animate-spin" /> {t("Counting what there is to copy…")}
            </div>
          )}
          {preview.error && <ErrorText>{errorMessage(preview.error, t("Operation failed"))}</ErrorText>}
          {p && (
            <div className="grid gap-3 text-xs">
              <div className="grid gap-1">
                <p className="font-medium text-foreground">{t("What is copied")}</p>
                <ul className="max-h-32 divide-y overflow-y-auto rounded-md border">
                  {p.spaces.map((s) => {
                    const Icon = DRIVE_ICON[s.kind];
                    return (
                      <li key={s.id} className="flex items-center gap-2 px-2.5 py-1">
                        <Icon className="size-3.5 shrink-0 text-muted-foreground" />
                        <span className="min-w-0 flex-1 truncate">{s.kind === "personal" && s.owner_name ? `${s.name} · ${s.owner_name}` : s.name}</span>
                        <span className="text-muted-foreground tabular-nums">{formatBytes(s.used_bytes)}</span>
                      </li>
                    );
                  })}
                </ul>
                <p className="text-muted-foreground">
                  {t("{n} file, of which {trash} in the trash.|{n} files, of which {trash} in the trash.", { n: p.files, trash: p.trash_files })}{" "}
                  {t("{n} earlier version.|{n} earlier versions.", { n: p.versions })}{" "}
                  {t("{size} in all; identical content is stored once, so {content} go to {name}.", {
                    size: formatBytes(p.bytes),
                    content: formatBytes(p.content_bytes),
                    name: destName,
                  })}
                </p>
                <p className="text-muted-foreground">
                  {p.free_bytes != null ? t("{free} is free there.", { free: formatBytes(p.free_bytes) }) : t("How much room is left there can't be told: the storage service doesn't say.")}
                </p>
              </div>
              <div className="grid gap-1 text-muted-foreground">
                <p className="font-medium text-foreground">{t("Where it goes, and how it is used")}</p>
                <p>
                  {t(
                    "The copy gets a folder of its own on {name} (in .thirtyfile-backups), which nothing else writes to: nothing there is replaced, and each copy is kept apart. It is listed in Control panel › Backups, where a space of it can be restored into a new folder, and where it can be checked or deleted.",
                    {
                      name: destName,
                    },
                  )}
                </p>
                <p>
                  {t(
                    "The spaces stay usable meanwhile. The copy shows them as they were when their list of files was made: files changed or deleted after that are copied as they were. Content-store files are kept from deletion until they are copied; files in folders are read and checked unchanged.",
                  )}
                </p>
                <p>{t("Share links, favorites and upload sessions aren't copied. Who had access is recorded, but restoring doesn't give it again.")}</p>
              </div>
              {(p.shared || p.unencrypted || tooSmall || p.problem) && (
                <div className="grid gap-1 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-amber-900 dark:text-amber-100" role="note">
                  {p.problem && (
                    <p className="flex gap-1.5">
                      <AlertTriangleIcon className="mt-px size-3.5 shrink-0" /> {t("{name} can't be reached now: {error}", { name: destName, error: tServer(p.problem) })}
                    </p>
                  )}
                  {tooSmall && <p>{t("There isn't enough free space there for the copy.")}</p>}
                  {p.shared && <p>{t("{name} is on the same disk or storage service as {source}: the copy doesn't survive a failure of it.", { name: destName, source: location.name })}</p>}
                  {p.unencrypted && <p>{t("{name} is FTP without encryption: the files travel unencrypted.", { name: destName })}</p>}
                </div>
              )}
            </div>
          )}
          <ErrorText>{error}</ErrorText>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={busy || !p || !!p.problem || tooSmall || !name.trim() || p.spaces.length === 0}>
              {busy && <Loader2Icon className="animate-spin" />}
              {t("Start copying")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
