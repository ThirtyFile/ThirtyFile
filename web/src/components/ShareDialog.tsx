import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CopyIcon, KeyRoundIcon, Link2Icon, Loader2Icon, PencilIcon, Trash2Icon } from "lucide-react";
import { toast } from "sonner";
import { api, type Node, type ShareAccessOptions, type SharePolicy, type ShareInfo, type ShareUpdate } from "@/api";
import { keys } from "@/api/queryKeys";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Separator } from "@/components/ui/separator";
import { ErrorText, errorProps } from "@/components/dialogs";
import { ErrorState } from "@/components/ErrorState";
import { useConfirm } from "@/components/confirm";
import type { ConfirmOptions } from "@/lib/confirm";
import { copyAndSay, copyText, formatDate } from "@/lib/utils";
import { useMe } from "@/lib/session";
import { t, tc } from "@/lib/i18n";

/** Path of a share link */
export const sharePath = (id: string) => `/share/${id}`;

/** The browser is currently on a local address (127.0.0.1, localhost); others can't open the generated link */
const isLocalOrigin = () => /^(localhost|127\.|\[::1\]$|0\.0\.0\.0$)/.test(location.hostname);

/**
 * Build a share link: prefer the "Site URL" from system settings, falling back to the browser's current URL.
 * local: no URL configured and currently on a local address (others can't open the generated link)
 */
export function useShareLink() {
  const me = useMe();
  const base = me.public_url || location.origin;
  return {
    link: (id: string) => base + sharePath(id),
    local: !me.public_url && isLocalOrigin(),
    admin: me.role === "admin",
  };
}

/** Warning when no site URL is configured and we're on a local address */
export function LocalLinkWarning({ admin }: { admin: boolean }) {
  return (
    <p className="rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs leading-relaxed text-amber-700 dark:text-amber-300">
      {admin
        ? t("Share links currently use a local address ({host}), so other people can't open them. Enter the site URL in Control panel › General.", { host: location.host })
        : t("Share links currently use a local address ({host}), so other people can't open them. Ask an administrator to set the site URL in system settings.", { host: location.host })}
    </p>
  );
}

/** Shown while an administrator has turned public links off */
export function LinksOffNotice() {
  return (
    <p className="rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs leading-relaxed text-amber-700 dark:text-amber-300">
      {t("An administrator has turned off public share links. Existing links don't work until they're allowed again.")}
    </p>
  );
}

export function shareSummary(s: ShareInfo) {
  const parts = [];
  if (s.has_password) parts.push(t("Password required"));
  parts.push(s.expires_at ? tc("date", "Expires {date}", { date: formatDate(s.expires_at) }) : t("Never expires"));
  if (s.drop_only) parts.push(t("Only accepts files"));
  else {
    if (s.allow_upload) parts.push(t("Accepts files"));
    if (!s.allow_download) parts.push(t("Preview only"));
  }
  if (s.max_downloads) parts.push(t("Downloaded {n}/{max} times", { n: s.downloads, max: s.max_downloads }));
  else parts.push(t("Downloaded {n} time|Downloaded {n} times", { n: s.downloads }));
  return parts.join(" · ");
}

/** What visitors may do: download, upload (folders), only upload (folders) */
export function AccessOptions({ folder, value, onChange }: { folder: boolean; value: ShareAccessOptions; onChange(v: ShareAccessOptions): void }) {
  const set = (patch: Partial<ShareAccessOptions>) => onChange({ ...value, ...patch });
  return (
    <div className="grid gap-2">
      <Label>{t("Visitors can")}</Label>
      {!value.drop_only && (
        <Label className="flex items-start gap-2 font-normal">
          <Checkbox className="mt-0.5" checked={value.allow_download} onCheckedChange={(v) => set({ allow_download: !!v })} />
          <span>
            {t("Download")}
            {!value.allow_download && (
              <span className="block text-xs text-muted-foreground">
                {t("Visitors can only preview files. A preview still sends the whole file to their browser, so this hides the download buttons but can't stop someone from saving a file.")}
              </span>
            )}
          </span>
        </Label>
      )}
      {folder && (
        <Label className="flex items-start gap-2 font-normal">
          <Checkbox className="mt-0.5" checked={value.allow_upload} onCheckedChange={(v) => set({ allow_upload: !!v, drop_only: !!v && value.drop_only })} />
          <span>
            {t("Upload files")}
            {value.allow_upload && (
              <span className="block text-xs text-muted-foreground">{t("Uploaded files are yours and count toward the space's size. A file with a name that is taken gets a number.")}</span>
            )}
          </span>
        </Label>
      )}
      {folder && value.allow_upload && (
        <Label className="flex items-start gap-2 font-normal">
          <Checkbox className="mt-0.5" checked={value.drop_only} onCheckedChange={(v) => set({ drop_only: !!v })} />
          <span>
            {t("Only upload")}
            <span className="block text-xs text-muted-foreground">{t("Visitors don't see what is in the folder, and can't download anything: for collecting files.")}</span>
          </span>
        </Label>
      )}
    </div>
  );
}

