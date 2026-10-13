import { useState } from "react";
import { useQuery, useQueryClient, useMutation } from "@tanstack/react-query";
import { DatabaseIcon } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import type { S3Bucket, S3Key } from "@/api/s3";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { DropdownMenuItem } from "@/components/ui/dropdown-menu";
import { ConfirmDialog } from "@/components/dialogs";
import { ErrorState } from "@/components/ErrorState";
import { t } from "@/lib/i18n";
import { formatDateTime } from "@/lib/utils";

export const s3FolderQuery = (nodeId: string) => ({ queryKey: ["s3-buckets", nodeId], queryFn: ({ signal }: { signal: AbortSignal }) => api.s3Buckets(nodeId, undefined, signal) });

function KeyForm({ bucket, close }: { bucket: S3Bucket; close(): void }) {
  const qc = useQueryClient();
  const [description, setDescription] = useState("");
  const [writable, setWritable] = useState(false);
  const [expiry, setExpiry] = useState("");
  const [created, setCreated] = useState<(S3Key & { secret_key: string }) | null>(null);
  const make = useMutation({
    mutationFn: () => api.newS3Key(bucket.id, description.trim(), writable, expiry ? Math.floor(new Date(expiry).getTime() / 1000) : null),
    onSuccess: (key) => {
      setCreated(key);
      void qc.invalidateQueries({ queryKey: ["s3-buckets"] });
    },
    onError: (e) => toast.error(e.message),
  });
  return (
    <Dialog open onOpenChange={(open) => !open && close()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("New S3 key")}</DialogTitle>
          <DialogDescription>{bucket.name}</DialogDescription>
        </DialogHeader>
        {created ? (
          <div className="grid gap-3">
            <p className="text-sm">{t("Save the Secret Key now. It is shown only once.")}</p>
            <Label>
              {t("Access Key")}
              <Input readOnly value={created.access_key} className="mt-2 font-mono" />
            </Label>
            <Label>
              {t("Secret Key")}
              <Input readOnly value={created.secret_key} className="mt-2 font-mono" autoComplete="off" />
            </Label>
            <Button
              onClick={async () => {
                try {
                  await navigator.clipboard.writeText(`AWS_ACCESS_KEY_ID=${created.access_key}\nAWS_SECRET_ACCESS_KEY=${created.secret_key}`);
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
              make.mutate();
            }}
          >
            <Label>
              {t("Key description")}
              <Input className="mt-2" value={description} maxLength={160} onChange={(e) => setDescription(e.target.value)} required />
            </Label>
            <Label className="flex items-center gap-2">
              <input type="checkbox" checked={writable} onChange={(e) => setWritable(e.target.checked)} />
              {t("Read and write")}
            </Label>
            <p className="text-xs text-muted-foreground">{t("Leave unchecked for read-only access. Read-only spaces accept read-only keys only.")}</p>
            <Label>
              {t("End date (optional)")}
              <Input className="mt-2" type="datetime-local" value={expiry} onChange={(e) => setExpiry(e.target.value)} />
            </Label>
            <Button type="submit" disabled={make.isPending || !description.trim()}>
              {make.isPending ? t("Creating…") : t("Create key")}
            </Button>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}

export function BucketCard({ bucket }: { bucket: S3Bucket }) {
  const qc = useQueryClient();
  const [newKey, setNewKey] = useState(false);
  const [remove, setRemove] = useState<S3Key | "bucket" | null>(null);
  const changed = () => qc.invalidateQueries({ queryKey: ["s3-buckets"] });
  const toggle = useMutation({ mutationFn: () => api.setS3Bucket(bucket.id, !bucket.enabled), onSuccess: changed, onError: (e) => toast.error(e.message) });
  const toggleKey = useMutation({ mutationFn: (key: S3Key) => api.setS3Key(bucket.id, key.id, !key.enabled), onSuccess: changed, onError: (e) => toast.error(e.message) });
  return (
    <section className="grid gap-3 rounded-lg border p-4" aria-label={bucket.name}>
      <div className="flex flex-wrap items-center gap-2">
        <h3 className="min-w-0 flex-1 font-medium [overflow-wrap:anywhere]">{bucket.name}</h3>
        <span className="text-xs text-muted-foreground">{bucket.active ? t("Active") : t("Stopped")}</span>
      </div>
      <p className="text-xs text-muted-foreground">
        {t("Creator")}: {bucket.creator} · {t("Folder")}: {bucket.folder} · {t("Last used")}: {bucket.last_used_at ? formatDateTime(bucket.last_used_at) : t("Never")}
      </p>
      <div className="flex flex-wrap gap-2">
        <Button size="sm" variant="outline" disabled={toggle.isPending} onClick={() => toggle.mutate()}>
          {bucket.enabled ? t("Stop bucket") : t("Enable bucket")}
        </Button>
        <Button size="sm" variant="outline" onClick={() => setNewKey(true)}>
          {t("New S3 key")}
        </Button>
        <Button size="sm" variant="outline" onClick={() => setRemove("bucket")}>
          {t("Delete bucket")}
        </Button>
      </div>
      <div className="grid gap-2">
        {bucket.keys.length === 0 && <p className="text-sm text-muted-foreground">{t("No Access Keys yet")}</p>}
        {bucket.keys.map((key) => (
          <div key={key.id} className="grid gap-1 rounded border p-3 text-xs">
            <strong className="[overflow-wrap:anywhere]">{key.description}</strong>
            <code className="[overflow-wrap:anywhere]">{key.access_key}</code>
            <span>
              {key.writable ? t("Read and write") : t("Read-only")} · {key.enabled ? t("Enabled") : t("Stopped")}
            </span>
            <span>
              {t("End date")}: {key.expires_at ? formatDateTime(key.expires_at) : t("No end date")}
            </span>
            <span>
              {t("Last used")}: {key.last_used_at ? formatDateTime(key.last_used_at) : t("Never")}
            </span>
            <div className="flex gap-2">
              <Button size="sm" variant="outline" disabled={toggleKey.isPending} onClick={() => toggleKey.mutate(key)}>
                {key.enabled ? t("Revoke key") : t("Enable key")}
              </Button>
              <Button size="sm" variant="outline" onClick={() => setRemove(key)}>
                {t("Delete key")}
              </Button>
            </div>
          </div>
        ))}
      </div>
      {newKey && <KeyForm bucket={bucket} close={() => setNewKey(false)} />}
      {remove && (
        <ConfirmDialog
          title={remove === "bucket" ? t("Delete bucket?") : t("Delete key?")}
          description={t("Programs using these credentials will lose access. Files stay in their folder.")}
          confirmText={t("Delete")}
          destructive
          onClose={() => setRemove(null)}
          onConfirm={async () => {
            if (remove === "bucket") await api.deleteS3Bucket(bucket.id);
            else await api.deleteS3Key(bucket.id, remove.id);
            await changed();
            setRemove(null);
          }}
        />
      )}
    </section>
  );
}

export function S3BucketDialog({ nodeId, onClose }: { nodeId: string; onClose(): void }) {
  const qc = useQueryClient();
  const q = useQuery(s3FolderQuery(nodeId));
  const options = useQuery({ queryKey: ["s3-options"], queryFn: ({ signal }) => api.s3Options(signal) });
  const [name, setName] = useState("");
  const publish = useMutation({
    mutationFn: () => api.publishS3Bucket(nodeId, name.trim()),
    onSuccess: () => {
      setName("");
      void qc.invalidateQueries({ queryKey: ["s3-buckets"] });
    },
    onError: (e) => toast.error(e.message),
  });
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>{t("S3 buckets")}</DialogTitle>
          <DialogDescription>{t("Programs reach this folder with its own Access Keys. Buckets follow the folder when it is renamed or moved.")}</DialogDescription>
        </DialogHeader>
        {q.error ? (
          <ErrorState message={q.error.message} onRetry={() => q.refetch()} />
        ) : !q.data ? (
          <p role="status">{t("Loading…")}</p>
        ) : (
          <>
            {options.data && (
              <p className="text-xs [overflow-wrap:anywhere]">
                {t("Endpoint")}: https://{options.data.hostname} · {t("Region")}: {options.data.region} · {t("Use path-style URLs")}
              </p>
            )}
            {q.data.can_publish ? (
              <form
                className="flex gap-2"
                onSubmit={(e) => {
                  e.preventDefault();
                  publish.mutate();
                }}
              >
                <Input
                  aria-label={t("Bucket name")}
                  placeholder={t("Bucket name")}
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  minLength={3}
                  maxLength={63}
                  pattern="[a-z0-9][a-z0-9-]*[a-z0-9]"
                  required
                />
                <Button type="submit" disabled={publish.isPending}>
                  {t("Publish")}
                </Button>
              </form>
            ) : (
              <p>{t("This folder can't be published as an S3 bucket")}</p>
            )}
            {q.data.buckets.map((bucket) => (
              <BucketCard key={bucket.id} bucket={bucket} />
            ))}
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}

export function PublishBucketMenu({ nodeId, onPublish }: { nodeId: string; onPublish(): void }) {
  const q = useQuery(s3FolderQuery(nodeId));
  return q.data?.can_publish ? (
    <DropdownMenuItem onClick={onPublish}>
      <DatabaseIcon />
      {t("Publish as S3 bucket…")}
    </DropdownMenuItem>
  ) : null;
}

export function S3FolderSection({ nodeId }: { nodeId: string }) {
  const q = useQuery(s3FolderQuery(nodeId));
  const [open, setOpen] = useState(false);
  if (!q.data || (!q.data.can_publish && q.data.buckets.length === 0)) return null;
  return (
    <section className="grid gap-2 border-t pt-3">
      <h3 className="text-xs font-medium">{t("S3 buckets")}</h3>
      {q.data.buckets.map((bucket) => (
        <p key={bucket.id} className="text-xs [overflow-wrap:anywhere]">
          {bucket.name} · {bucket.keys.length} {t("Access Keys")}
        </p>
      ))}
      <Button size="sm" variant="outline" onClick={() => setOpen(true)}>
        {t("Manage buckets and keys")}
      </Button>
      {open && <S3BucketDialog nodeId={nodeId} onClose={() => setOpen(false)} />}
    </section>
  );
}
