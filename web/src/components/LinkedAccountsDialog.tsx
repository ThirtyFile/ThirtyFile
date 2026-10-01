import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link2Icon, Loader2Icon, UnlinkIcon } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { keys } from "@/api/queryKeys";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { ProviderIcon, SSO_LABEL, type SsoProviderId } from "@/components/ProviderIcon";
import { useConfirm } from "@/components/confirm";
import { useConfirmIdentity } from "@/components/ConfirmIdentity";
import { ErrorText } from "@/components/dialogs";
import { formatDateTime } from "@/lib/utils";
import { t } from "@/lib/i18n";

/** Account menu › Sign-in methods: link or unlink Microsoft / Google / GitHub accounts */
export function LinkedAccountsDialog({ onClose }: { onClose(): void }) {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: keys.identities(), queryFn: api.myIdentities });
  const unlink = useMutation({
    mutationFn: api.unlinkIdentity,
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: keys.identities() });
      toast.success(t("Account unlinked"));
    },
    onError: (e) => toast.error(e.message),
  });
  const [ask, question] = useConfirm();
  const confirmUnlink = async (p: SsoProviderId) => {
    const ok = await ask({
      title: t("Unlink your {provider} account?", { provider: SSO_LABEL[p] ?? p }),
      description: t("After unlinking, you can no longer sign in with this account"),
      confirmText: t("Unlink"),
      destructive: true,
    });
    if (ok) unlink.mutate(p);
  };
  const here = window.location.pathname + window.location.search;
  // A linked account signs in without the password, so linking asks for it (and a code) first
  const [linking, setLinking] = useState<SsoProviderId | null>(null);
  const identity = useConfirmIdentity("link");
  const startLink = useMutation({
    mutationFn: (p: SsoProviderId) => api.ssoLink(p, here, identity.values.password, identity.values.code),
    onSuccess: ({ url }) => window.location.assign(url),
  });
  const submitLink = (e: FormEvent) => {
    e.preventDefault();
    if (linking && identity.ready && !startLink.isPending) startLink.mutate(linking);
  };
  const providers = [...new Set([...(q.data?.available ?? []), ...(q.data?.linked.map((l) => l.provider) ?? [])])] as SsoProviderId[];

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-md">
        {question}
        <DialogHeader>
          <DialogTitle>{t("Sign-in methods")}</DialogTitle>
          <DialogDescription>{t("Link a work or personal external account to sign in with it directly from the sign-in page.")}</DialogDescription>
        </DialogHeader>
        {linking && (
          <form onSubmit={submitLink} className="grid gap-2 rounded-lg border p-3">
            <div className="flex items-center gap-2 text-sm font-medium">
              <ProviderIcon provider={linking} className="size-4" />
              {t("Link your {provider} account", { provider: SSO_LABEL[linking] ?? linking })}
            </div>
            <p className="text-xs text-muted-foreground">{t("The linked account can sign in to yours without the password, so confirm it's you first.")}</p>
            {identity.fields(t("For your security, accounts can only be linked within 10 minutes of signing in."))}
            <ErrorText>{startLink.error?.message}</ErrorText>
            <div className="flex justify-end gap-2">
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => {
                  setLinking(null);
                  identity.clear();
                  startLink.reset();
                }}
              >
                {t("Cancel")}
              </Button>
              <Button type="submit" size="sm" disabled={!identity.ready || startLink.isPending}>
                {startLink.isPending ? <Loader2Icon className="animate-spin" /> : <Link2Icon />}
                {t("Continue")}
              </Button>
            </div>
          </form>
        )}
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
                    <Button variant="ghost" size="sm" disabled={unlink.isPending} onClick={() => confirmUnlink(p)} title={t("After unlinking, you can no longer sign in with this account")}>
                      <UnlinkIcon /> {t("Unlink")}
                    </Button>
                  ) : available ? (
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={linking === p}
                      onClick={() => {
                        startLink.reset();
                        setLinking(p);
                      }}
                    >
                      <Link2Icon /> {t("Link")}
                    </Button>
                  ) : null}
                </div>
              );
            })}
          </div>
        )}
        <p className="text-xs text-muted-foreground">
          {t("Before unlinking all accounts, make sure you remember your username and password; otherwise, you'll need an administrator to reset your password to sign in.")}
        </p>
      </DialogContent>
    </Dialog>
  );
}
