import { useId, useRef, useState } from "react";
import { useQuery, useQueryClient, useMutation } from "@tanstack/react-query";
import { FolderInputIcon } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import type { FtpChange, FtpCreated, FtpFolder } from "@/api/ftp";
import { keys } from "@/api/queryKeys";
import { focusList } from "@/components/S3Buckets";
import { useConfirmIdentity } from "@/components/ConfirmIdentity";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { SecretInput } from "@/components/SecretField";
import { Label } from "@/components/ui/label";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { DropdownMenuItem } from "@/components/ui/dropdown-menu";
import { ErrorState } from "@/components/ErrorState";
import { ErrorText } from "@/components/dialogs";
import { t } from "@/lib/i18n";
import { formatDateTime } from "@/lib/utils";

export const ftpFolderQuery = (nodeId?: string, search = "") => ({
  queryKey: keys.receiveFolders(nodeId ?? null, search),
  queryFn: ({ signal }: { signal: AbortSignal }) => api.ftpFolders(nodeId, search || undefined, signal),
});

/** What a receive folder's state is now: stopped, past its end date, working, or on but not working */
function folderState(folder: FtpFolder) {
  if (!folder.enabled) return t("Stopped");
  if (folder.expires_at && folder.expires_at * 1000 <= Date.now()) return t("Ended");
  return folder.active ? t("Active") : t("Not active: FTP is off, or its creator can't publish this folder any more");
}

/** The time as the browser's end-date field shows it (local, to the minute) */
function localMinute(seconds: number) {
  const d = new Date(seconds * 1000);
  return new Date(d.getTime() - d.getTimezoneOffset() * 60000).toISOString().slice(0, 16);
}
const recentNote = () => t("Sign in again before managing FTP credentials if your sign-in is older than 10 minutes.");

