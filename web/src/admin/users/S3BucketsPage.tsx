import { useEffect, useState } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { SearchXIcon } from "lucide-react";
import { api } from "@/api";
import { keys } from "@/api/queryKeys";
import { SettingsFrame } from "@/admin/SettingsFrame";
import { BucketCard } from "@/components/S3Buckets";
import { EmptyState } from "@/components/DataTable";
import { ErrorState } from "@/components/ErrorState";
import { t } from "@/lib/i18n";

export function S3BucketsPage() {
  // Searched for on the server (the bucket's name, its creator's or its folder's), as the Users page does
  const [typed, setTyped] = useState("");
  const [search, setSearch] = useState("");
  useEffect(() => {
    const timer = setTimeout(() => setSearch(typed.trim()), 250);
    return () => clearTimeout(timer);
  }, [typed]);
  // The buckets shown stay while another search loads, and with them a question open about one
  const q = useQuery({ queryKey: keys.buckets(null, search), queryFn: ({ signal }) => api.s3Buckets(undefined, search || undefined, signal), placeholderData: keepPreviousData });
  return (
    <SettingsFrame item="s3" onRefresh={() => q.refetch()} searchPlaceholder={t("Search buckets")} onSearch={setTyped}>
      {q.error ? (
        <ErrorState message={q.error.message} onRetry={() => q.refetch()} />
      ) : !q.data ? (
        <p role="status">{t("Loading…")}</p>
      ) : q.data.buckets.length === 0 ? (
        search ? (
          <EmptyState icon={SearchXIcon} title={t('No buckets match "{query}"', { query: search })} />
        ) : (
          <p>{t("No S3 buckets yet")}</p>
        )
      ) : (
        q.data.buckets.map((bucket) => <BucketCard key={bucket.id} bucket={bucket} />)
      )}
    </SettingsFrame>
  );
}