/** Where the linked item is: the space, and whose "My files" it is */
export function shareSpace(s: ShareInfo) {
  if (s.drive_kind === "personal") return t("My files of {name}", { name: s.drive_owner });
  if (s.drive_kind === "company" && s.drive_name === "All files") return t("All files");
  return s.drive_name;
}

/** Expiry choices allowed by the policy: with a maximum, "never" and longer choices are left out, and the maximum itself is offered */
export function expiryChoices(policy: SharePolicy) {
  const days = [0, 1, 7, 30, 90].filter((d) => !policy.max_days || (d > 0 && d <= policy.max_days));
  if (policy.max_days && !days.includes(policy.max_days)) days.push(policy.max_days);
  return days.map((d) => ({ days: d, label: d ? t("{n} day|{n} days", { n: d }) : t("Never expires") }));
}

const expiresIn = (days: number) => (days ? Math.floor(Date.now() / 1000) + days * 86400 : null);

/** Asked before a share link is deleted (here and on the share link lists) */
export const deleteLinkQuestion = (): ConfirmOptions => ({
  title: t("Delete this share link?"),
  description: t("People who have the link can no longer open it. A new link would have a different address."),
  confirmText: t("Delete link"),
  destructive: true,
});

export function ShareDialog({ node, onClose }: { node: Node; onClose(): void }) {
  const qc = useQueryClient();
  const me = useMe();
  const policy = me.share_policy;
  const { link: shareLink, local, admin } = useShareLink();
  const shares = useQuery({
    queryKey: keys.sharesOf(node.id),
    queryFn: () => api.shares(node.id),
  });
  const choices = expiryChoices(policy);
  const [password, setPassword] = useState("");
  const [days, setDays] = useState(choices.some((c) => c.days === 7) ? 7 : choices[choices.length - 1].days);
  const [maxDownloads, setMaxDownloads] = useState("");
  const [editing, setEditing] = useState<ShareInfo | null>(null);
  const [access, setAccess] = useState<ShareAccessOptions>({ allow_upload: false, drop_only: false, allow_download: true });

  const create = useMutation({
    mutationFn: () =>
      api.createShare({
        ...access,
        node_id: node.id,
        password: password || undefined,
        expires_at: expiresIn(days) ?? undefined,
        max_downloads: maxDownloads ? Number(maxDownloads) : undefined,
      }),
    onSuccess: () => {
      setPassword("");
      qc.invalidateQueries({ queryKey: keys.shares() });
    },
  });
  // The link is put on the clipboard from within the click (as it is being made): some browsers allow writing to the
  // clipboard only then. Only a link that got there is said to be copied
  const createAndCopy = async () => {
    const made = create.mutateAsync();
    const copied = copyText(made.then((s) => shareLink(s.id)));
    let link: string;
    try {
      link = shareLink((await made).id);
    } catch {
      // Shown under the form
      return;
    }
    if (await copied) toast.success(t("Share link created and copied to clipboard"));
    else toast.success(t("Share link created"), { action: { label: t("Copy link"), onClick: () => void copyAndSay(link, t("Link copied")) } });
  };

  const [ask, question] = useConfirm();
  const remove = useMutation({
    mutationFn: (id: string) => api.deleteShare(id),
    onSuccess: () => {
      toast.success(t("Share link deleted"));
      qc.invalidateQueries({ queryKey: keys.shares() });
    },
    onError: (e) => toast.error(e.message),
  });

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        {question}
        <DialogHeader>
          <DialogTitle className="truncate pr-8">{t("Share link for “{name}”", { name: node.name })}</DialogTitle>
          <DialogDescription>
            {node.kind === "folder"
              ? t("Anyone with the link can browse the folder's contents and, as you choose below, download them or upload files.")
              : t("Anyone with the link can view and, as you choose below, download this file.")}
          </DialogDescription>
        </DialogHeader>

        {local && <LocalLinkWarning admin={admin} />}
        {!policy.public_links && <LinksOffNotice />}

        {shares.error && <ErrorState compact message={shares.error.message} onRetry={() => shares.refetch()} />}

        {shares.data && shares.data.length > 0 && (
          <div className="grid gap-2">
            {shares.data.map((s) => (
              <div key={s.id} className="flex items-center gap-2 rounded-lg border px-3 py-2">
                <Link2Icon className="size-4 shrink-0 text-brand" />
                <div className="min-w-0 flex-1">
                  <div className="truncate font-mono text-xs">{shareLink(s.id)}</div>
                  <div className="text-xs text-muted-foreground">
                    {s.owner_id !== me.id && `${t("Created by {name}", { name: s.owner_name })} · `}
                    {shareSummary(s)}
                  </div>
                </div>
                <Button size="icon-sm" variant="ghost" aria-label={t("Copy link")} title={t("Copy link")} onClick={() => copyAndSay(shareLink(s.id), t("Link copied"))}>
                  <CopyIcon />
                </Button>
                <Button size="icon-sm" variant="ghost" aria-label={t("Edit link")} title={t("Edit link")} onClick={() => setEditing(s)}>
                  <PencilIcon />
                </Button>
                <Button
                  size="icon-sm"
                  variant="ghost"
                  aria-label={t("Delete link")}
                  title={t("Delete link")}
                  onClick={async () => (await ask(deleteLinkQuestion())) && remove.mutate(s.id)}
                  disabled={remove.isPending}
                >
                  <Trash2Icon />
                </Button>
              </div>
            ))}
            <ErrorText>{remove.error?.message}</ErrorText>
            <Separator className="my-1" />
          </div>
        )}

        {policy.public_links && (
          <form
            className="grid gap-3"
            onSubmit={(e) => {
              e.preventDefault();
              void createAndCopy();
            }}
          >
            <div className="grid gap-1.5">
              <Label id="share-expiry">{t("Expiration")}</Label>
              {/* One of the buttons is chosen: aria-pressed says which */}
              <div role="group" aria-labelledby="share-expiry" className="flex flex-wrap gap-1.5">
                {choices.map((o) => (
                  <Button key={o.days} type="button" size="sm" variant={days === o.days ? "default" : "outline"} aria-pressed={days === o.days} onClick={() => setDays(o.days)}>
                    {o.label}
                  </Button>
                ))}
              </div>
            </div>
            <AccessOptions folder={node.kind === "folder"} value={access} onChange={setAccess} />
            <div className="grid grid-cols-2 gap-3">
              <div className="grid gap-1.5">
                <Label htmlFor="share-pw">
                  <KeyRoundIcon className="size-3.5" /> {policy.password_required ? t("Password (required)") : t("Password (optional)")}
                </Label>
                <Input
                  id="share-pw"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  placeholder={policy.password_required ? undefined : tc("unset", "None")}
                  required={policy.password_required}
                  autoComplete="off"
                  {...errorProps(create.error, "share-error")}
                />
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor="share-max">{t("Download limit (optional)")}</Label>
                <Input id="share-max" type="number" min={1} value={maxDownloads} onChange={(e) => setMaxDownloads(e.target.value)} placeholder={t("Unlimited")} />
              </div>
            </div>
            <ErrorText id="share-error">{create.error?.message}</ErrorText>
            <div className="flex justify-end gap-2">
              <Button type="button" variant="outline" onClick={onClose}>
                {t("Close")}
              </Button>
              <Button type="submit" disabled={create.isPending || (policy.password_required && !password.trim())}>
                {create.isPending ? <Loader2Icon className="animate-spin" /> : <Link2Icon />}
                {t("Create link")}
              </Button>
            </div>
          </form>
        )}
        {editing && <EditShareDialog share={editing} onClose={() => setEditing(null)} />}
      </DialogContent>
    </Dialog>
  );
}

