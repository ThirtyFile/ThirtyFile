import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Switch } from "@base-ui/react/switch";
import { ActivityIcon, CalendarClockIcon, DatabaseIcon, KeyRoundIcon, ShieldCheckIcon, DownloadIcon, FilesIcon, FolderSyncIcon, GlobeIcon, HardDriveIcon, HistoryIcon, LanguagesIcon, Link2Icon, Loader2Icon, LogInIcon, RefreshCwIcon, SettingsIcon, Trash2Icon, type LucideIcon } from "lucide-react";
import { Link } from "react-router";
import { ActivityLog } from "@/components/logs/ActivityLog";
import { ShareAccessLog } from "@/components/logs/ShareAccessLog";
import { LoginLog } from "@/components/logs/LoginLog";
import { StorageLocations } from "@/components/StorageLocations";
import { toast } from "sonner";
import { api, triggerDownload, type LogArchive, type DefaultLang, type LogSettings, type SystemSettingsReq } from "@/api";
import { Skeleton } from "@/components/ui/skeleton";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ConfirmDialog } from "@/components/dialogs";
import { Frame, ToolButton } from "@/components/Frame";
import { controlPanelItem, type ControlPanelKey, useSettingsSearch } from "@/lib/controlPanel";
import { cn, formatBytes, formatDate } from "@/lib/utils";
import { usePersisted } from "@/lib/session";
import { LANGS, locale, t, tServer } from "@/lib/i18n";
import { invalidateFiles } from "@/lib/queries";

export function Toggle({ checked, disabled, onChange, label }: { checked: boolean; disabled?: boolean; onChange(v: boolean): void; label: string }) {
  return (
    <Switch.Root
      checked={checked}
      disabled={disabled}
      onCheckedChange={onChange}
      aria-label={label}
      className={cn(
        "relative inline-flex h-5 w-9 shrink-0 items-center rounded-full border border-transparent transition-colors outline-none focus-visible:ring-3 focus-visible:ring-ring disabled:opacity-50",
        checked ? "bg-brand" : "bg-input",
      )}
    >
      <Switch.Thumb className={cn("block size-4 rounded-full bg-white shadow transition-transform", checked ? "translate-x-4" : "translate-x-0.5")} />
    </Switch.Root>
  );
}

export function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="grid gap-3">
      <h2 className="text-xs font-medium text-muted-foreground">{title}</h2>
      <div className="overflow-hidden rounded-lg border bg-card">{children}</div>
    </section>
  );
}

/** Settings page under the control panel: the address bar shows "Control panel › item", and going up returns to the control panel */
export function SettingsFrame({
  item,
  onRefresh,
  footer,
  children,
}: {
  item: ControlPanelKey;
  onRefresh(): void;
  footer?: React.ReactNode;
  children: React.ReactNode;
}) {
  const { title, icon } = controlPanelItem(item);
  const searchSettings = useSettingsSearch();
  return (
    <Frame
      toolbar={<ToolButton icon={RefreshCwIcon} label={t("Refresh")} showLabel onClick={onRefresh} />}
      crumbs={[{ label: t("Control panel"), to: "/admin" }, { label: title }]}
      icon={icon as LucideIcon}
      upTo="/admin"
      searchPlaceholder={t("Search settings")}
      onSearch={searchSettings}
      footer={footer ?? <span>{t("Only administrators can make changes")}</span>}
    >
      <div className="min-h-0 flex-1 overflow-auto">
        <div className="mx-auto grid max-w-3xl gap-8 p-6">{children}</div>
      </div>
    </Frame>
  );
}

function useSystem() {
  return useQuery({ queryKey: ["system"], queryFn: api.systemSettings });
}

const GB = 1024 ** 3;

/** Capacity input in GB (decimals allowed); blank means unlimited */
function QuotaInput({ value, saving, onSave }: { value: number; saving: boolean; onSave(bytes: number): void }) {
  const [text, setText] = useState(value ? String(+(value / GB).toFixed(2)) : "");
  const bytes = text.trim() ? Math.round(Number(text) * GB) : 0;
  const invalid = text.trim() !== "" && (!Number.isFinite(Number(text)) || Number(text) < 0);
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (!invalid && bytes !== value) onSave(bytes);
  };
  return (
    <form className="flex shrink-0 items-center gap-2" onSubmit={submit}>
      <div className="relative">
        <Input
          aria-label={t("Personal space size for new users (GB)")}
          inputMode="decimal"
          className="h-8 w-28 pr-9 text-right tabular-nums"
          placeholder={t("Unlimited")}
          value={text}
          aria-invalid={invalid}
          onChange={(e) => setText(e.target.value)}
        />
        <span className="pointer-events-none absolute top-1/2 right-2.5 -translate-y-1/2 text-xs text-muted-foreground">GB</span>
      </div>
      <Button type="submit" size="sm" disabled={saving || invalid || bytes === value}>
        {t("Save")}
      </Button>
    </form>
  );
}

