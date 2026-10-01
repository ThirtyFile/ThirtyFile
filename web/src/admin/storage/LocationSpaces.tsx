//! The spaces on a storage location: the list of them, and moving all of them to another location

import { useQuery } from "@tanstack/react-query";
import { Loader2Icon, Trash2Icon } from "lucide-react";
import { moveActive, type SpaceMove, type StorageLocation } from "@/api";
import { queries } from "@/api/queryKeys";
import { MoveDialog } from "@/admin/storage/MoveDialog";
import { DRIVE_ICON, DRIVE_KIND_LABEL } from "@/lib/drives";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { ErrorText } from "@/components/dialogs";
import { formatBytes, errorMessage } from "@/lib/utils";
import { t } from "@/lib/i18n";

/** "Move everything to…": every space on a location, moved to another one (a move each, one after the other) */
export function MoveEverythingDialog({ location, onClose, onDone }: { location: StorageLocation; onClose(): void; onDone(): void }) {
  const q = useQuery(queries.storageLocationSpaces(location.id));
  if (!q.data) {
    return q.error ? (
      <Dialog open onOpenChange={(o) => !o && onClose()}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('Move everything on "{name}"', { name: location.name })}</DialogTitle>
          </DialogHeader>
          <ErrorText>{errorMessage(q.error, t("Operation failed"))}</ErrorText>
        </DialogContent>
      </Dialog>
    ) : null;
  }
  const spaces = q.data.map((s) => ({
    id: s.id,
    label: s.kind === "personal" && s.owner_name ? `${s.name} · ${s.owner_name}` : s.name,
    mode: s.mode,
    location_id: location.id,
    source_path: s.source_path,
    used_bytes: s.used_bytes,
  }));
  return (
    <MoveDialog
      spaces={spaces}
      title={t('Move everything on "{name}"', { name: location.name })}
      description={t(
        "Its space ({size}) is moved to the location you choose. Once it's there, this location can be deleted.|Each of its {n} spaces ({size}) is moved to the location you choose, one after the other. Once they're all there, this location can be deleted.",
        {
          n: spaces.length,
          size: formatBytes(spaces.reduce((n, s) => n + s.used_bytes, 0)),
        },
      )}
      from={location.id}
      onClose={onClose}
      onDone={onDone}
    />
  );
}

/**
 * Under a location its spaces are being moved off: how far that is; once nothing is left on it, the offer to make the
 * location they went to the default and to delete this one
 */
export function MovedOff({
  location: l,
  moves,
  list,
  onDefault,
  onDelete,
}: {
  location: StorageLocation;
  moves: SpaceMove[];
  list: StorageLocation[];
  onDefault(l: StorageLocation): void;
  onDelete(): void;
}) {
  const active = moves.filter(moveActive);
  if (active.length > 0) {
    const done = moves.filter((m) => m.state === "done" && m.finished_at && m.finished_at >= Math.min(...active.map((a) => a.created_at))).length;
    return <div className="mt-0.5 text-xs text-brand">{t("Moving its spaces to other locations: {done} of {total} done", { done, total: active.length + done })}</div>;
  }
  const last = moves.find((m) => m.state === "done");
  if (!last || l.builtin || l.drive_count > 0 || l.blob_count > 0) return null;
  const to = list.find((x) => x.id === last.to_location);
  // The old copies go a minute after each space switched: deleting the location before would leave them there
  const clearing = l.pending_deletes > 0;
  return (
    <div className="mt-1 flex flex-wrap items-center gap-2 text-xs">
      <span className="text-muted-foreground">
        {clearing ? t("Its spaces were moved. The old copies are being removed from here; then it can be deleted.") : t("Its spaces were moved; nothing uses this location any more.")}
      </span>
      {l.is_default && to && (
        <Button
          size="xs"
          variant="outline"
          onClick={(e) => {
            e.stopPropagation();
            onDefault(to);
          }}
        >
          {t('Make "{name}" the default', { name: to.name })}
        </Button>
      )}
      {!l.is_default && !clearing && (
        <Button
          size="xs"
          variant="outline"
          onClick={(e) => {
            e.stopPropagation();
            onDelete();
          }}
        >
          <Trash2Icon /> {t("Delete this location")}
        </Button>
      )}
    </div>
  );
}

/** The spaces on a location: name, kind, owner and size (nothing of what is in them) */
export function SpacesDialog({ location, onClose }: { location: StorageLocation; onClose(): void }) {
  const q = useQuery(queries.storageLocationSpaces(location.id));
  const spaces = q.data ?? [];
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t('Spaces on "{name}"', { name: location.name })}</DialogTitle>
          <DialogDescription>{t("Spaces stay on the location they were created on until they're moved.")}</DialogDescription>
        </DialogHeader>
        {q.isLoading ? (
          <div className="flex h-20 items-center justify-center text-muted-foreground">
            <Loader2Icon className="size-5 animate-spin" />
          </div>
        ) : q.error ? (
          <ErrorText>{errorMessage(q.error, t("Operation failed"))}</ErrorText>
        ) : spaces.length === 0 ? (
          <p className="py-4 text-center text-sm text-muted-foreground">{t("No spaces are on this location.")}</p>
        ) : (
          <ul className="max-h-80 divide-y overflow-y-auto rounded-md border text-sm">
            {spaces.map((s) => {
              const Icon = DRIVE_ICON[s.kind];
              return (
                <li key={s.id} className="flex items-center gap-2.5 px-3 py-2">
                  <Icon className="size-4 shrink-0 text-muted-foreground" />
                  <div className="min-w-0 flex-1">
                    <div className="truncate">{s.kind === "personal" && s.owner_name ? `${s.name} · ${s.owner_name}` : s.name}</div>
                    <div className="truncate text-xs text-muted-foreground">
                      {DRIVE_KIND_LABEL[s.kind]}
                      {s.mode === "folder" && ` · ${t("Folder on the server")}`}
                    </div>
                  </div>
                  <span className="shrink-0 text-xs text-muted-foreground tabular-nums">{formatBytes(s.used_bytes)}</span>
                </li>
              );
            })}
          </ul>
        )}
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {t("Close")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
