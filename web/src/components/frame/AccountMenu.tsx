import { useState } from "react";
import { leaveAfterSignOut } from "@/lib/signOut";
import {
  BellIcon,
  ChevronsUpDownIcon,
  HistoryIcon,
  KeyRoundIcon,
  KeySquareIcon,
  LanguagesIcon,
  Link2Icon,
  LogOutIcon,
  MonitorSmartphoneIcon,
  MoonIcon,
  ShieldCheckIcon,
  SunIcon,
} from "lucide-react";
import { api } from "@/api";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { ChangePasswordDialog } from "@/components/ChangePasswordDialog";
import { LoginLogDialog } from "@/components/logs/LoginLog";
import { LinkedAccountsDialog } from "@/components/LinkedAccountsDialog";
import { DevicesDialog } from "@/components/DevicesDialog";
import { AppPasswordsDialog } from "@/components/AppPasswordsDialog";
import { TwoFactorDialog } from "@/components/TwoFactor";
import { NotificationSettingsDialog } from "@/components/NotificationSettingsDialog";
import { useMe } from "@/lib/session";
import { useTheme, type ThemeMode } from "@/lib/theme";
import { LANGS, lang, setLang, t, type Lang } from "@/lib/i18n";

/**
 * The signed-in user's menu at the bottom of the locations list: account settings, appearance, language and signing
 * out. `usage`: how much of "My files" is used; null for someone without it.
 */
export function AccountMenu({ usage }: { usage: string | null }) {
  const me = useMe();
  const { dark, mode, canToggle, setMode } = useTheme();
  const [changingPassword, setChangingPassword] = useState(false);
  const [showLogins, setShowLogins] = useState(false);
  const [showAccounts, setShowAccounts] = useState(false);
  const [showDevices, setShowDevices] = useState(false);
  const [showAppPasswords, setShowAppPasswords] = useState(false);
  const [showTwoFactor, setShowTwoFactor] = useState(false);
  const [showNotifications, setShowNotifications] = useState(false);

  const logout = async () => {
    await api.logout().catch(() => {});
    leaveAfterSignOut(me.id);
  };

  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger render={<button type="button" className="flex items-center gap-2 rounded px-1 py-1 text-left hover:bg-muted" />}>
          <span className="flex size-6 shrink-0 items-center justify-center rounded-full bg-brand text-[11px] font-medium text-brand-foreground uppercase">{me.username.slice(0, 1)}</span>
          <span className="min-w-0 flex-1 truncate text-xs" title={me.display_name ? me.username : undefined}>
            {me.display_name || me.username}
          </span>
          <ChevronsUpDownIcon className="size-3.5 text-muted-foreground" />
        </DropdownMenuTrigger>
        <DropdownMenuContent side="top" className="w-52">
          <div className="px-1.5 py-1 text-xs text-muted-foreground">
            {usage === null
              ? me.role === "admin"
                ? t("Administrator")
                : t("Standard user")
              : me.role === "admin"
                ? t("Administrator · {size} used", { size: usage })
                : t("User · {size} used", { size: usage })}
          </div>
          <DropdownMenuSeparator />
          <DropdownMenuItem onClick={() => setChangingPassword(true)}>
            <KeyRoundIcon /> {t("Change password")}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={() => setShowTwoFactor(true)}>
            <ShieldCheckIcon /> {t("Two-factor sign-in")}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={() => setShowAccounts(true)}>
            <Link2Icon /> {t("Sign-in methods")}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={() => setShowDevices(true)}>
            <MonitorSmartphoneIcon /> {t("Devices")}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={() => setShowAppPasswords(true)}>
            <KeySquareIcon /> {t("App passwords")}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={() => setShowLogins(true)}>
            <HistoryIcon /> {t("My sign-in history")}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={() => setShowNotifications(true)}>
            <BellIcon /> {t("Notification settings")}
          </DropdownMenuItem>
          {canToggle && (
            <DropdownMenuSub>
              <DropdownMenuSubTrigger>
                {dark ? <MoonIcon /> : <SunIcon />} {t("Appearance")}
              </DropdownMenuSubTrigger>
              <DropdownMenuSubContent>
                <DropdownMenuRadioGroup value={mode} onValueChange={(v) => setMode(v as ThemeMode)}>
                  <DropdownMenuRadioItem value="system">{t("Use system setting")}</DropdownMenuRadioItem>
                  <DropdownMenuRadioItem value="light">{t("Light")}</DropdownMenuRadioItem>
                  <DropdownMenuRadioItem value="dark">{t("Dark")}</DropdownMenuRadioItem>
                </DropdownMenuRadioGroup>
              </DropdownMenuSubContent>
            </DropdownMenuSub>
          )}
          <DropdownMenuSub>
            <DropdownMenuSubTrigger>
              <LanguagesIcon /> {t("Language")}
              {lang === "zh-TW" && " · Language"}
            </DropdownMenuSubTrigger>
            <DropdownMenuSubContent>
              <DropdownMenuRadioGroup value={lang} onValueChange={(v) => setLang(v as Lang)}>
                {LANGS.map((l) => (
                  <DropdownMenuRadioItem key={l.id} value={l.id}>
                    {l.label}
                  </DropdownMenuRadioItem>
                ))}
              </DropdownMenuRadioGroup>
            </DropdownMenuSubContent>
          </DropdownMenuSub>
          <DropdownMenuSeparator />
          <DropdownMenuItem onClick={logout}>
            <LogOutIcon /> {t("Sign out")}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      {changingPassword && <ChangePasswordDialog onClose={() => setChangingPassword(false)} />}
      {showAccounts && <LinkedAccountsDialog onClose={() => setShowAccounts(false)} />}
      {showDevices && <DevicesDialog onClose={() => setShowDevices(false)} />}
      {showAppPasswords && <AppPasswordsDialog onClose={() => setShowAppPasswords(false)} />}
      {showTwoFactor && <TwoFactorDialog onClose={() => setShowTwoFactor(false)} />}
      {showNotifications && <NotificationSettingsDialog onClose={() => setShowNotifications(false)} />}
      {showLogins && (
        <LoginLogDialog
          title={t("My sign-in history")}
          description={t('If you see an IP address or device you don\'t recognize, sign it out under "Devices". If you sign in with a password, change it too.')}
          onClose={() => setShowLogins(false)}
        />
      )}
    </>
  );
}