/** Minimum password length (6 to 64 characters) */
function MinPasswordInput({ value, saving, onSave }: { value: number; saving: boolean; onSave(n: number): void }) {
  const [text, setText] = useState(String(value));
  const n = Number(text);
  const invalid = !/^\d+$/.test(text.trim()) || n < 6 || n > 64;
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (!invalid && n !== value) onSave(n);
  };
  return (
    <form className="flex shrink-0 items-center gap-2" onSubmit={submit}>
      <Input
        aria-label={t("Minimum password length")}
        inputMode="numeric"
        className="h-8 w-20 text-right tabular-nums"
        value={text}
        aria-invalid={invalid}
        onChange={(e) => setText(e.target.value)}
      />
      <Button type="submit" size="sm" disabled={saving || invalid || n === value}>
        {t("Save")}
      </Button>
    </form>
  );
}

/** How often folder spaces are checked for changes (minutes, 0 = only by hand) */
function ScanIntervalInput({ value, saving, onSave }: { value: number; saving: boolean; onSave(minutes: number): void }) {
  const [text, setText] = useState(String(value));
  const minutes = Number(text);
  const invalid = !/^\d+$/.test(text.trim()) || minutes > 1440;
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (!invalid && minutes !== value) onSave(minutes);
  };
  return (
    <form className="flex shrink-0 items-center gap-2" onSubmit={submit}>
      <div className="relative">
        <Input
          aria-label={t("Check folder spaces every (minutes)")}
          inputMode="numeric"
          className="h-8 w-28 pr-12 text-right tabular-nums"
          value={text}
          aria-invalid={invalid}
          onChange={(e) => setText(e.target.value)}
        />
        <span className="pointer-events-none absolute top-1/2 right-2.5 -translate-y-1/2 text-xs text-muted-foreground">{t("min")}</span>
      </div>
      <Button type="submit" size="sm" disabled={saving || invalid || minutes === value}>
        {t("Save")}
      </Button>
    </form>
  );
}

/** A whole number from 0 to `max` with its unit, saved on its own (module level, so it doesn't lose focus on re-render) */
function CountInput(props: { value: number; max: number; unit: string; label: string; saving: boolean; onSave(n: number): void }) {
  const [text, setText] = useState(String(props.value));
  const n = Number(text);
  const invalid = !/^\d+$/.test(text.trim()) || n > props.max;
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (!invalid && n !== props.value) props.onSave(n);
  };
  return (
    <form className="flex shrink-0 items-center gap-2" onSubmit={submit}>
      <div className="relative">
        <Input
          aria-label={props.label}
          inputMode="numeric"
          className="h-8 w-32 pr-16 text-right tabular-nums"
          value={text}
          aria-invalid={invalid}
          onChange={(e) => setText(e.target.value)}
        />
        <span className="pointer-events-none absolute top-1/2 right-2.5 -translate-y-1/2 text-xs text-muted-foreground">{props.unit}</span>
      </div>
      <Button type="submit" size="sm" disabled={props.saving || invalid || n === props.value}>
        {t("Save")}
      </Button>
    </form>
  );
}

/** Longest expiry allowed for share links (days, 0 = no limit) */
function MaxDaysInput({ value, saving, onSave }: { value: number; saving: boolean; onSave(days: number): void }) {
  const [text, setText] = useState(value ? String(value) : "");
  const days = text.trim() ? Number(text) : 0;
  const invalid = text.trim() !== "" && (!/^\d+$/.test(text.trim()) || days > 3650);
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (!invalid && days !== value) onSave(days);
  };
  return (
    <form className="flex shrink-0 items-center gap-2" onSubmit={submit}>
      <div className="relative">
        <Input
          aria-label={t("Share links must expire within (days)")}
          inputMode="numeric"
          className="h-8 w-28 pr-12 text-right tabular-nums"
          placeholder={t("No limit")}
          value={text}
          aria-invalid={invalid}
          onChange={(e) => setText(e.target.value)}
        />
        <span className="pointer-events-none absolute top-1/2 right-2.5 -translate-y-1/2 text-xs text-muted-foreground">{t("days")}</span>
      </div>
      <Button type="submit" size="sm" disabled={saving || invalid || days === value}>
        {t("Save")}
      </Button>
    </form>
  );
}

/** Site URL input (module level, so it doesn't lose focus on re-render) */
function PublicUrlInput({ value, saving, onSave }: { value: string; saving: boolean; onSave(url: string): void }) {
  const [text, setText] = useState(value);
  const url = text.trim().replace(/\/+$/, "");
  const invalid = url !== "" && !/^https?:\/\/[a-z0-9.\-:[\]]+$/i.test(url);
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (!invalid && url !== value) onSave(url);
  };
  return (
    <form className="mt-3 grid gap-1.5" onSubmit={submit}>
      <div className="flex flex-wrap gap-2">
        <Input
          aria-label={t("Site URL")}
          className="h-8 min-w-60 flex-1 font-mono text-xs"
          placeholder={t("Not set (using the current address {url})", { url: location.origin })}
          value={text}
          aria-invalid={invalid}
          onChange={(e) => setText(e.target.value)}
        />
        <Button type="button" variant="outline" size="sm" onClick={() => setText(location.origin)} title={t("Fill in the browser's current address")}>
          {t("Use current address")}
        </Button>
        <Button type="submit" size="sm" disabled={saving || invalid || url === value}>
          {t("Save")}
        </Button>
      </div>
      {invalid ? (
        <p className="text-xs text-destructive">
          {t("Invalid format: enter only http:// or https:// followed by a domain or IP (a port is allowed), without a path.")}
        </p>
      ) : (
        <p className="text-xs text-muted-foreground">
          {t("Share links will look like")} <span className="font-mono">{url || location.origin}/share/Ab3dE6gH9k</span>
        </p>
      )}
    </form>
  );
}