/** Changes a link's password, expiry and download limit; the address stays the same */
export function EditShareDialog({ share, onClose }: { share: ShareInfo; onClose(): void }) {
  const qc = useQueryClient();
  const policy = useMe().share_policy;
  const choices = expiryChoices(policy);
  const [password, setPassword] = useState("");
  const [removePassword, setRemovePassword] = useState(false);
  // "keep": the current expiry stays; otherwise a number of days from now (0 = never)
  const [expiry, setExpiry] = useState<"keep" | number>("keep");
  const [maxDownloads, setMaxDownloads] = useState(share.max_downloads ? String(share.max_downloads) : "");
  const [access, setAccess] = useState<ShareAccessOptions>({ allow_upload: share.allow_upload, drop_only: share.drop_only, allow_download: share.allow_download });

  const save = useMutation({
    mutationFn: () => {
      const req: ShareUpdate = {};
      if (password.trim()) req.password = password;
      else if (removePassword) req.password = "";
      if (expiry !== "keep") req.expires_at = expiresIn(expiry);
      const limit = maxDownloads ? Number(maxDownloads) : null;
      if (limit !== share.max_downloads) req.max_downloads = limit;
      if (access.allow_upload !== share.allow_upload) req.allow_upload = access.allow_upload;
      if (access.drop_only !== share.drop_only) req.drop_only = access.drop_only;
      if (access.allow_download !== share.allow_download) req.allow_download = access.allow_download;
      return api.updateShare(share.id, req);
    },
    onSuccess: () => {
      toast.success(t("Share link updated"));
      qc.invalidateQueries({ queryKey: keys.shares() });
      onClose();
    },
  });

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-md">
        <form
          className="grid gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            save.mutate();
          }}
        >
          <DialogHeader>
            <DialogTitle className="truncate pr-8">{t("Edit link for “{name}”", { name: share.node_name })}</DialogTitle>
            <DialogDescription className="font-mono text-xs">{sharePath(share.id)}</DialogDescription>
          </DialogHeader>
          <div className="grid gap-1.5">
            <Label id="edit-share-expiry">{t("Expiration")}</Label>
            <div role="group" aria-labelledby="edit-share-expiry" className="flex flex-wrap gap-1.5">
              <Button type="button" size="sm" variant={expiry === "keep" ? "default" : "outline"} aria-pressed={expiry === "keep"} onClick={() => setExpiry("keep")}>
                {share.expires_at ? t("Keep ({date})", { date: formatDate(share.expires_at) }) : t("Keep (never expires)")}
              </Button>
              {choices.map((o) => (
                <Button key={o.days} type="button" size="sm" variant={expiry === o.days ? "default" : "outline"} aria-pressed={expiry === o.days} onClick={() => setExpiry(o.days)}>
                  {o.label}
                </Button>
              ))}
            </div>
          </div>
          <AccessOptions folder={share.node_kind === "folder"} value={access} onChange={setAccess} />
          <div className="grid gap-1.5">
            <Label htmlFor="edit-share-pw">
              <KeyRoundIcon className="size-3.5" /> {share.has_password ? t("New password (leave blank to keep the current one)") : t("Password (optional)")}
            </Label>
            <Input
              id="edit-share-pw"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              disabled={removePassword}
              autoComplete="off"
              {...errorProps(save.error, "edit-share-error")}
            />
            {share.has_password && !policy.password_required && (
              <Label className="flex items-center gap-2 font-normal">
                <Checkbox checked={removePassword} onCheckedChange={(v) => setRemovePassword(!!v)} />
                {t("Remove the password")}
              </Label>
            )}
            <p className="text-xs text-muted-foreground">{t("With a new password, people who opened the link before must enter it again.")}</p>
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="edit-share-max">{t("Download limit (optional)")}</Label>
            <Input id="edit-share-max" type="number" min={1} value={maxDownloads} onChange={(e) => setMaxDownloads(e.target.value)} placeholder={t("Unlimited")} />
            <p className="text-xs text-muted-foreground">{t("Downloaded {n} time|Downloaded {n} times", { n: share.downloads })}</p>
          </div>
          <ErrorText id="edit-share-error">{save.error?.message}</ErrorText>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={save.isPending}>
              {save.isPending && <Loader2Icon className="animate-spin" />}
              {t("Save")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
