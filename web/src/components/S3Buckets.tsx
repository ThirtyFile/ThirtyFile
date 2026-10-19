import { useRef, useState } from "react";
import { useQuery, useQueryClient, useMutation } from "@tanstack/react-query";
import { DatabaseIcon } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { keys } from "@/api/queryKeys";
import type { S3Bucket, S3Key } from "@/api/s3";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { SecretInput } from "@/components/SecretField";
import { Label } from "@/components/ui/label";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { DropdownMenuItem } from "@/components/ui/dropdown-menu";
import { ConfirmDialog } from "@/components/dialogs";
import { useConfirmIdentity } from "@/components/ConfirmIdentity";
import { ErrorState } from "@/components/ErrorState";
import { t } from "@/lib/i18n";
import { formatDateTime } from "@/lib/utils";

/** Credentials outlast the browser session that makes them, so who makes them is asked again */
const recentNote = () => t("Sign in again before managing S3 keys if your sign-in is older than 10 minutes.");

export const s3FolderQuery = (nodeId: string) => ({
  queryKey: keys.buckets(nodeId),
  queryFn: ({ signal }: { signal: AbortSignal }) => api.s3Buckets(nodeId, undefined, signal),
});

/** Moves the focus to the list a card was in, once the card is gone (deleted): not left on the page behind */
export function focusList(card: HTMLElement | null) {
  const list = card?.parentElement;
  if (!list) return;
  requestAnimationFrame(() => {
    list.tabIndex = -1;
    list.focus();
  });
}

/** What a key's state is now: stopped, past its end date, or working */
function keyState(key: S3Key) {
  if (!key.enabled) return t("Stopped");
  if (key.expires_at && key.expires_at * 1000 <= Date.now()) return t("Ended");
  return t("Enabled");
}