export function GeneralSettingsPage() {
  const qc = useQueryClient();
  const q = useSystem();
  const [confirmDisable, setConfirmDisable] = useState(false);
  const [confirmLinksOff, setConfirmLinksOff] = useState(false);

  const save = useMutation({
    mutationFn: (req: SystemSettingsReq) => api.updateSystemSettings(req),
    onSuccess: (data) => {
      qc.setQueryData(["system"], data);
      // The left-hand menu and permissions both depend on the space list and me
      invalidateFiles(qc);
      toast.success(t("System settings updated"));
    },
    onError: (e) => toast.error(e.message),
  });

  return (
    <SettingsFrame item="general" onRefresh={() => q.refetch()}>
      {!q.data ? (
        <Skeleton className="h-40" />
      ) : (
        <>
          <Section title={t("Website")}>
            <div className="flex items-start gap-4 p-4">
              <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-emerald-500/10 text-emerald-600 dark:text-emerald-300">
                <GlobeIcon className="size-4" />
              </span>
              <div className="min-w-0 flex-1">
                <div className="font-medium">{t("Site URL")}</div>
                <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                  {t("The address other people use to reach this site, such as https://drive.example.com or http://192.168.1.10:8080. Share links are generated with this address. If it isn't set, the browser's current address is used; if you opened the site on the server itself (127.0.0.1), others won't be able to open the generated links.")}
                </p>
                <PublicUrlInput
                  key={q.data.public_url}
                  value={q.data.public_url}
                  saving={save.isPending}
                  onSave={(url) => save.mutate({ public_url: url })}
                />
              </div>
            </div>
            <div className="flex items-start gap-4 border-t p-4">
              <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-amber-500/10 text-amber-600 dark:text-amber-300">
                <LanguagesIcon className="size-4" />
              </span>
              <div className="min-w-0 flex-1">
                <div className="font-medium">{t("Default language")}</div>
                <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                  {t("The interface language for people who haven't chosen one themselves. \"Follow browser\" uses the language set in each person's browser. Everyone can still switch languages on the sign-in page or in the account menu, and their own choice takes priority.")}
                </p>
                <div className="mt-3 flex flex-wrap gap-2" role="radiogroup" aria-label={t("Default language")}>
                  {([["auto", t("Follow browser")], ...LANGS.map((l) => [l.id, l.label])] as [DefaultLang, string][]).map(([id, label]) => (
                    <button
                      key={id}
                      type="button"
                      role="radio"
                      aria-checked={q.data.default_lang === id}
                      disabled={save.isPending}
                      onClick={() => q.data.default_lang !== id && save.mutate({ default_lang: id })}
                      className={cn(
                        "flex h-8 items-center gap-2 rounded-lg border px-3 text-[13px] hover:bg-muted disabled:opacity-60",
                        q.data.default_lang === id && "border-brand bg-brand/5 text-brand ring-2 ring-brand/20",
                      )}
                    >
                      {id === "auto" && <GlobeIcon className="size-3.5" />}
                      {label}
                    </button>
                  ))}
                </div>
              </div>
            </div>
          </Section>
          <Section title={t("Folders")}>
            <div className="flex items-start gap-4 p-4">
              <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-brand/10 text-brand">
                <FilesIcon className="size-4" />
              </span>
              <div className="min-w-0 flex-1">
                <div className="font-medium">{t("\"All files\" company space")}</div>
                <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                  {t("A folder shared by all users. Each user uploads and organizes files in it according to their own permissions (edit, delete); files in the shared folder don't count toward personal space quotas. Every user also has their own private \"My files\" folder that no one else can see.")}
                </p>
                {!q.data.shared_enabled && (
                  <p className="mt-2 text-xs text-amber-600 dark:text-amber-400">
                    {t("Currently turned off: users can't see \"All files\", and share links inside it are temporarily disabled. The files are kept; turn it back on to restore access.")}
                  </p>
                )}
              </div>
              <Toggle
                label={t("Turn on the \"All files\" company space")}
                checked={q.data.shared_enabled}
                disabled={save.isPending}
                onChange={(v) => (v ? save.mutate({ shared_enabled: true }) : setConfirmDisable(true))}
              />
            </div>
            <div className="flex items-start gap-4 border-t p-4">
              <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-violet-500/10 text-violet-600 dark:text-violet-300">
                <DatabaseIcon className="size-4" />
              </span>
              <div className="min-w-0 flex-1">
                <div className="font-medium">{t("Allow regular users to create team spaces")}</div>
                <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                  {t("When off, only administrators can create team spaces. When on, users can create their own spaces and invite members; storage limits are still set by administrators in \"Space management\".")}
                </p>
              </div>
              <Toggle
                label={t("Allow regular users to create team spaces")}
                checked={q.data.allow_user_drives}
                disabled={save.isPending}
                onChange={(v) => save.mutate({ allow_user_drives: v })}
              />
            </div>
          </Section>
        </>
      )}
      {q.data && (
        <Section title={t("Users")}>
          <div className="flex flex-wrap items-start gap-4 p-4">
            <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-sky-500/10 text-sky-600 dark:text-sky-300">
              <HardDriveIcon className="size-4" />
            </span>
            <div className="min-w-0 flex-1">
              <div className="font-medium">{t("Personal space size for new users")}</div>
              <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                {t("The default storage limit for \"My files\" when adding a user; you can still adjust it for each user. Leave blank for unlimited. Changes only affect users added afterward; adjust existing users' space in \"Users\".")}
              </p>
              <p className="mt-1 text-xs text-muted-foreground">
                {t("Current setting: {value}", { value: q.data.default_user_quota ? formatBytes(q.data.default_user_quota) : t("Unlimited") })}
              </p>
            </div>
            <QuotaInput
              key={q.data.default_user_quota}
              value={q.data.default_user_quota}
              saving={save.isPending}
              onSave={(bytes) => save.mutate({ default_user_quota: bytes })}
            />
          </div>
          <div className="flex flex-wrap items-start gap-4 border-t p-4">
            <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-emerald-500/10 text-emerald-600 dark:text-emerald-300">
              <ShieldCheckIcon className="size-4" />
            </span>
            <div className="min-w-0 flex-1">
              <div className="font-medium">{t("Require two-factor sign-in for password accounts")}</div>
              <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                {t("Signing in with a password also asks for a code from an authenticator app. People who haven't set it up are asked to right after their password, before they get in, and can't turn it off. Sign-in with Microsoft, Google or GitHub relies on that provider, and app passwords keep working. Devices already signed in stay signed in.")}
              </p>
            </div>
            <Toggle
              label={t("Require two-factor sign-in for password accounts")}
              checked={q.data.require_two_factor}
              disabled={save.isPending}
              onChange={(v) => save.mutate({ require_two_factor: v })}
            />
          </div>
          <div className="flex flex-wrap items-start gap-4 border-t p-4">
            <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-amber-500/10 text-amber-600 dark:text-amber-300">
              <KeyRoundIcon className="size-4" />
            </span>
            <div className="min-w-0 flex-1">
              <div className="font-medium">{t("Minimum password length")}</div>
              <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                {t("The shortest password people can choose, from 6 to 64 characters. It applies to new and changed passwords; existing passwords keep working.")}
              </p>
            </div>
            <MinPasswordInput
              key={q.data.min_password_length}
              value={q.data.min_password_length}
              saving={save.isPending}
              onSave={(n) => save.mutate({ min_password_length: n })}
            />
          </div>
        </Section>
      )}
      {q.data && (
        <Section title={t("Folder spaces")}>
          <div className="flex flex-wrap items-start gap-4 p-4">
            <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-emerald-500/10 text-emerald-600 dark:text-emerald-300">
              <FolderSyncIcon className="size-4" />
            </span>
            <div className="min-w-0 flex-1">
              <div className="font-medium">{t("Check for changes made on the server")}</div>
              <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                {t("Folder spaces show folders on the server. Changes made there (for example over SMB) appear when someone opens the folder, and all folders are checked this often. 0 = only when someone opens a folder or clicks \"Check for changes\".")}
              </p>
            </div>
            <ScanIntervalInput
              key={q.data.scan_minutes}
              value={q.data.scan_minutes}
              saving={save.isPending}
              onSave={(minutes) => save.mutate({ scan_minutes: minutes })}
            />
          </div>
        </Section>
      )}
      {q.data && (
        <Section title={t("Share links")}>
          <div className="flex items-start gap-4 p-4">
            <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-teal-500/10 text-teal-700 dark:text-teal-300">
              <Link2Icon className="size-4" />
            </span>
            <div className="min-w-0 flex-1">
              <div className="font-medium">{t("Allow public share links")}</div>
              <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                {t("Anyone with a link can open the linked file or folder without an account. When off, no one can create links, and existing links stop working until they're allowed again; they aren't deleted. Find and revoke single links in \"All share links\".")}
              </p>
              <Link to="/admin/shares" className="mt-2 inline-block text-xs text-brand hover:underline">
                {t("All share links")}
              </Link>
            </div>
            <Toggle
              label={t("Allow public share links")}
              checked={q.data.public_links}
              disabled={save.isPending}
              onChange={(v) => (v ? save.mutate({ public_links: true }) : setConfirmLinksOff(true))}
            />
          </div>
          <div className="flex items-start gap-4 border-t p-4">
            <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-amber-500/10 text-amber-600 dark:text-amber-300">
              <KeyRoundIcon className="size-4" />
            </span>
            <div className="min-w-0 flex-1">
              <div className="font-medium">{t("Require a password")}</div>
              <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                {t("New links must have a password, and a password can't be removed from a link. Links created earlier keep working as they are.")}
              </p>
            </div>
            <Toggle
              label={t("Require a password")}
              checked={q.data.share_password_required}
              disabled={save.isPending}
              onChange={(v) => save.mutate({ share_password_required: v })}
            />
          </div>
          <div className="flex flex-wrap items-start gap-4 border-t p-4">
            <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-sky-500/10 text-sky-600 dark:text-sky-300">
              <CalendarClockIcon className="size-4" />
            </span>
            <div className="min-w-0 flex-1">
              <div className="font-medium">{t("Longest expiry")}</div>
              <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                {t("New and changed links must expire within this many days. Leave blank to allow links that never expire. Links created earlier keep their expiry.")}
              </p>
            </div>
            <MaxDaysInput
              key={q.data.share_max_days}
              value={q.data.share_max_days}
              saving={save.isPending}
              onSave={(days) => save.mutate({ share_max_days: days })}
            />
          </div>
        </Section>
      )}
      {q.data && (
        <Section title={t("Earlier versions of files")}>
          <div className="flex flex-wrap items-start gap-4 border-b p-4">
            <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-sky-500/10 text-sky-600 dark:text-sky-300">
              <HistoryIcon className="size-4" />
            </span>
            <div className="min-w-0 flex-1">
              <div className="font-medium">{t("Versions kept per file")}</div>
              <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                {t("When a file is saved over in the editor, replaced by an upload or restored, the content it had is kept as an earlier version, which people can open, download or restore from the details pane. 0 = don't keep versions (those already kept are removed within the hour). Earlier versions don't count toward the spaces' sizes.")}
              </p>
            </div>
            <CountInput
              key={q.data.version_keep}
              value={q.data.version_keep}
              max={1000}
              unit={t("versions")}
              label={t("Versions kept per file")}
              saving={save.isPending}
              onSave={(n) => save.mutate({ version_keep: n })}
            />
          </div>
          <div className="flex flex-wrap items-start gap-4 p-4">
            <span className="size-8 shrink-0" />
            <div className="min-w-0 flex-1">
              <div className="font-medium">{t("Keep versions for")}</div>
              <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                {t("Days a version is kept after it was replaced. 0 = no time limit (only the number of versions counts).")}
              </p>
            </div>
            <CountInput
              key={q.data.version_days}
              value={q.data.version_days}
              max={3650}
              unit={t("days")}
              label={t("Keep versions for (days)")}
              saving={save.isPending}
              onSave={(n) => save.mutate({ version_days: n })}
            />
          </div>
        </Section>
      )}
      {confirmLinksOff && (
        <ConfirmDialog
          title={t("Turn off public share links?")}
          description={t("All existing links stop working right away, and no one can create new ones. The links aren't deleted: allow public links again and they work as before.")}
          confirmText={t("Turn off")}
          destructive
          onClose={() => setConfirmLinksOff(false)}
          onConfirm={async () => {
            await save.mutateAsync({ public_links: false });
            setConfirmLinksOff(false);
          }}
        />
      )}
      {confirmDisable && (
        <ConfirmDialog
          title={t("Turn off the \"All files\" company space?")}
          description={t("Once turned off, no user can see \"All files\", and share links inside it are temporarily disabled. No files are deleted; turn it back on to restore access.")}
          confirmText={t("Turn off")}
          destructive
          onClose={() => setConfirmDisable(false)}
          onConfirm={async () => {
            await save.mutateAsync({ shared_enabled: false });
            setConfirmDisable(false);
          }}
        />
      )}
    </SettingsFrame>
  );
}

