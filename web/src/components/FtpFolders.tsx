import { useId, useState } from "react";
import { useQuery, useQueryClient, useMutation } from "@tanstack/react-query";
import { FolderInputIcon } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import type { FtpChange, FtpCreated, FtpFolder } from "@/api/ftp";
import { useConfirmIdentity } from "@/components/ConfirmIdentity";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { DropdownMenuItem } from "@/components/ui/dropdown-menu";
import { ErrorState } from "@/components/ErrorState";
import { ErrorText } from "@/components/dialogs";
import { t } from "@/lib/i18n";
import { formatDateTime } from "@/lib/utils";

export const ftpFolderQuery = (nodeId?: string) => ({ queryKey: ["ftp-folders", nodeId ?? "admin"], queryFn: ({ signal }: { signal: AbortSignal }) => api.ftpFolders(nodeId, signal) });
const recentNote = () => t("Sign in again before managing FTP credentials if your sign-in is older than 10 minutes.");

function EditDialog({ nodeId, folder, action = "edit", close }: { nodeId: string; folder?: FtpFolder; action?: "edit" | "rotate" | "delete"; close(): void }) {
  const id = useId();
  const identity = useConfirmIdentity(id);
  const qc = useQueryClient();
  const [draft, setDraft] = useState<FtpChange>(folder ?? { name: "", enabled: true, allow_list: false, allow_read: false, expires_at: null });
  const [expiry, setExpiry] = useState(() => {
    if (!folder?.expires_at) return "";
    const d = new Date(folder.expires_at * 1000);
    return new Date(d.getTime() - d.getTimezoneOffset() * 60000).toISOString().slice(0, 16);
  });
  const [created, setCreated] = useState<FtpCreated | null>(null);
  const save = useMutation({
    mutationFn: async () => {
      if (folder && action === "rotate") return api.rotateFtpPassword(folder.id, identity.values);
      if (folder && action === "delete") {
        await api.deleteFtpFolder(folder.id, identity.values);
        return null;
      }
      const config = { ...draft, expires_at: expiry ? Math.floor(new Date(expiry).getTime() / 1000) : null };
      if (folder) {
        await api.saveFtpFolder(folder.id, config, identity.values);
        return null;
      }
      return api.createFtpFolder(nodeId, config, identity.values);
    },
    onSuccess: (result) => {
      identity.clear();
      void qc.invalidateQueries({ queryKey: ["ftp-folders"] });
      if (result) setCreated(result);
      else close();
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
            <Label className="grid gap-2">
              {t("Device password")}
              <Input readOnly value={created.password} className="font-mono" autoComplete="off" />
            </Label>
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
                  <input type="checkbox" checked={draft.enabled} onChange={(e) => setDraft({ ...draft, enabled: e.target.checked })} />
                  {t("Enabled")}
                </Label>
                <Label className="flex gap-2">
                  <input type="checkbox" checked={draft.allow_list} onChange={(e) => setDraft({ ...draft, allow_list: e.target.checked })} />
                  {t("Allow device to list files")}
                </Label>
                <Label className="flex gap-2">
                  <input type="checkbox" checked={draft.allow_read} onChange={(e) => setDraft({ ...draft, allow_read: e.target.checked })} />
                  {t("Allow device to download files")}
                </Label>
                <p className="text-xs text-muted-foreground">{t("Uploads are always allowed while the receive folder is active. Devices cannot delete, move or rename files.")}</p>
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
  return (
    <section className="grid gap-3 rounded-lg border p-4" aria-label={folder.name}>
      <div className="flex flex-wrap gap-2">
        <h3 className="min-w-0 flex-1 font-medium [overflow-wrap:anywhere]">{folder.name}</h3>
        <span className="text-xs text-muted-foreground">{folder.active ? t("Active") : t("Stopped")}</span>
      </div>
      <p className="text-xs [overflow-wrap:anywhere]">
        {t("Creator")}: {folder.creator} · {t("Folder")}: {folder.folder_name}
      </p>
      <p className="text-xs [overflow-wrap:anywhere]">
        {t("Device username")}: <code>{folder.username}</code>
      </p>
      <p className="text-xs">
        {folder.allow_list ? t("Listing allowed") : t("Listing blocked")} · {folder.allow_read ? t("Downloads allowed") : t("Downloads blocked")}
      </p>
      <p className="text-xs">
        {t("End date")}: {folder.expires_at ? formatDateTime(folder.expires_at) : t("No end date")} · {t("Last used")}:{" "}
        {folder.last_used_at ? formatDateTime(folder.last_used_at) : t("Never")}
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
      {action && <EditDialog folder={folder} nodeId={folder.node_id} action={action} close={() => setAction(null)} />}
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
          <DialogDescription>{t("Give each device its own credentials for this folder. Uploads replace files with the same name and keep earlier versions.")}</DialogDescription>
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
          {f.name} · {f.active ? t("Active") : t("Stopped")}
        </p>
      ))}
      <Button size="sm" variant="outline" onClick={() => setOpen(true)}>
        {t("Manage receive folders")}
      </Button>
      {open && <FtpFolderDialog nodeId={nodeId} onClose={() => setOpen(false)} />}
    </section>
  );
}
