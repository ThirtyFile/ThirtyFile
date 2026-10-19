import { useEffect, useMemo, useState } from "react";
import { keepPreviousData, useInfiniteQuery } from "@tanstack/react-query";
import { SearchXIcon } from "lucide-react";
import { api } from "@/api";
import { nextPage, type ListPage } from "@/api/client";
import { keys } from "@/api/queryKeys";
import { SettingsFrame } from "@/admin/SettingsFrame";
import { BucketCard } from "@/components/S3Buckets";
import { EmptyState } from "@/components/DataTable";
import { ErrorState } from "@/components/ErrorState";
import { Button } from "@/components/ui/button";
import { t } from "@/lib/i18n";

/** Buckets asked for at a time */
const PAGE = 100;

export function S3BucketsPage() {
  // Searched for on the server (the bucket's name, its creator's or its folder's), as the Users page does
  const [typed, setTyped] = useState("");
  const [search, setSearch] = useState("");
  useEffect(() => {
    const timer = setTimeout(() => setSearch(typed.trim()), 250);
    return () => clearTimeout(timer);
  }, [typed]);
  // Newest first, a page at a time. The buckets shown stay while another search loads, and with them a question open
  // about one
  const q = useInfiniteQuery({
    queryKey: keys.bucketPages(search),
    queryFn: ({ pageParam, signal }) => api.s3Buckets(undefined, search || undefined, signal, { ...pageParam, limit: PAGE }),
    initialPageParam: {} as ListPage,
    getNextPageParam: (last) => nextPage(last.buckets, PAGE),
    placeholderData: keepPreviousData,
  });
  const buckets = useMemo(() => q.data?.pages.flatMap((p) => p.buckets) ?? [], [q.data]);
  return (
    <SettingsFrame item="s3" onRefresh={() => q.refetch()} searchPlaceholder={t("Search buckets")} onSearch={setTyped}>
      {q.error ? (
        <ErrorState message={q.error.message} onRetry={() => q.refetch()} />
      ) : !q.data ? (
        <p role="status">{t("Loading…")}</p>
      ) : buckets.length === 0 ? (
        search ? (
          <EmptyState icon={SearchXIcon} title={t('No buckets match "{query}"', { query: search })} />
        ) : (
          <p>{t("No S3 buckets yet")}</p>
        )
      ) : (
        <>
          {buckets.map((bucket) => (
            <BucketCard key={bucket.id} bucket={bucket} />
          ))}
          {q.hasNextPage && (
            <Button variant="outline" size="sm" className="justify-self-start" disabled={q.isFetchingNextPage} onClick={() => void q.fetchNextPage()}>
              {q.isFetchingNextPage ? t("Loading…") : t("Show more")}
            </Button>
          )}
        </>
      )}
    </SettingsFrame>
  );
}