export function StorageSettingsPage() {
  const qc = useQueryClient();
  return (
    <SettingsFrame item="storage" onRefresh={() => qc.invalidateQueries({ queryKey: ["storage-locations"] })}>
      <Section title={t("Storage locations")}>
        <StorageLocations />
      </Section>
    </SettingsFrame>
  );
}

export function UsageSettingsPage() {
  const q = useSystem();
  const s = q.data?.stats;
  const stats: [string, string, string?][] = s
    ? [
        [t("Users"), t("{n} user|{n} users", { n: s.users }), t("{n} group|{n} groups", { n: s.groups })],
        [t("Personal space"), formatBytes(s.personal_bytes), t("{n} file|{n} files", { n: s.personal_files })],
        [t("All files (company)"), formatBytes(s.shared_bytes), t("{n} file|{n} files", { n: s.shared_files })],
        [t("Team space"), formatBytes(s.team_bytes), `${t("{n} space|{n} spaces", { n: s.team_drives })} · ${t("{n} file|{n} files", { n: s.team_files })}`],
        [t("Trash"), formatBytes(s.trash_bytes)],
        [t("Earlier versions of files"), formatBytes(s.version_bytes), t("Not counted toward the spaces' sizes")],
        [t("Actual storage used"), formatBytes(s.stored_bytes), `${t("Identical content stored once")} · ${t("{n} share link|{n} share links", { n: s.share_links })}`],
      ]
    : [];
  return (
    <SettingsFrame item="usage" onRefresh={() => q.refetch()} footer={<span>{t("Includes all spaces and the trash")}</span>}>
      {!q.data ? (
        <Skeleton className="h-40" />
      ) : (
        <Section title={t("Storage usage")}>
          <dl className="grid grid-cols-2 sm:grid-cols-3">
            {stats.map(([label, value, hint]) => (
              <div key={label} className="border-r border-b p-4 [&:nth-child(2n)]:max-sm:border-r-0 sm:[&:nth-child(3n)]:border-r-0">
                <dt className="text-xs text-muted-foreground">{label}</dt>
                <dd className="mt-1 text-lg font-medium tabular-nums">{value}</dd>
                {hint && <dd className="text-[11px] text-muted-foreground">{hint}</dd>}
              </div>
            ))}
          </dl>
        </Section>
      )}
    </SettingsFrame>
  );
}

