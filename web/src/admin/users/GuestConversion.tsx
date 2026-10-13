import type { UseQueryResult } from "@tanstack/react-query";
import type { GuestConversionPreview } from "@/api";
import { Pending } from "@/components/ErrorState";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { ROLE_LABEL } from "@/lib/drives";
import { t } from "@/lib/i18n";
import { formatBytes, formatDate } from "@/lib/utils";

export function GuestConversion({ query, confirmed, onConfirm }: { query: UseQueryResult<GuestConversionPreview>; confirmed: boolean; onConfirm(v: boolean): void }) {
  const p = query.data;
  if (!p) return <Pending query={query} compact loading={<p className="text-xs text-muted-foreground">{t("Loading…")}</p>} />;
  const blocked = p.owned_spaces.length > 0 || p.unsafe_groups.length > 0;
  return (
    <div className="grid gap-3 rounded-md border p-3 text-sm" aria-live="polite">
      <div className="font-medium">{t("Guest conversion preview")}</div>
      <p className="text-xs text-muted-foreground">{t('Files are kept. "My files" is hidden while the account is a guest and becomes available again if it is changed back.')}</p>
      {p.personal_space && <p className="text-xs">{t("Personal files kept: {n} · {size}", { n: p.personal_files, size: formatBytes(p.personal_bytes) })}</p>}
      <p className="text-xs">{t("Existing share links and protocol publications stop working while this account is a guest.")}</p>
      {p.owned_spaces.length > 0 && (
        <div className="grid gap-1 text-xs text-destructive">
          <p>{t("Transfer these spaces to another owner first:")}</p>
          <ul className="list-inside list-disc [overflow-wrap:anywhere]">
            {p.owned_spaces.map((name, i) => (
              <li key={i}>{name}</li>
            ))}
          </ul>
        </div>
      )}
      {p.unsafe_groups.length > 0 && (
        <div className="grid gap-1 text-xs text-destructive">
          <p>{t("Fix these group grants before converting. Group members must not gain Manager, Owner or access without a suitable end date:")}</p>
          <ul className="max-h-36 list-inside list-disc overflow-y-auto [overflow-wrap:anywhere]">
            {p.unsafe_groups.map((g) => (
              <li key={g.grant_id}>
                {g.group_name} → {g.item_name} · {ROLE_LABEL[g.role]}
              </li>
            ))}
          </ul>
        </div>
      )}
      {p.changed_grants.length > 0 && (
        <div className="grid gap-1 text-xs">
          <p>{t("These direct grants change to Editor or below and end within {n} days:", { n: p.max_days })}</p>
          <ul className="max-h-36 list-inside list-disc overflow-y-auto [overflow-wrap:anywhere]">
            {p.changed_grants.map((g) => (
              <li key={g.id}>
                {g.item_name} · {ROLE_LABEL[g.old_role]} → {ROLE_LABEL[g.role]} · {formatDate(g.expires_at)}
              </li>
            ))}
          </ul>
        </div>
      )}
      {!blocked && (
        <Label className="flex items-start gap-2 text-xs font-normal">
          <Checkbox checked={confirmed} onCheckedChange={(v) => onConfirm(!!v)} />
          {t("I have reviewed and agree to these guest account changes")}
        </Label>
      )}
      <Button
        type="button"
        variant="outline"
        size="sm"
        onClick={() => {
          onConfirm(false);
          void query.refetch();
        }}
        disabled={query.isFetching}
      >
        {t("Refresh preview")}
      </Button>
    </div>
  );
}
