import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArchiveIcon,
  CalendarClockIcon,
  DatabaseIcon,
  KeyRoundIcon,
  ShieldCheckIcon,
  FilesIcon,
  FolderOpenIcon,
  FolderSyncIcon,
  GlobeIcon,
  HardDriveIcon,
  HistoryIcon,
  LanguagesIcon,
  Link2Icon,
} from "lucide-react";
import { Link } from "react-router";
import { toast } from "sonner";
import { api, type DefaultLang, type SystemSettingsReq } from "@/api";
import { keys, queries } from "@/api/queryKeys";
import { Skeleton } from "@/components/ui/skeleton";
import { Pending } from "@/components/ErrorState";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ConfirmDialog } from "@/components/dialogs";
import { LocationSelect } from "@/components/LocationSelect";
import { cn, formatBytes } from "@/lib/utils";
import { LANGS, t } from "@/lib/i18n";
import { invalidateFiles } from "@/lib/queries";
import { Toggle, Section, SettingsFrame } from "@/admin/SettingsFrame";

const GB = 1024 ** 3;

/**
 * A number saved on its own with its Save button (module level, so it doesn't lose focus on re-render): a whole number
 * from `min` to `max`, or with `decimal` any number from 0 (a size in GB); `blank` lets it be left empty for 0
 */