type LogTab = "activity" | "login" | "share";
const LOG_TABS: Record<LogTab, { query: string; footer: string }> = {
  activity: { query: "activity", footer: t("Actions users performed in the system") },
  login: { query: "login-log", footer: t("Records of successful and failed sign-ins, sign-outs, and password changes") },
  share: {
    query: "share-access",
    footer: t("Records of public share links being opened, previewed, and downloaded"),
  },
};

export function ActivitySettingsPage() {
  const qc = useQueryClient();
  const [tab, setTab] = usePersisted<LogTab>("tf-log-tab", "activity");
  const { title, icon } = controlPanelItem("activity");
  const searchSettings = useSettingsSearch();
  const tabBtn = (key: LogTab, label: string, Icon: LucideIcon) => (
    <button
      type="button"
      role="tab"
      aria-selected={tab === key}
      onClick={() => setTab(key)}
      className={cn(
        "flex h-9 items-center gap-1.5 border-b-2 px-3 text-[13px]",
        tab === key ? "border-brand font-medium text-foreground" : "border-transparent text-muted-foreground hover:text-foreground",
      )}
    >
      <Icon className="size-4" /> {label}
    </button>
  );
  return (
    <Frame
      toolbar={
        <>
          <ToolButton icon={RefreshCwIcon} label={t("Refresh")} showLabel onClick={() => qc.invalidateQueries({ queryKey: [LOG_TABS[tab].query] })} />
          <span className="flex-1" />
          <Link
            to="/admin/logs"
            className="flex h-8 items-center gap-1.5 rounded-md px-2.5 text-xs text-muted-foreground hover:bg-muted hover:text-foreground"
          >
            <SettingsIcon className="size-4" /> {t("Log settings")}
          </Link>
        </>
      }
      crumbs={[{ label: t("Control panel"), to: "/admin" }, { label: title }]}
      icon={icon as LucideIcon}
      upTo="/admin"
      searchPlaceholder={t("Search settings")}
      onSearch={searchSettings}
      footer={<span>{LOG_TABS[tab].footer}</span>}
    >
      <div role="tablist" className="flex shrink-0 gap-1 border-b px-3">
        {tabBtn("activity", t("Activity log"), ActivityIcon)}
        {tabBtn("login", t("Sign-in log"), LogInIcon)}
        {tabBtn("share", t("Share link access"), Link2Icon)}
      </div>
      {tab === "activity" ? (
        <ActivityLog className="min-h-0 flex-1" />
      ) : tab === "login" ? (
        <LoginLog admin className="min-h-0 flex-1" />
      ) : (
        <ShareAccessLog admin className="min-h-0 flex-1" />
      )}
    </Frame>
  );
}