function KeyForm({ bucket, close }: { bucket: S3Bucket; close(): void }) {
  const qc = useQueryClient();
  const [description, setDescription] = useState("");
  const [writable, setWritable] = useState(false);
  const [expiry, setExpiry] = useState("");
  const [created, setCreated] = useState<(S3Key & { secret_key: string }) | null>(null);
  const identity = useConfirmIdentity("s3-key");
  const make = useMutation({
    // The Secret Key is in the answer: kept no longer than the dialog shows it
    gcTime: 0,
    mutationFn: () => api.newS3Key(bucket.id, description.trim(), writable, expiry ? Math.floor(new Date(expiry).getTime() / 1000) : null, identity.values),
    onSuccess: (key) => {
      identity.clear();
      setCreated(key);
      void qc.invalidateQueries({ queryKey: keys.bucketLists() });
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
            <Label className="grid gap-2">
              {t("Access Key")}
              <Input readOnly value={created.access_key} className="mt-2 font-mono" />
            </Label>
            <div className="grid gap-2">
              <Label htmlFor="s3-new-secret">{t("Secret Key")}</Label>
              <SecretInput id="s3-new-secret" readOnly value={created.secret_key} className="font-mono" autoComplete="off" revealLabel={t("Show Secret Key")} />
            </div>
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
            <Label className="grid gap-2">
              {t("Key description")}
              <Input className="mt-2" value={description} maxLength={160} onChange={(e) => setDescription(e.target.value)} required />
            </Label>
            <Label className="flex items-center gap-2">
              <input type="checkbox" className="size-4 accent-brand" checked={writable} onChange={(e) => setWritable(e.target.checked)} />
              {t("Read and change files")}
            </Label>
            <p className="text-xs text-muted-foreground">{t("Leave unchecked for read-only access. Read-only spaces accept read-only keys only.")}</p>
            <Label className="grid gap-2">
              {t("End date (optional)")}
              <Input className="mt-2" type="datetime-local" value={expiry} onChange={(e) => setExpiry(e.target.value)} />
            </Label>
            {identity.fields(recentNote())}
            <Button type="submit" disabled={make.isPending || !description.trim() || !identity.ready}>
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
  const card = useRef<HTMLElement>(null);
  const newKeyButton = useRef<HTMLButtonElement>(null);
  const changed = () => qc.invalidateQueries({ queryKey: keys.bucketLists() });
  const toggle = useMutation({ mutationFn: () => api.setS3Bucket(bucket.id, !bucket.enabled), onSuccess: changed, onError: (e) => toast.error(e.message) });
  const toggleKey = useMutation({ mutationFn: (key: S3Key) => api.setS3Key(bucket.id, key.id, !key.enabled), onSuccess: changed, onError: (e) => toast.error(e.message) });
  return (
    <section ref={card} className="grid gap-3 rounded-lg border p-4" aria-label={bucket.name}>
      <div className="flex flex-wrap items-center gap-2">
        <h3 className="min-w-0 flex-1 font-medium [overflow-wrap:anywhere]">{bucket.name}</h3>
        <span className="text-xs text-muted-foreground">
          {bucket.active ? t("Active") : bucket.enabled ? t("Not active: S3 is off, or its creator can't publish this folder any more") : t("Stopped")}
        </span>
      </div>
      <p className="text-xs text-muted-foreground">
        {t("Creator: {creator} · Folder: {folder} · Last used: {when}", {
          creator: bucket.creator,
          folder: bucket.folder,
          when: bucket.last_used_at ? formatDateTime(bucket.last_used_at) : t("Never"),
        })}
      </p>
      <div className="flex flex-wrap gap-2">
        <Button size="sm" variant="outline" disabled={toggle.isPending} onClick={() => toggle.mutate()}>
          {bucket.enabled ? t("Stop bucket") : t("Enable bucket")}
        </Button>
        <Button ref={newKeyButton} size="sm" variant="outline" onClick={() => setNewKey(true)}>
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
              {key.writable ? t("Read and change files") : t("Read files only")} · {keyState(key)}
            </span>
            <span>{t("End date: {date}", { date: key.expires_at ? formatDateTime(key.expires_at) : t("No end date") })}</span>
            <span>{t("Last used: {when}", { when: key.last_used_at ? formatDateTime(key.last_used_at) : t("Never") })}</span>
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
          description={
            remove === "bucket"
              ? t("The bucket and its Access Keys are deleted for good: programs using them lose access. Files stay in their folder.")
              : t("The Access Key is deleted for good: programs using it lose access. Files stay in their folder.")
          }
          confirmText={t("Delete")}
          destructive
          onClose={() => setRemove(null)}
          onConfirm={async () => {
            if (remove === "bucket") await api.deleteS3Bucket(bucket.id);
            else await api.deleteS3Key(bucket.id, remove.id);
            await changed();
            setRemove(null);
            // Somewhere to be once what was deleted is gone: the bucket's other buttons, or the list it was in
            if (remove === "bucket") focusList(card.current);
            else requestAnimationFrame(() => newKeyButton.current?.focus());
          }}
        />
      )}
    </section>
  );
}

export function S3BucketDialog({ nodeId, onClose }: { nodeId: string; onClose(): void }) {
  const qc = useQueryClient();
  const q = useQuery(s3FolderQuery(nodeId));
  const options = useQuery({ queryKey: keys.s3Options(), queryFn: ({ signal }) => api.s3Options(signal) });
  const [name, setName] = useState("");
  const identity = useConfirmIdentity("s3-publish");
  const publish = useMutation({
    mutationFn: () => api.publishS3Bucket(nodeId, name.trim(), identity.values),
    onSuccess: () => {
      setName("");
      identity.clear();
      void qc.invalidateQueries({ queryKey: keys.bucketLists() });
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
                {/* The S3 hostname is reached over HTTPS on its own (a reverse proxy), not at this page's address */}
                {t("Endpoint: {url} · Region: {region} · Use path-style URLs", { url: `https://${options.data.hostname}`, region: options.data.region })}
              </p>
            )}
            {q.data.can_publish ? (
              <form
                className="grid gap-2"
                onSubmit={(e) => {
                  e.preventDefault();
                  publish.mutate();
                }}
              >
                <div className="flex gap-2">
                  <Input
                    aria-label={t("Bucket name")}
                    placeholder={t("Bucket name")}
                    value={name}
                    onChange={(e) => setName(e.target.value)}
                    minLength={3}
                    maxLength={63}
                    pattern="[a-z0-9][a-z0-9\\-]*[a-z0-9]"
                    required
                  />
                  <Button type="submit" disabled={publish.isPending || !identity.ready}>
                    {t("Publish")}
                  </Button>
                </div>
                {identity.fields(recentNote())}
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
          {t("{name} · {n} Access Key|{name} · {n} Access Keys", { name: bucket.name, n: bucket.keys.length })}
        </p>
      ))}
      <Button size="sm" variant="outline" onClick={() => setOpen(true)}>
        {t("Manage buckets and keys")}
      </Button>
      {open && <S3BucketDialog nodeId={nodeId} onClose={() => setOpen(false)} />}
    </section>
  );
}