function EditDialog({ nodeId, folder, action = "edit", close, deleted }: { nodeId: string; folder?: FtpFolder; action?: "edit" | "rotate" | "delete"; close(): void; deleted?(): void }) {
  const id = useId();
  const identity = useConfirmIdentity(id);
  const qc = useQueryClient();
  // Only what can be changed is sent, not the rest of the receive folder
  const [draft, setDraft] = useState<FtpChange>(() =>
    folder
      ? { name: folder.name, enabled: folder.enabled, allow_list: folder.allow_list, allow_read: folder.allow_read, replace_existing: folder.replace_existing, expires_at: folder.expires_at }
      : { name: "", enabled: true, allow_list: false, allow_read: false, replace_existing: true, expires_at: null },
  );
  // The end date as it was opened: left so, it is sent as it is, also when it has passed (renaming or stopping an
  // ended receive folder keeps its end), and to the second
  const [initialExpiry] = useState(() => (folder?.expires_at ? localMinute(folder.expires_at) : ""));
  const [expiry, setExpiry] = useState(initialExpiry);
  const [created, setCreated] = useState<FtpCreated | null>(null);
  const save = useMutation({
    // A new device password is in the answer: kept no longer than the dialog shows it
    gcTime: 0,
    mutationFn: async () => {
      if (folder && action === "rotate") return api.rotateFtpPassword(folder.id, identity.values);
      if (folder && action === "delete") {
        await api.deleteFtpFolder(folder.id, identity.values);
        return null;
      }
      const expires_at = folder && expiry === initialExpiry ? folder.expires_at : expiry ? Math.floor(new Date(expiry).getTime() / 1000) : null;
      const config = { ...draft, expires_at };
      if (folder) {
        await api.saveFtpFolder(folder.id, config, identity.values);
        return null;
      }
      return api.createFtpFolder(nodeId, config, identity.values);
    },
    onSuccess: (result) => {
      identity.clear();
      void qc.invalidateQueries({ queryKey: keys.receiveFolderLists() });
      if (result) setCreated(result);
      else {
        close();
        if (action === "delete") deleted?.();
      }
    },
  });
  return (
    <Dialog open onOpenChange={(open) => !open && close()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>
            {action === "rotate" ? t("Rotate FTP password") : action === "delete" ? t("Delete FTP receive folder?") : folder ? t("Edit FTP receive folder") : t("Make FTP receive folder…")}
          </DialogTitle>
          <DialogDescription>{t("Programs using these credentials will lose access when they are stopped, rotated or deleted. Files stay in their folder.")}</DialogDescription>
        </DialogHeader>
        {created ? (
          <div className="grid gap-3">
            <p className="text-sm">{t("Save the device password now. It is shown only once.")}</p>
            <Label className="grid gap-2">
              {t("Device username")}
              <Input readOnly value={created.username} className="font-mono" />
            </Label>
            <div className="grid gap-2">
              <Label htmlFor="ftp-new-password">{t("Device password")}</Label>
              <SecretInput id="ftp-new-password" readOnly value={created.password} className="font-mono" autoComplete="off" revealLabel={t("Show device password")} />
            </div>
            <Button
              onClick={async () => {
                try {
                  await navigator.clipboard.writeText(`${created.username}\n${created.password}`);
                  toast.success(t("Copied"));
                } catch {
                  toast.error(t("Couldn't copy"));
                }
              }}
            >
              {t("Copy credentials")}
            </Button>
            <Button variant="outline" onClick={close}>
              {t("Done")}
            </Button>
          </div>
        ) : (
          <form
            className="grid gap-4"
            onSubmit={(e) => {
              e.preventDefault();
              save.mutate();
            }}
          >
            {action === "edit" && (
              <>
                <Label className="grid gap-2">
                  {t("Receive folder name")}
                  <Input value={draft.name} maxLength={80} onChange={(e) => setDraft({ ...draft, name: e.target.value })} required />
                </Label>
                <Label className="flex gap-2">
                  <input type="checkbox" className="size-4 accent-brand" checked={draft.enabled} onChange={(e) => setDraft({ ...draft, enabled: e.target.checked })} />
                  {t("Enabled")}
                </Label>
                <Label className="flex gap-2">
                  <input type="checkbox" className="size-4 accent-brand" checked={draft.allow_list} onChange={(e) => setDraft({ ...draft, allow_list: e.target.checked })} />
                  {t("Allow device to list files")}
                </Label>
                <Label className="flex gap-2">
                  <input type="checkbox" className="size-4 accent-brand" checked={draft.allow_read} onChange={(e) => setDraft({ ...draft, allow_read: e.target.checked })} />
                  {t("Allow device to download files")}
                </Label>
                <Label className="flex gap-2">
                  <input type="checkbox" className="size-4 accent-brand" checked={draft.replace_existing} onChange={(e) => setDraft({ ...draft, replace_existing: e.target.checked })} />
                  {t("Replace files with the same name (earlier versions are kept)")}
                </Label>
                <p className="text-xs text-muted-foreground">
                  {t("Uploads are always allowed while the receive folder is active. Devices cannot delete, move or rename files.")}{" "}
                  {t("Without replacing, a new file whose name is taken gets a number.")}
                </p>
                <Label className="grid gap-2">
                  {t("End date")}
                  <Input type="datetime-local" value={expiry} onChange={(e) => setExpiry(e.target.value)} />
                </Label>
              </>
            )}
            {identity.fields(recentNote())}
            <ErrorText>{save.error?.message}</ErrorText>
            <div className="flex flex-wrap gap-2">
              <Button type="submit" variant={action === "delete" ? "destructive" : "default"} disabled={save.isPending || !identity.ready || (action === "edit" && !draft.name.trim())}>
                {save.isPending ? t("Saving…") : action === "rotate" ? t("Rotate password") : action === "delete" ? t("Delete") : folder ? t("Save") : t("Create")}
              </Button>
              <Button type="button" variant="outline" onClick={close}>
                {t("Cancel")}
              </Button>
            </div>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}

export function FtpFolderCard({ folder }: { folder: FtpFolder }) {
  const [action, setAction] = useState<"edit" | "rotate" | "delete" | null>(null);
  const card = useRef<HTMLElement>(null);
  return (
    <section ref={card} className="grid gap-3 rounded-lg border p-4" aria-label={folder.name}>
      <div className="flex flex-wrap gap-2">
        <h3 className="min-w-0 flex-1 font-medium [overflow-wrap:anywhere]">{folder.name}</h3>
        <span className="text-xs text-muted-foreground">{folderState(folder)}</span>
      </div>
      <p className="text-xs [overflow-wrap:anywhere]">{t("Creator: {creator} · Folder: {folder}", { creator: folder.creator, folder: folder.folder_name })}</p>
      <p className="font-mono text-xs [overflow-wrap:anywhere]">{t("Device username: {username}", { username: folder.username })}</p>
      <p className="text-xs">
        {folder.allow_list ? t("Listing allowed") : t("Listing blocked")} · {folder.allow_read ? t("Downloads allowed") : t("Downloads blocked")} ·{" "}
        {folder.replace_existing ? t("Replaces files with the same name") : t("Keeps both files with the same name")}
      </p>
      <p className="text-xs">
        {t("End date: {date} · Last used: {when}", {
          date: folder.expires_at ? formatDateTime(folder.expires_at) : t("No end date"),
          when: folder.last_used_at ? formatDateTime(folder.last_used_at) : t("Never"),
        })}
      </p>
      <div className="flex flex-wrap gap-2">
        <Button size="sm" variant="outline" onClick={() => setAction("edit")}>
          {t("Edit")}
        </Button>
        <Button size="sm" variant="outline" onClick={() => setAction("rotate")}>
          {t("Rotate password")}
        </Button>
        <Button size="sm" variant="outline" onClick={() => setAction("delete")}>
          {t("Delete")}
        </Button>
      </div>
      {action && <EditDialog folder={folder} nodeId={folder.node_id} action={action} close={() => setAction(null)} deleted={() => focusList(card.current)} />}
    </section>
  );
}
export function FtpFolderDialog({ nodeId, onClose }: { nodeId: string; onClose(): void }) {
  const q = useQuery(ftpFolderQuery(nodeId));
  const [create, setCreate] = useState(false);
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>{t("FTP receive folders")}</DialogTitle>
          <DialogDescription>{t("Give each device its own credentials for this folder.")}</DialogDescription>
        </DialogHeader>
        {q.error ? (
          <ErrorState message={q.error.message} onRetry={() => q.refetch()} />
        ) : !q.data ? (
          <p role="status">{t("Loading…")}</p>
        ) : (
          <div className="grid gap-3">
            {q.data.can_publish && (
              <Button className="justify-self-start" onClick={() => setCreate(true)}>
                {t("Make FTP receive folder…")}
              </Button>
            )}
            {q.data.folders.map((folder) => (
              <FtpFolderCard key={folder.id} folder={folder} />
            ))}
            {q.data.folders.length === 0 && <p>{q.data.can_publish ? t("No FTP receive folders yet") : t("This folder can't be published as an FTP receive folder")}</p>}
          </div>
        )}
        {create && <EditDialog nodeId={nodeId} close={() => setCreate(false)} />}
      </DialogContent>
    </Dialog>
  );
}
export function FtpFolderMenu({ nodeId, onPublish }: { nodeId: string; onPublish(): void }) {
  const q = useQuery(ftpFolderQuery(nodeId));
  return q.data?.can_publish ? (
    <DropdownMenuItem onClick={onPublish}>
      <FolderInputIcon />
      {t("Make FTP receive folder…")}
    </DropdownMenuItem>
  ) : null;
}
export function FtpFolderSection({ nodeId }: { nodeId: string }) {
  const q = useQuery(ftpFolderQuery(nodeId));
  const [open, setOpen] = useState(false);
  if (!q.data || (!q.data.can_publish && q.data.folders.length === 0)) return null;
  return (
    <section className="grid gap-2 border-t pt-3">
      <h3 className="text-xs font-medium">{t("FTP receive folders")}</h3>
      {q.data.folders.map((f) => (
        <p key={f.id} className="text-xs [overflow-wrap:anywhere]">
          {f.name} · {folderState(f)}
        </p>
      ))}
      <Button size="sm" variant="outline" onClick={() => setOpen(true)}>
        {t("Manage receive folders")}
      </Button>
      {open && <FtpFolderDialog nodeId={nodeId} onClose={() => setOpen(false)} />}
    </section>
  );
}
