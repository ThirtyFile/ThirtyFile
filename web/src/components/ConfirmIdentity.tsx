import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/api";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { t } from "@/lib/i18n";

/**
 * Asking who it is again before a change that outlasts this browser session (the email address, a linked sign-in
 * method): the current password and, with two-factor sign-in, a code. Accounts without a password confirm by having
 * signed in recently instead, which the server checks.
 */
export function useConfirmIdentity(id: string) {
  const tf = useQuery({ queryKey: ["two-factor"], queryFn: api.twoFactor });
  const hasPassword = tf.data?.has_password ?? true;
  const needsCode = !!tf.data?.enabled;
  const [password, setPassword] = useState("");
  const [code, setCode] = useState("");
  return {
    ready: (!hasPassword || !!password) && (!needsCode || !!code.trim()),
    values: { password: hasPassword ? password : undefined, code: needsCode ? code.trim() : undefined },
    clear: () => {
      setPassword("");
      setCode("");
    },
    /** The fields, or `recentNote` for an account without a password */
    fields: (recentNote: string) =>
      hasPassword ? (
        <div className="flex flex-wrap gap-2">
          <div className="grid min-w-40 flex-1 gap-1.5">
            <Label htmlFor={`${id}-password`}>{t("Your current password")}</Label>
            <Input id={`${id}-password`} type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" />
          </div>
          {needsCode && (
            <div className="grid w-40 gap-1.5">
              <Label htmlFor={`${id}-code`}>{t("Code from your app")}</Label>
              <Input id={`${id}-code`} value={code} onChange={(e) => setCode(e.target.value)} inputMode="numeric" autoComplete="one-time-code" />
            </div>
          )}
        </div>
      ) : (
        <p className="text-xs text-muted-foreground">{recentNote}</p>
      ),
  };
}