function NumberInput(props: {
  value: number;
  saving: boolean;
  onSave(n: number): void;
  label: string;
  /** Shown inside the box, after the number */
  unit?: string;
  /** The box's width, and room on the right for the unit */
  width: string;
  min?: number;
  max?: number;
  decimal?: boolean;
  /** Empty stands for 0, and says this */
  blank?: string;
  /** The number typed is this many of what is saved (GB: 1024³ bytes) */
  scale?: number;
}) {
  const { value, blank, scale } = props;
  const [text, setText] = useState(blank !== undefined && !value ? "" : String(scale ? +(value / scale).toFixed(2) : value));
  const empty = blank !== undefined && !text.trim();
  const n = empty ? 0 : scale ? Math.round(Number(text) * scale) : Number(text);
  const invalid = empty ? false : props.decimal ? !Number.isFinite(Number(text)) || Number(text) < 0 : !/^\d+$/.test(text.trim()) || n < (props.min ?? 0) || n > (props.max ?? Infinity);
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (!invalid && n !== value) props.onSave(n);
  };
  const input = (
    <Input
      aria-label={props.label}
      inputMode={props.decimal ? "decimal" : "numeric"}
      className={cn("h-8 text-right tabular-nums", props.width)}
      placeholder={blank}
      value={text}
      aria-invalid={invalid}
      onChange={(e) => setText(e.target.value)}
    />
  );
  return (
    <form className="flex shrink-0 items-center gap-2" onSubmit={submit}>
      {props.unit ? (
        <div className="relative">
          {input}
          <span className="pointer-events-none absolute top-1/2 right-2.5 -translate-y-1/2 text-xs text-muted-foreground">{props.unit}</span>
        </div>
      ) : (
        input
      )}
      <Button type="submit" size="sm" disabled={props.saving || invalid || n === value}>
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
        <p className="text-xs text-destructive">{t("Invalid format: enter only http:// or https:// followed by a domain or IP (a port is allowed), without a path.")}</p>
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
  const q = useQuery(queries.system);
  const [confirmDisable, setConfirmDisable] = useState(false);
  const [confirmLinksOff, setConfirmLinksOff] = useState(false);

  const save = useMutation({
    mutationFn: (req: SystemSettingsReq) => api.updateSystemSettings(req),
    onSuccess: (data) => {
      qc.setQueryData(keys.system(), data);
      // The left-hand menu and permissions both depend on the space list and me
      invalidateFiles(qc);
      toast.success(t("System settings updated"));
    },
    onError: (e) => toast.error(e.message),
  });

  return (
    <SettingsFrame item="general" onRefresh={() => q.refetch()}>
      {!q.data ? (
        <Pending query={q} loading={<Skeleton className="h-40" />} />
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
                  {t(
                    "The address other people use to reach this site, such as https://drive.example.com or http://192.168.1.10:8080. Share links are generated with this address. If it isn't set, the browser's current address is used; if you opened the site on the server itself (127.0.0.1), others won't be able to open the generated links.",
                  )}
                </p>
                <PublicUrlInput key={q.data.public_url} value={q.data.public_url} saving={save.isPending} onSave={(url) => save.mutate({ public_url: url })} />
              </div>
            </div>
            <div className="flex items-start gap-4 border-t p-4">
              <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-amber-500/10 text-amber-600 dark:text-amber-300">
                <LanguagesIcon className="size-4" />
              </span>
              <div className="min-w-0 flex-1">
                <div className="font-medium">{t("Default language")}</div>
                <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                  {t(
                    "The interface language for people who haven't chosen one themselves. \"Follow browser\" uses the language set in each person's browser. Everyone can still switch languages on the sign-in page or in the account menu, and their own choice takes priority.",
                  )}
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
                <div className="font-medium">{t('"All files" company space')}</div>
                <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                  {t(
                    'A folder shared by all users. Each user uploads and organizes files in it according to their own permissions (edit, delete); files in the shared folder don\'t count toward personal space quotas. Each user can also have their own private "My files" folder that no one else can see.',
                  )}
                </p>
                {!q.data.shared_enabled && (
                  <p className="mt-2 text-xs text-amber-600 dark:text-amber-400">
                    {t('Currently turned off: users can\'t see "All files", and share links inside it are temporarily disabled. The files are kept; turn it back on to restore access.')}
                  </p>
                )}
              </div>
              <Toggle
                label={t('Turn on the "All files" company space')}
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
                  {t(
                    "When off, only administrators can create team spaces. When on, users can create their own spaces and invite members; storage limits are still set by administrators in Control panel › Spaces.",
                  )}
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
          <div className="flex items-start gap-4 p-4">
            <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-brand/10 text-brand">
              <FolderOpenIcon className="size-4" />
            </span>
            <div className="min-w-0 flex-1">
              <div className="font-medium">{t('Give new users a personal space ("My files")')}</div>
              <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                {t(
                  'When off, new users only see the spaces they\'re given access to, such as "All files" and team spaces. You can still give someone "My files" when adding them, or later under "Users". Existing users keep theirs.',
                )}
              </p>
            </div>
            <Toggle
              label={t('Give new users a personal space ("My files")')}
              checked={q.data.personal_spaces}
              disabled={save.isPending}
              onChange={(v) => save.mutate({ personal_spaces: v })}
            />
          </div>
          <div className="flex flex-wrap items-start gap-4 border-t p-4">
            <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-sky-500/10 text-sky-600 dark:text-sky-300">
              <ArchiveIcon className="size-4" />
            </span>
            <div className="min-w-0 flex-1 basis-60">
              <label htmlFor="personal-location" className="font-medium">
                {t('Location of new users\' "My files"')}
              </label>
              <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                {t(
                  'The storage location where new personal spaces are created. "Default location" follows the default under "Storage locations" at the time each user is added. If the location isn\'t available (a disk that isn\'t mounted), the user is still created, and their "My files" is created once it\'s available again.',
                )}
              </p>
            </div>
            <LocationSelect
              id="personal-location"
              className="max-w-full shrink-0"
              value={q.data.personal_location}
              blank="default"
              disabled={save.isPending || !q.data.personal_spaces}
              onChange={(v) => save.mutate({ personal_location: v })}
            />
          </div>
          <div className="flex flex-wrap items-start gap-4 border-t p-4">
            <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-sky-500/10 text-sky-600 dark:text-sky-300">
              <HardDriveIcon className="size-4" />
            </span>
            <div className="min-w-0 flex-1">
              <div className="font-medium">{t("Personal space size for new users")}</div>
              <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                {t(
                  'The default storage limit for "My files" when adding a user; you can still adjust it for each user. Leave blank for unlimited. Changes only affect users added afterward; adjust existing users\' space in "Users".',
                )}
              </p>
              <p className="mt-1 text-xs text-muted-foreground">
                {t("Current setting: {value}", { value: q.data.default_user_quota ? formatBytes(q.data.default_user_quota) : t("Unlimited") })}
              </p>
            </div>
            <NumberInput
              key={q.data.default_user_quota}
              value={q.data.default_user_quota}
              label={t("Personal space size for new users (GB)")}
              unit="GB"
              width="w-28 pr-9"
              decimal
              scale={GB}
              blank={t("Unlimited")}
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
                {t(
                  "Signing in with a password also asks for a code from an authenticator app. People who haven't set it up are asked to right after their password, before they get in, and can't turn it off. Sign-in with single sign-on (Microsoft, Google, GitHub or OpenID Connect) relies on that provider, and app passwords keep working. Devices already signed in stay signed in.",
                )}
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
            <NumberInput
              key={q.data.min_password_length}
              value={q.data.min_password_length}
              label={t("Minimum password length")}
              width="w-20"
              min={6}
              max={64}
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
                {t(
                  'Folder spaces show folders on the server. Changes made there (for example over SMB) appear when someone opens the folder, and all folders are checked this often. 0 = only when someone opens a folder or clicks "Check for changes".',
                )}
              </p>
            </div>
            <NumberInput
              key={q.data.scan_minutes}
              value={q.data.scan_minutes}
              label={t("Check folder spaces every (minutes)")}
              unit={t("min")}
              width="w-28 pr-12"
              max={1440}
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
                {t(
                  "Anyone with a link can open the linked file or folder without an account. When off, no one can create links, and existing links stop working until they're allowed again; they aren't deleted. Find and revoke single links in \"All share links\".",
                )}
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
            <Toggle label={t("Require a password")} checked={q.data.share_password_required} disabled={save.isPending} onChange={(v) => save.mutate({ share_password_required: v })} />
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
            <NumberInput
              key={q.data.share_max_days}
              value={q.data.share_max_days}
              label={t("Share links must expire within (days)")}
              unit={t("days")}
              width="w-28 pr-12"
              max={3650}
              blank={t("No limit")}
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
                {t(
                  "When a file is saved over in the editor, replaced by an upload or restored, the content it had is kept as an earlier version, which people can open, download or restore from the details pane. 0 = don't keep versions (those already kept are removed within the hour). Earlier versions don't count toward the spaces' sizes.",
                )}
              </p>
            </div>
            <NumberInput
              key={q.data.version_keep}
              value={q.data.version_keep}
              max={1000}
              unit={t("versions")}
              width="w-32 pr-16"
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
            <NumberInput
              key={q.data.version_days}
              value={q.data.version_days}
              max={3650}
              unit={t("days")}
              width="w-32 pr-16"
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
          title={t('Turn off the "All files" company space?')}
          description={t('Once turned off, no user can see "All files", and share links inside it are temporarily disabled. No files are deleted; turn it back on to restore access.')}
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
