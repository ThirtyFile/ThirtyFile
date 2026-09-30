import { useState, type FormEvent } from "react";
import { Link, useSearchParams } from "react-router";
import { Loader2Icon } from "lucide-react";
import { api } from "@/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ErrorText, errorProps } from "@/components/dialogs";
import { SiteName } from "@/components/SiteName";
import { useBranding } from "@/lib/branding";
import { t } from "@/lib/i18n";

/** "Forgot password" (/reset-password): asks for a link by email; with the link's token, sets a new password */
export function ResetPasswordPage() {
  const [params] = useSearchParams();
  const token = params.get("token");
  const b = useBranding();
  return (
    <div className="flex min-h-full items-center justify-center bg-muted/40 p-4">
      <div className="grid w-full max-w-sm gap-4 rounded-lg border bg-background p-6 shadow-sm">
        <h1 className="text-lg font-semibold">
          <SiteName name={b.site_name} />
        </h1>
        {token ? <NewPassword token={token} /> : <AskForLink />}
        <Link to="/login" className="text-sm text-muted-foreground underline-offset-4 hover:underline">
          {t("Back to sign-in")}
        </Link>
      </div>
    </div>
  );
}

function AskForLink() {
  const [account, setAccount] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [sent, setSent] = useState(false);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api.forgotPassword(account.trim());
      setSent(true);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };
  if (sent) {
    return (
      <p className="text-sm" role="status">
        {t("If the account has an email address, a link to choose a new password is on its way. It works for an hour.")}
      </p>
    );
  }
  return (
    <form onSubmit={submit} className="grid gap-3">
      <p className="text-sm text-muted-foreground">{t("Enter your username or email address. We'll send a link to choose a new password.")}</p>
      <div className="grid gap-1.5">
        <Label htmlFor="rp-account">{t("Username or email address")}</Label>
        <Input id="rp-account" value={account} onChange={(e) => setAccount(e.target.value)} autoComplete="username" autoFocus {...errorProps(error, "rp-error")} />
      </div>
      <ErrorText id="rp-error">{error}</ErrorText>
      <Button type="submit" disabled={busy || !account.trim()}>
        {busy && <Loader2Icon className="animate-spin" />}
        {t("Send link")}
      </Button>
    </form>
  );
}

function NewPassword({ token }: { token: string }) {
  const [next, setNext] = useState("");
  const [confirm, setConfirm] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (next !== confirm) {
      setError(t("The new passwords don't match"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await api.resetPassword(token, next);
      setDone(true);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };
  if (done) {
    return (
      <p className="text-sm" role="status">
        {t("Your password is changed. Sign in with it; you were signed out everywhere else.")}
      </p>
    );
  }
  return (
    <form onSubmit={submit} className="grid gap-3">
      <div className="grid gap-1.5">
        <Label htmlFor="rp-new">{t("New password")}</Label>
        <Input id="rp-new" type="password" value={next} onChange={(e) => setNext(e.target.value)} autoComplete="new-password" autoFocus {...errorProps(error, "rp-new-error")} />
      </div>
      <div className="grid gap-1.5">
        <Label htmlFor="rp-confirm">{t("Confirm new password")}</Label>
        <Input id="rp-confirm" type="password" value={confirm} onChange={(e) => setConfirm(e.target.value)} autoComplete="new-password" {...errorProps(error, "rp-new-error")} />
      </div>
      <ErrorText id="rp-new-error">{error}</ErrorText>
      <Button type="submit" disabled={busy || !next || !confirm}>
        {busy && <Loader2Icon className="animate-spin" />}
        {t("Change password")}
      </Button>
    </form>
  );
}
