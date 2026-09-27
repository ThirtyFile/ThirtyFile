import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link2Icon, Loader2Icon, UnlinkIcon } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { ProviderIcon, SSO_LABEL, type SsoProviderId } from "@/components/ProviderIcon";
import { formatDateTime } from "@/lib/utils";
import { t } from "@/lib/i18n";

/** Account menu › Sign-in methods: link or unlink Microsoft / Google / GitHub accounts */
export function LinkedAccountsDialog({ onClose }: { onClose(): void }) {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["identities"], queryFn: api.myIdentities });
  const unlink = useMutation({
    mutationFn: api.unlinkIdentity,
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["identities"] });
      toast.success(t("Account unlinked"));
    },
    onError: (e) => toast.error(e.message),
  });
  const here = window.location.pathname + window.location.search;
  const providers = [...new Set([...(q.data?.available ?? []), ...(q.data?.linked.map((l) => l.provider) ?? [])])] as SsoProviderId[];

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{t("Sign-in methods")}</DialogTitle>
          <DialogDescription>{t("Link a work or personal external account to sign in with it directly from the sign-in page.")}</DialogDescription>
        </DialogHeader>
        {q.isLoading ? (
          <Loader2Icon className="mx-auto size-5 animate-spin text-muted-foreground" />
        ) : providers.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t("Your administrator hasn't turned on any third-party sign-in.")}</p>
        ) : (
          <div className="divide-y rounded-lg border">
            {providers.map((p) => {
              const linked = q.data?.linked.find((l) => l.provider === p);
              const available = q.data?.available.includes(p);
              return (
                <div key={p} className="flex items-center gap-3 px-3 py-2.5">
                  <ProviderIcon provider={p} className="size-5" />
                  <div className="min-w-0 flex-1">
                    <div className="text-sm">{SSO_LABEL[p] ?? p}</div>
                    <div className="truncate text-xs text-muted-foreground">
                      {linked
                        ? `${linked.email || linked.name || t("Linked")}${linked.last_login_at ? ` · ${t("Last sign-in {time}", { time: formatDateTime(linked.last_login_at) })}` : ""}`
                        : t("Not linked")}
                    </div>
                  </div>
                  {linked ? (
                    <Button variant="ghost" size="sm" disabled={unlink.isPending} onClick={() => unlink.mutate(p)} title={t("After unlinking, you can no longer sign in with this account")}>
                      <UnlinkIcon /> {t("Unlink")}
                    </Button>
                  ) : available ? (
                    <Button variant="outline" size="sm" nativeButton={false} render={<a href={api.ssoStartUrl(p, here, true)} />}>
                      <Link2Icon /> {t("Link")}
                    </Button>
                  ) : null}
                </div>
              );
            })}
          </div>
        )}
        <p className="text-xs text-muted-foreground">{t("Before unlinking all accounts, make sure you remember your username and password; otherwise, you'll need an administrator to reset your password to sign in.")}</p>
      </DialogContent>
    </Dialog>
  );
}
