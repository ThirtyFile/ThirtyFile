import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { LockIcon } from "lucide-react";
import { toast } from "sonner";
import { api, type Hold } from "@/api";
import { keys } from "@/api/queryKeys";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { ConfirmDialog } from "@/components/dialogs";
import { t } from "@/lib/i18n";
import { formatDate } from "@/lib/utils";

/** The reason a hold is given: asked for before it is placed */
export function HoldReasonField({ value, onChange }: { value: string; onChange(v: string): void }) {
  return (
    <div className="grid gap-1.5">
      <Label htmlFor="hold-reason">{t("Reason")}</Label>
      <Textarea
        id="hold-reason"
        value={value}
        maxLength={500}
        rows={3}
        onChange={(e) => onChange(e.target.value)}
        placeholder={t("For example: Case 2026-14, kept for the audit")}
        autoFocus
      />
      <p className="text-xs text-muted-foreground">{t("It is shown to the managers of what is held, and kept in the activity log.")}</p>
    </div>
  );
}

/** Which items it covers, in words */
export function scopeLabel(scope: Hold["scope"]) {
  return scope === "node" ? t("Folder or file") : scope === "space" ? t("Space") : t("Person's files");
}

/**
 * Details pane › Retention: what keeps this item from being deleted for good (the retention period of its space, and the
 * legal holds on it or above it), for its managers; administrators place and release holds here
 */
export function RetentionSection({ nodeId, name, ready }: { nodeId: string; name: string; ready: boolean }) {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: keys.nodeRetention(nodeId), queryFn: ({ signal }) => api.nodeRetention(nodeId, signal), enabled: ready, retry: false });
  const [placing, setPlacing] = useState(false);
  const [reason, setReason] = useState("");
  const [releasing, setReleasing] = useState<Hold | null>(null);
  const changed = () => Promise.all([qc.invalidateQueries({ queryKey: keys.nodeRetention(nodeId) }), qc.invalidateQueries({ queryKey: ["holds"] })]);
  const release = useMutation({
    mutationFn: (h: Hold) => api.releaseHold(h.id),
    onSuccess: () => {
      toast.success(t("Hold released"));
      return changed();
    },
  });
  const d = q.data;
  // Not for those who may not see it (the server says so), and nothing to say when nothing keeps it
  if (!d || (!d.can_place && !d.holds.length && !d.retention_days)) return null;
  return (
    <section aria-label={t("Retention")} className="grid gap-2 border-t pt-3">
      <h3 className="flex items-center gap-1.5 text-xs font-medium">
        <LockIcon className="size-3.5" /> {t("Retention")}
      </h3>
      {d.retention_days > 0 && (
        <p className="text-xs">
          {t("Files in this space are kept for {n} day after their last change.|Files in this space are kept for {n} days after their last change.", { n: d.retention_days })}
          {d.kept_until ? ` ${t("This one is kept until {date}.", { date: formatDate(d.kept_until) })}` : ""}
        </p>
      )}
      {d.holds.map((h) => (
        <div key={h.id} className="grid gap-0.5 rounded-md border border-amber-500/40 bg-amber-500/5 p-2 text-xs">
          <div className="font-medium">{t("On legal hold")}</div>
          <div className="[overflow-wrap:anywhere]">{h.reason}</div>
          <div className="text-muted-foreground">
            {h.scope === "node" ? t("Placed on {what}", { what: h.label }) : h.scope === "space" ? t("Placed on the whole space") : t("Placed on the files of {name}", { name: h.label })}
            {" · "}
            {t("{name}, {date}", { name: h.created_by_name, date: formatDate(h.created_at) })}
          </div>
          {d.can_place && (
            <Button type="button" variant="outline" size="xs" className="mt-1 w-fit" onClick={() => setReleasing(h)}>
              {t("Release")}
            </Button>
          )}
        </div>
      ))}
      {!d.holds.length && d.retention_days === 0 && <p className="text-xs text-muted-foreground">{t("Nothing keeps this from being deleted for good.")}</p>}
      {d.can_place && (
        <Button type="button" variant="outline" size="xs" className="w-fit" onClick={() => setPlacing(true)}>
          <LockIcon /> {t("Place a legal hold")}
        </Button>
      )}
      {placing && (
        <ConfirmDialog
          title={t('Place a legal hold on "{name}"?', { name })}
          description={t("Nothing in it can be deleted for good, and its earlier versions are kept, until an administrator releases the hold.")}
          confirmText={t("Place hold")}
          ready={!!reason.trim()}
          onClose={() => {
            setPlacing(false);
            setReason("");
          }}
          onConfirm={async () => {
            await api.placeHold({ scope: "node", node_id: nodeId, reason: reason.trim() });
            setPlacing(false);
            setReason("");
            toast.success(t("Hold placed"));
            await changed();
          }}
        >
          <HoldReasonField value={reason} onChange={setReason} />
        </ConfirmDialog>
      )}
      {releasing && (
        <ConfirmDialog
          title={t('Release the hold on "{label}"?', { label: releasing.label })}
          description={t("What it covered can be deleted for good again, unless something else keeps it.")}
          confirmText={t("Release")}
          onClose={() => setReleasing(null)}
          onConfirm={async () => {
            await release.mutateAsync(releasing);
            setReleasing(null);
          }}
        />
      )}
    </section>
  );
}
