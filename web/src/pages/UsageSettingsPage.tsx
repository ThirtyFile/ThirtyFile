import { Skeleton } from "@/components/ui/skeleton";
import { Pending } from "@/components/ErrorState";
import { formatBytes } from "@/lib/utils";
import { t } from "@/lib/i18n";
import { Section, SettingsFrame, useSystem } from "@/pages/SettingsFrame";

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
        <Pending query={q} loading={<Skeleton className="h-40" />} />
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
