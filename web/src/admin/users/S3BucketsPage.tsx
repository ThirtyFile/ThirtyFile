import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/api";
import { SettingsFrame } from "@/admin/SettingsFrame";
import { BucketCard } from "@/components/S3Buckets";
import { ErrorState } from "@/components/ErrorState";
import { Input } from "@/components/ui/input";
import { t } from "@/lib/i18n";

export function S3BucketsPage() {
  const [search, setSearch] = useState("");
  const q = useQuery({ queryKey: ["s3-buckets", "admin"], queryFn: ({ signal }) => api.s3Buckets(undefined, undefined, signal) });
  return (
    <SettingsFrame item="s3" onRefresh={() => q.refetch()}>
      <Input aria-label={t("Filter buckets")} placeholder={t("Filter buckets")} value={search} onChange={(e) => setSearch(e.target.value)} />
      {q.error ? (
        <ErrorState message={q.error.message} onRetry={() => q.refetch()} />
      ) : !q.data ? (
        <p role="status">{t("Loading…")}</p>
      ) : (
        <>
          {q.data.buckets
            .filter((bucket) => `${bucket.name} ${bucket.creator} ${bucket.folder}`.toLocaleLowerCase().includes(search.toLocaleLowerCase()))
            .map((bucket) => (
              <BucketCard key={bucket.id} bucket={bucket} />
            ))}
          {q.data.buckets.length === 0 && <p>{t("No S3 buckets yet")}</p>}
        </>
      )}
    </SettingsFrame>
  );
}
