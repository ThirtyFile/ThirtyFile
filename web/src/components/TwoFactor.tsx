import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CopyIcon, DownloadIcon, Loader2Icon, ShieldCheckIcon, ShieldIcon } from "lucide-react";
import { toast } from "sonner";
import { api, type TwoFactorSetup } from "@/api";
import { keys, queries } from "@/api/queryKeys";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ErrorText, errorProps } from "@/components/dialogs";
import { cn, copyAndSay } from "@/lib/utils";
import { t } from "@/lib/i18n";

/** The QR code to scan and the secret to type in instead (grouped by four characters) */
export function SetupCode({ setup, className }: { setup: TwoFactorSetup; className?: string }) {
  const grouped = setup.secret.match(/.{1,4}/g)?.join(" ") ?? setup.secret;
  return (
    <div className={cn("grid justify-items-center gap-2", className)}>
      <img
        src={`data:image/svg+xml;charset=utf-8,${encodeURIComponent(setup.qr_svg)}`}
        alt={t("QR code for the authenticator app")}
        className="size-44 rounded-md bg-white p-1.5"
        draggable={false}
      />
      <div className="text-xs opacity-80">{t("Can't scan it? Enter this key in the app:")}</div>
      <button type="button" className="rounded px-1.5 font-mono text-sm tracking-wide select-all hover:bg-black/10" title={t("Copy")} onClick={() => copyAndSay(setup.secret)}>
        {grouped}
      </button>
    </div>
  );
}