const KIND_LABEL: Record<LogArchive["kind"], string> = {
  activity: t("Activity log"),
  share_access: t("Share link access log"),
  login_log: t("Sign-in log"),
};

function days(n: number) {
  return n === 0 ? t("no cleanup") : t("{n} day|{n} days", { n });
}

/** Retention days input (at module level, so the input isn't recreated and doesn't lose focus on re-render) */
function DayInput({ label, hint, value, onChange }: { label: string; hint: string; value: number; onChange(v: number): void }) {
  return (
    <label className="grid gap-1.5">
      <span className="text-[13px] font-medium">{label}</span>
      <div className="flex items-center gap-2">
        <Input
          type="number"
          min={0}
          max={36500}
          className="h-8 w-28 text-right tabular-nums"
          value={value}
          onChange={(e) => onChange(Math.max(0, Math.floor(Number(e.target.value) || 0)))}
        />
        <span className="text-xs text-muted-foreground">{t("days")}</span>
      </div>
      <span className="text-xs text-muted-foreground">{hint}</span>
    </label>
  );
}

/** Log settings: retention days, archive or delete, archive retention, whether to record guest info; plus the archive list */
export function LogSettingsPage() {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["log-status"], queryFn: api.logStatus });
  const [draft, setDraft] = useState<LogSettings | null>(null);
  const [deleting, setDeleting] = useState<LogArchive | null>(null);
  const cur = draft ?? q.data?.settings;
  const changed = !!draft && JSON.stringify(draft) !== JSON.stringify(q.data?.settings);

  const save = useMutation({
    mutationFn: (s: LogSettings) => api.updateLogSettings(s),
    onSuccess: (data) => {
      qc.setQueryData(["log-status"], data);
      setDraft(null);
      toast.success(t("Log settings updated"));
    },
    onError: (e) => toast.error(e.message),
  });
  const archive = useMutation({
    mutationFn: api.archiveLogsNow,
    onSuccess: (data) => {
      qc.setQueryData(["log-status"], data);
      qc.invalidateQueries({ queryKey: ["activity"] });
      qc.invalidateQueries({ queryKey: ["share-access"] });
      toast.success(data.summary ? tServer(data.summary) : t("Archiving complete"));
    },
    onError: (e) => toast.error(e.message),
  });

  const set = (patch: Partial<LogSettings>) => cur && setDraft({ ...cur, ...patch });
  const d = q.data;
  const modes: [boolean, string, string][] = [
    [true, t("Compress and archive"), t("Moved out of the database and saved as compressed files you can download when needed")],
    [false, t("Delete directly"), t("Not kept; frees space in both the database and on disk")],
  ];
  return (
    <SettingsFrame item="logs" onRefresh={() => q.refetch()}>
      {!d || !cur ? (
        <Skeleton className="h-40" />
      ) : (
        <>
          <Section title={t("Current status")}>
            <dl className="grid grid-cols-2 sm:grid-cols-5">
              {[
                [
                  t("Activity log"),
                  t("{n} entry|{n} entries", { n: d.activity.rows }),
                  d.activity.oldest ? t("Oldest: {date}", { date: formatDate(d.activity.oldest) }) : "—",
                ],
                [
                  t("Share link access"),
                  t("{n} entry|{n} entries", { n: d.share_access.rows }),
                  d.share_access.oldest ? t("Oldest: {date}", { date: formatDate(d.share_access.oldest) }) : "—",
                ],
                [
                  t("Sign-in log"),
                  t("{n} entry|{n} entries", { n: d.login_log.rows }),
                  d.login_log.oldest ? t("Oldest: {date}", { date: formatDate(d.login_log.oldest) }) : "—",
                ],
                [t("Archives"), t("{n}", { n: d.archives.length }), formatBytes(d.archive_bytes)],
                [t("Last cleanup"), d.last_run ? formatDate(d.last_run) : t("Not run yet"), t("Runs automatically every day")],
              ].map(([k, v, hint]) => (
                <div key={k} className="border-r border-b p-4 [&:nth-child(2n)]:max-sm:border-r-0 sm:[&:nth-child(5n)]:border-r-0">
                  <dt className="text-xs text-muted-foreground">{k}</dt>
                  <dd className="mt-1 text-lg font-medium tabular-nums">{v}</dd>
                  <dd className="text-[11px] text-muted-foreground">{hint}</dd>
                </div>
              ))}
            </dl>
          </Section>

          <Section title={t("Retention and archiving")}>
            <form
              className="grid gap-5 p-4"
              onSubmit={(e) => {
                e.preventDefault();
                if (changed && draft) save.mutate(draft);
              }}
            >
              <div className="grid gap-5 sm:grid-cols-2">
                <DayInput
                  label={t("Activity log retention (days)")}
                  hint={t("Days to keep in the database; older entries are cleaned up once a day. 0 = no cleanup")}
                  value={cur.activity_days}
                  onChange={(v) => set({ activity_days: v })}
                />
                <DayInput
                  label={t("Share link access log retention (days)")}
                  hint={t("Same as above, for access records of public share links")}
                  value={cur.share_days}
                  onChange={(v) => set({ share_days: v })}
                />
                <DayInput
                  label={t("Sign-in log retention (days)")}
                  hint={t("Successful or failed sign-ins, sign-outs, and password changes; security audits often require keeping these for a year or more")}
                  value={cur.login_days}
                  onChange={(v) => set({ login_days: v })}
                />
              </div>
              <div className="grid gap-2">
                <span className="text-[13px] font-medium">{t("Entries older than the retention period")}</span>
                <div className="flex flex-wrap gap-2">
                  {modes.map(([v, label, hint]) => (
                    <button
                      key={String(v)}
                      type="button"
                      aria-pressed={cur.archive === v}
                      onClick={() => set({ archive: v })}
                      className={cn(
                        "grid max-w-64 gap-0.5 rounded-lg border px-3 py-2 text-left",
                        cur.archive === v ? "border-brand bg-brand/5" : "hover:bg-muted/60",
                      )}
                    >
                      <span className="text-[13px] font-medium">{label}</span>
                      <span className="text-xs text-muted-foreground">{hint}</span>
                    </button>
                  ))}
                </div>
              </div>
              {cur.archive && (
                <DayInput
                  label={t("Archive retention (days)")}
                  hint={t("Archives older than this are deleted automatically. 0 = keep forever")}
                  value={cur.archive_keep_days}
                  onChange={(v) => set({ archive_keep_days: v })}
                />
              )}
              <div className="flex items-start gap-4">
                <div className="min-w-0 flex-1">
                  <div className="text-[13px] font-medium">{t("Record share link visitors' IP address and browser")}</div>
                  <p className="mt-1 text-xs text-muted-foreground">{t("Used to investigate unusual downloads; when off, only the time and event are recorded. Follow your company's privacy policy when deciding whether to turn this on.")}</p>
                </div>
                <Toggle label={t("Record visitors' IP address and browser")} checked={cur.record_visitor} onChange={(v) => set({ record_visitor: v })} />
              </div>
              <div className="flex flex-wrap items-center gap-2 border-t pt-4">
                <Button type="submit" size="sm" disabled={!changed || save.isPending}>
                  {t("Save settings")}
                </Button>
                {changed && (
                  <Button type="button" size="sm" variant="ghost" onClick={() => setDraft(null)}>
                    {t("Discard changes")}
                  </Button>
                )}
                <span className="flex-1" />
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  disabled={archive.isPending || changed}
                  onClick={() => archive.mutate()}
                  title={changed ? t("Save your settings first") : t("Clean up now using the current settings instead of waiting for the daily schedule")}
                >
                  {archive.isPending && <Loader2Icon className="animate-spin" />}
                  {t("Clean up now")}
                </Button>
              </div>
              <p className="-mt-2 text-xs text-muted-foreground">
                {(() => {
                  const vars = { activity: days(d.settings.activity_days), login: days(d.settings.login_days), share: days(d.settings.share_days) };
                  if (!d.settings.archive) return t("Current settings: activity log {activity}, sign-in log {login}, share access {share}. Older entries are deleted.", vars);
                  if (!d.settings.archive_keep_days)
                    return t("Current settings: activity log {activity}, sign-in log {login}, share access {share}. Older entries are compressed and archived, and archives are kept forever.", vars);
                  return t("Current settings: activity log {activity}, sign-in log {login}, share access {share}. Older entries are compressed and archived, and archives are kept for {n} day.|Current settings: activity log {activity}, sign-in log {login}, share access {share}. Older entries are compressed and archived, and archives are kept for {n} days.", {
                    ...vars,
                    n: d.settings.archive_keep_days,
                  });
                })()}
              </p>
            </form>
          </Section>

          <Section title={t("Archives ({n})", { n: d.archives.length })}>
            {d.archives.length === 0 ? (
              <p className="p-6 text-center text-sm text-muted-foreground">{t("No archives yet")}</p>
            ) : (
              <table className="w-full border-collapse text-xs">
                <thead>
                  <tr className="border-b text-left text-muted-foreground">
                    <th className="px-3 py-2 font-normal">{t("Log type")}</th>
                    <th className="px-3 py-2 font-normal">{t("Period")}</th>
                    <th className="px-3 py-2 text-right font-normal">{t("Entries")}</th>
                    <th className="px-3 py-2 text-right font-normal max-sm:hidden">{t("Size")}</th>
                    <th className="w-24 px-3 py-2" />
                  </tr>
                </thead>
                <tbody>
                  {d.archives.map((a) => (
                    <tr key={a.id} className="border-b border-border/40 last:border-0">
                      <td className="px-3 py-2">{KIND_LABEL[a.kind]}</td>
                      <td className="px-3 py-2 whitespace-nowrap text-muted-foreground">
                        {formatDate(a.from_at)} – {formatDate(a.to_at)}
                      </td>
                      <td className="px-3 py-2 text-right tabular-nums">{a.rows.toLocaleString(locale)}</td>
                      <td className="px-3 py-2 text-right text-muted-foreground tabular-nums max-sm:hidden">{formatBytes(a.bytes)}</td>
                      <td className="px-3 py-2 text-right whitespace-nowrap">
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          title={t("Download (.jsonl.gz, one JSON entry per line)")}
                          aria-label={t("Download")}
                          onClick={() => triggerDownload(api.logArchiveUrl(a.id))}
                        >
                          <DownloadIcon />
                        </Button>
                        <Button variant="ghost" size="icon-sm" title={t("Delete archive")} aria-label={t("Delete")} onClick={() => setDeleting(a)}>
                          <Trash2Icon />
                        </Button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </Section>
        </>
      )}
      {deleting && (
        <ConfirmDialog
          title={t("Delete this archive?")}
          description={t("{kind} {from} – {to}, {n} entry. This can't be undone, so consider downloading it first.|{kind} {from} – {to}, {n} entries. This can't be undone, so consider downloading it first.", {
            kind: KIND_LABEL[deleting.kind],
            from: formatDate(deleting.from_at),
            to: formatDate(deleting.to_at),
            n: deleting.rows,
          })}
          confirmText={t("Delete permanently")}
          destructive
          onClose={() => setDeleting(null)}
          onConfirm={async () => {
            await api.deleteLogArchive(deleting.id);
            setDeleting(null);
            q.refetch();
          }}
        />
      )}
    </SettingsFrame>
  );
}
