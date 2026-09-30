import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Loader2Icon } from "lucide-react";
import { useNavigate } from "react-router";
import { toast } from "sonner";
import { api, moveActive, type StorageLocation } from "@/api";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { ErrorText } from "@/components/dialogs";
import { locationLabel } from "@/components/LocationSelect";
import { t } from "@/lib/i18n";
import { formatBytes } from "@/lib/utils";
import { NativeSelect } from "@/components/ui/native-select";

/** A space as moving it needs it */
export interface MovableSpace {
  id: string;
  /** As lists name it ("My files · amy" for personal spaces) */
  label: string;
  mode: "store" | "folder";
  location_id: string | null;
  used_bytes: number;
  /** Folder spaces, when known: their folder */
  source_path?: string | null;
}

/** How a space keeps its files on a location: the built-in storage and Local folder locations keep folders */
const modeOn = (l: StorageLocation) => (l.kind === "local" ? "folder" : "store");

/**
 * Moving one or more spaces to another storage location: where they go, how much room they need there, and what
 * happens meanwhile. `from`: the location they are all on, left out of the targets.
 */
export function MoveDialog({
  spaces,
  title,
  description,
  from,
  onClose,
  onDone,
}: {
  spaces: MovableSpace[];
  title: string;
  description: string;
  from?: string | null;
  onClose(): void;
  onDone(target: StorageLocation, moved: number): void;
}) {
  const navigate = useNavigate();
  const locations = useQuery({ queryKey: ["storage-locations"], queryFn: api.storageLocations });
  const moves = useQuery({ queryKey: ["moves"], queryFn: api.moves });
  const targets = (locations.data ?? []).filter((l) => l.id !== from);
  const [value, setValue] = useState<string>("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const target = targets.find((l) => l.id === value);
  // Spaces already being moved, and those already where they would go, are left out
  const moving = new Set((moves.data?.moves ?? []).filter(moveActive).map((m) => m.drive_id));
  const busyCount = spaces.filter((s) => moving.has(s.id)).length;
  const movable = spaces.filter((s) => !moving.has(s.id) && !(target && s.location_id === target.id && s.mode === modeOn(target)));
  const there = target ? spaces.length - busyCount - movable.length : 0;
  const bytes = movable.reduce((n, s) => n + s.used_bytes, 0);
  const tooSmall = target?.disk_free_bytes != null && target.disk_free_bytes < bytes;
  const fromFolder = movable.filter((s) => s.mode === "folder");
  // Files copied from or into a folder must stay as they are meanwhile
  const readOnly = fromFolder.length > 0 || target?.kind === "local";
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-md">
        <form
          className="grid gap-4"
          onSubmit={async (e) => {
            e.preventDefault();
            if (!target) return;
            setBusy(true);
            setError(null);
            try {
              await api.startMoves(
                movable.map((s) => s.id),
                target.id,
              );
              toast.success(
                movable.length === 1
                  ? t("\"{name}\" is being moved in the background", { name: movable[0].label })
                  : t("{n} spaces are being moved in the background, one after the other", { n: movable.length }),
                { action: { label: t("Show moves"), onClick: () => navigate("/admin/moves") } },
              );
              onDone(target, movable.length);
            } catch (err) {
              setError(err instanceof Error ? err.message : t("Couldn't make the change"));
            } finally {
              setBusy(false);
            }
          }}
        >
          <DialogHeader>
            <DialogTitle>{title}</DialogTitle>
            <DialogDescription>{description}</DialogDescription>
          </DialogHeader>
          <div className="grid gap-2">
            <Label htmlFor="move-target">{t("Move to")}</Label>
            <NativeSelect
              id="move-target"
              size="lg"
              value={value}
              onChange={(e) => setValue(e.target.value)}
            >
              <option value="" disabled>
                {t("Choose a location")}
              </option>
              {targets.map((l) => (
                <option key={l.id} value={l.id} disabled={!l.connected}>
                  {l.disk_free_bytes != null && l.connected ? t("{location} · {free} free", { location: locationLabel(l), free: formatBytes(l.disk_free_bytes) }) : locationLabel(l)}
                </option>
              ))}
            </NativeSelect>
            {spaces.length > 1 && target && (
              <p className="text-xs">{t("{n} space to move · {size}|{n} spaces to move · {size}", { n: movable.length, size: formatBytes(bytes) })}</p>
            )}
            {busyCount > 0 && <p className="text-xs text-muted-foreground">{t("{n} is being moved already and is left out.|{n} are being moved already and are left out.", { n: busyCount })}</p>}
            {there > 0 && <p className="text-xs text-muted-foreground">{t("{n} is there already and stays.|{n} are there already and stay.", { n: there })}</p>}
            {tooSmall && <p className="text-xs text-destructive">{t("There isn't enough free space there for this space.")}</p>}
            {readOnly ? (
              <div className="grid gap-1.5 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs text-amber-900 dark:text-amber-100" role="note">
                <p>
                  {t("The space is read-only while its files are copied: people can open, download and share files, but not change them. It switches to the new location once all of them are there.")}
                </p>
                {fromFolder.length === 1 && movable.length === 1 && fromFolder[0].source_path && (
                  <p>
                    {t("Changes made in its folder from outside ThirtyFile meanwhile are copied too. Afterwards the folder {path} is removed, apart from anything that changed at the last moment.", {
                      path: fromFolder[0].source_path ?? "",
                    })}
                  </p>
                )}
                {fromFolder.length > 0 && (movable.length > 1 || !fromFolder[0].source_path) && (
                  <p>{t("Changes made in the folders of folder spaces from outside ThirtyFile meanwhile are copied too. Afterwards their folders are removed, apart from anything that changed at the last moment.")}</p>
                )}
                {target?.kind === "local" && <p>{t("There it gets a folder of its own, like a new space's, with its files as ordinary files.")}</p>}
              </div>
            ) : (
              <p className="text-xs text-muted-foreground">
                {t("The space stays usable while its files are copied, and switches to the new location once all of them are there. The copies are checked before the old files are removed.")}
              </p>
            )}
            <p className="text-xs text-muted-foreground">{t("Files with identical content are stored only once across the system. If other spaces have the same files, they'll be moved too.")}</p>
            <ErrorText>{error}</ErrorText>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={busy || !target || tooSmall || movable.length === 0}>
              {busy && <Loader2Icon className="animate-spin" />}
              {t("Move")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