/** New recovery codes: shown once, with copy and download */
export function RecoveryCodes({ codes, className }: { codes: string[]; className?: string }) {
  const text = codes.join("\n");
  const download = () => {
    const url = URL.createObjectURL(new Blob([`${text}\n`], { type: "text/plain" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = "recovery-codes.txt";
    a.click();
    URL.revokeObjectURL(url);
  };
  return (
    <div className={cn("grid gap-2", className)}>
      <p className="text-xs opacity-80">{t("Keep these recovery codes somewhere safe. Each one signs you in once if you lose your phone. They won't be shown again.")}</p>
      <ul className="grid grid-cols-2 gap-x-4 gap-y-1 rounded-md border border-current/20 p-3 font-mono text-sm">
        {codes.map((c) => (
          <li key={c}>{c}</li>
        ))}
      </ul>
      <div className="flex gap-2">
        <Button type="button" size="sm" variant="outline" className="text-foreground" onClick={() => copyAndSay(text)}>
          <CopyIcon /> {t("Copy")}
        </Button>
        <Button type="button" size="sm" variant="outline" className="text-foreground" onClick={download}>
          <DownloadIcon /> {t("Download")}
        </Button>
      </div>
    </div>
  );
}

type Action = "setup" | "disable" | "codes";

/** Account menu › Two-factor sign-in: set it up with an authenticator app, new recovery codes, turn it off */
export function TwoFactorDialog({ onClose }: { onClose(): void }) {
  const qc = useQueryClient();
  const q = useQuery(queries.twoFactor);
  /** Waiting for the password before this */
  const [action, setAction] = useState<Action | null>(null);
  const [password, setPassword] = useState("");
  /** Once it is on, a code from the current app (or a recovery code) comes with the password */
  const [currentCode, setCurrentCode] = useState("");
  const [setup, setSetup] = useState<TwoFactorSetup | null>(null);
  const [code, setCode] = useState("");
  const [codes, setCodes] = useState<string[] | null>(null);
  const reset = () => {
    setAction(null);
    setPassword("");
    setCurrentCode("");
    setSetup(null);
    setCode("");
    setCodes(null);
    qc.invalidateQueries({ queryKey: keys.twoFactor() });
  };

  const confirm = useMutation({
    mutationFn: async (a: Action) => {
      const current = q.data?.enabled ? currentCode.trim() : undefined;
      if (a === "setup") setSetup(await api.startTwoFactor(password, current));
      else if (a === "codes") setCodes((await api.newRecoveryCodes(password, current)).recovery_codes);
      else {
        await api.disableTwoFactor(password, current);
        toast.success(t("Two-factor sign-in turned off"));
        reset();
      }
      setPassword("");
      setCurrentCode("");
    },
  });
  const enable = useMutation({
    mutationFn: () => api.enableTwoFactor(code),
    onSuccess: (r) => {
      setSetup(null);
      setCodes(r.recovery_codes);
      toast.success(t("Two-factor sign-in turned on"));
    },
  });
  const s = q.data;
  const confirmReady = !!password && (!s?.enabled || !!currentCode.trim());

  const passwordForm = (a: Action) => (e: FormEvent) => {
    e.preventDefault();
    if (confirmReady && !confirm.isPending) confirm.mutate(a);
  };

  let body;
  if (!s) {
    body = <Loader2Icon className="mx-auto size-5 animate-spin text-muted-foreground" />;
  } else if (!s.has_password) {
    body = (
      <p className="text-sm text-muted-foreground">
        {t("You sign in with single sign-on (Microsoft, Google, GitHub or OpenID Connect), so there's no password here to protect. Turn on two-factor sign-in in that account instead.")}
      </p>
    );
  } else if (codes) {
    body = (
      <div className="grid gap-3">
        <RecoveryCodes codes={codes} />
        <div className="flex justify-end">
          <Button onClick={reset}>{t("Done")}</Button>
        </div>
      </div>
    );
  } else if (setup) {
    body = (
      <form
        className="grid gap-3"
        onSubmit={(e) => {
          e.preventDefault();
          if (code && !enable.isPending) enable.mutate();
        }}
      >
        <p className="text-sm">
          {t("Scan the QR code with an authenticator app (such as Microsoft Authenticator, Google Authenticator or 1Password), then enter the 6-digit code it shows.")}
        </p>
        <SetupCode setup={setup} />
        <div className="grid gap-1.5">
          <Label htmlFor="tf-code">{t("Code from the app")}</Label>
          <Input
            id="tf-code"
            inputMode="numeric"
            autoComplete="one-time-code"
            maxLength={7}
            value={code}
            onChange={(e) => setCode(e.target.value)}
            autoFocus
            {...errorProps(enable.error, "tf-code-error")}
          />
        </div>
        <ErrorText id="tf-code-error">{enable.error?.message}</ErrorText>
        <div className="flex justify-end gap-2">
          <Button type="button" variant="outline" onClick={reset}>
            {t("Cancel")}
          </Button>
          <Button type="submit" disabled={!code || enable.isPending}>
            {enable.isPending && <Loader2Icon className="animate-spin" />}
            {t("Turn on")}
          </Button>
        </div>
      </form>
    );
  } else if (action) {
    body = (
      <form className="grid gap-3" onSubmit={passwordForm(action)}>
        <div className="grid gap-1.5">
          <Label htmlFor="tf-pw">{t("Current password")}</Label>
          <Input
            id="tf-pw"
            type="password"
            autoComplete="current-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            autoFocus
            {...errorProps(confirm.error, "tf-confirm-error")}
          />
        </div>
        {s.enabled && (
          <div className="grid gap-1.5">
            <Label htmlFor="tf-current-code">{t("Code from your app, or a recovery code")}</Label>
            <Input
              id="tf-current-code"
              autoComplete="one-time-code"
              value={currentCode}
              onChange={(e) => setCurrentCode(e.target.value)}
              {...errorProps(confirm.error, "tf-confirm-error")}
            />
          </div>
        )}
        <ErrorText id="tf-confirm-error">{confirm.error?.message}</ErrorText>
        <div className="flex justify-end gap-2">
          <Button type="button" variant="outline" onClick={reset}>
            {t("Cancel")}
          </Button>
          <Button type="submit" variant={action === "disable" ? "destructive" : "default"} disabled={!confirmReady || confirm.isPending}>
            {confirm.isPending && <Loader2Icon className="animate-spin" />}
            {action === "disable" ? t("Turn off") : action === "codes" ? t("Create new codes") : t("Continue")}
          </Button>
        </div>
      </form>
    );
  } else {
    body = (
      <div className="grid gap-3">
        <div className="flex items-center gap-3 rounded-lg border p-3">
          {s.enabled ? <ShieldCheckIcon className="size-6 text-emerald-600 dark:text-emerald-400" /> : <ShieldIcon className="size-6 text-muted-foreground" />}
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2 text-sm font-medium">
              {s.enabled ? t("On") : t("Off")}
              {s.required && (
                <Badge variant="outline" className="h-4 px-1.5 text-[10px]">
                  {t("Required")}
                </Badge>
              )}
            </div>
            <div className="text-xs text-muted-foreground">
              {s.enabled
                ? t("{n} recovery code left|{n} recovery codes left", { n: s.recovery_codes_left })
                : t("Signing in with your password also asks for a code from an app on your phone.")}
            </div>
          </div>
        </div>
        <div className="flex flex-wrap justify-end gap-2">
          {s.enabled && (
            <>
              <Button variant="outline" onClick={() => setAction("codes")}>
                {t("New recovery codes")}
              </Button>
              <Button variant="outline" onClick={() => setAction("setup")} title={t("Use a different phone or app; the current one stops working")}>
                {t("Change app")}
              </Button>
              {!s.required && (
                <Button variant="outline" onClick={() => setAction("disable")}>
                  {t("Turn off")}
                </Button>
              )}
            </>
          )}
          {!s.enabled && <Button onClick={() => setAction("setup")}>{t("Set up")}</Button>}
        </div>
      </div>
    );
  }

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{t("Two-factor sign-in")}</DialogTitle>
          <DialogDescription>
            {t(
              "After your password, sign-in asks for a code from an authenticator app, so a stolen password alone isn't enough. App passwords and single sign-on (Microsoft, Google, GitHub or OpenID Connect) don't ask for it.",
            )}
          </DialogDescription>
        </DialogHeader>
        {body}
      </DialogContent>
    </Dialog>
  );
}
