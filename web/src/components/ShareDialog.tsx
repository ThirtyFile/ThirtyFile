import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CopyIcon, KeyRoundIcon, Link2Icon, Loader2Icon, Trash2Icon } from "lucide-react";
import { toast } from "sonner";
import { api, type Node, type ShareInfo } from "@/api";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Separator } from "@/components/ui/separator";
import { ErrorText } from "@/components/dialogs";
import { ErrorState } from "@/components/ErrorState";
import { copyText, formatDate } from "@/lib/utils";
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
        ? t("Share links currently use a local address ({host}), so other people can't open them. Enter the site URL in Control panel › General settings.", { host: location.host })
        : t("Share links currently use a local address ({host}), so other people can't open them. Ask an administrator to set the site URL in system settings.", { host: location.host })}
    </p>
  );
}

export function shareSummary(s: ShareInfo) {
  const parts = [];
  if (s.has_password) parts.push(t("Password required"));
  parts.push(s.expires_at ? tc("date", "Expires {date}", { date: formatDate(s.expires_at) }) : t("Never expires"));
  if (s.max_downloads) parts.push(t("Downloaded {n}/{max} times", { n: s.downloads, max: s.max_downloads }));
  else parts.push(t("Downloaded {n} time|Downloaded {n} times", { n: s.downloads }));
  return parts.join(" · ");
}

const EXPIRY = [
  { label: t("Never expires"), days: 0 },
  { label: t("{n} day|{n} days", { n: 1 }), days: 1 },
  { label: t("{n} day|{n} days", { n: 7 }), days: 7 },
  { label: t("{n} day|{n} days", { n: 30 }), days: 30 },
  { label: t("{n} day|{n} days", { n: 90 }), days: 90 },
];

export function ShareDialog({ node, onClose }: { node: Node; onClose(): void }) {
  const qc = useQueryClient();
  const { link: shareLink, local, admin } = useShareLink();
  const shares = useQuery({
    queryKey: ["shares", node.id],
    queryFn: () => api.shares(node.id),
  });
  const [password, setPassword] = useState("");
  const [days, setDays] = useState(7);
  const [maxDownloads, setMaxDownloads] = useState("");

  const create = useMutation({
    mutationFn: () =>
      api.createShare({
        node_id: node.id,
        password: password || undefined,
        expires_at: days ? Math.floor(Date.now() / 1000) + days * 86400 : undefined,
        max_downloads: maxDownloads ? Number(maxDownloads) : undefined,
      }),
    onSuccess: async (s) => {
      await copyText(shareLink(s.id));
      toast.success(t("Share link created and copied to clipboard"));
      setPassword("");
      qc.invalidateQueries({ queryKey: ["shares"] });
    },
  });

  const remove = useMutation({
    mutationFn: (id: string) => api.deleteShare(id),
    onSuccess: () => {
      toast.success(t("Share link disabled"));
      qc.invalidateQueries({ queryKey: ["shares"] });
    },
  });

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle className="truncate pr-8">{t("Share link for “{name}”", { name: node.name })}</DialogTitle>
          <DialogDescription>
            {node.kind === "folder" ? t("Anyone with the link can browse and download the folder's contents.") : t("Anyone with the link can view and download this file.")}
          </DialogDescription>
        </DialogHeader>

        {local && <LocalLinkWarning admin={admin} />}

        {shares.error && <ErrorState compact message={shares.error.message} onRetry={() => shares.refetch()} />}

        {shares.data && shares.data.length > 0 && (
          <div className="grid gap-2">
            {shares.data.map((s) => (
              <div key={s.id} className="flex items-center gap-2 rounded-lg border px-3 py-2">
                <Link2Icon className="size-4 shrink-0 text-brand" />
                <div className="min-w-0 flex-1">
                  <div className="truncate font-mono text-xs">{shareLink(s.id)}</div>
                  <div className="text-xs text-muted-foreground">{shareSummary(s)}</div>
                </div>
                <Button
                  size="icon-sm"
                  variant="ghost"
                  aria-label={t("Copy link")}
                  title={t("Copy link")}
                  onClick={async () => {
                    await copyText(shareLink(s.id));
                    toast.success(t("Link copied"));
                  }}
                >
                  <CopyIcon />
                </Button>
                <Button size="icon-sm" variant="ghost" aria-label={t("Disable link")} title={t("Disable link")} onClick={() => remove.mutate(s.id)} disabled={remove.isPending}>
                  <Trash2Icon />
                </Button>
              </div>
            ))}
            <ErrorText>{remove.error?.message}</ErrorText>
            <Separator className="my-1" />
          </div>
        )}

        <form
          className="grid gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            create.mutate();
          }}
        >
          <div className="grid gap-1.5">
            <Label>{t("Expiration")}</Label>
            <div className="flex flex-wrap gap-1.5">
              {EXPIRY.map((o) => (
                <Button key={o.days} type="button" size="sm" variant={days === o.days ? "default" : "outline"} onClick={() => setDays(o.days)}>
                  {o.label}
                </Button>
              ))}
            </div>
          </div>
          <div className="grid grid-cols-2 gap-3">
            <div className="grid gap-1.5">
              <Label htmlFor="share-pw">
                <KeyRoundIcon className="size-3.5" /> {t("Password (optional)")}
              </Label>
              <Input id="share-pw" value={password} onChange={(e) => setPassword(e.target.value)} placeholder={tc("unset", "None")} autoComplete="off" />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="share-max">{t("Download limit (optional)")}</Label>
              <Input
                id="share-max"
                type="number"
                min={1}
                value={maxDownloads}
                onChange={(e) => setMaxDownloads(e.target.value)}
                placeholder={t("Unlimited")}
              />
            </div>
          </div>
          <ErrorText>{create.error?.message}</ErrorText>
          <div className="flex justify-end gap-2">
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Close")}
            </Button>
            <Button type="submit" disabled={create.isPending}>
              {create.isPending ? <Loader2Icon className="animate-spin" /> : <Link2Icon />}
              {t("Create link")}
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}
