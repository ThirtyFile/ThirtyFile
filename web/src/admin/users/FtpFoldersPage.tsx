import { useEffect, useMemo, useState } from "react";
import { keepPreviousData, useInfiniteQuery } from "@tanstack/react-query";
import { SearchXIcon } from "lucide-react";
import { api } from "@/api";
import { nextPage, type ListPage } from "@/api/client";
import { keys } from "@/api/queryKeys";
import { SettingsFrame } from "@/admin/SettingsFrame";
import { FtpFolderCard } from "@/components/FtpFolders";
import { EmptyState } from "@/components/DataTable";
import { ErrorState } from "@/components/ErrorState";
import { Button } from "@/components/ui/button";
import { t } from "@/lib/i18n";

/** Receive folders asked for at a time */
const PAGE = 100;

export function FtpFoldersPage() {
  // Searched for on the server (the receive folder's name, its device username, its creator's or its folder's), as
  // the Users page does
  const [typed, setTyped] = useState("");
  const [search, setSearch] = useState("");
  useEffect(() => {
    const timer = setTimeout(() => setSearch(typed.trim()), 250);
    return () => clearTimeout(timer);
  }, [typed]);
  // Newest first, a page at a time. The receive folders shown stay while another search loads, and with them a
  // question open about one
  const q = useInfiniteQuery({
    queryKey: keys.receiveFolderPages(search),
    queryFn: ({ pageParam, signal }) => api.ftpFolders(undefined, search || undefined, signal, { ...pageParam, limit: PAGE }),
    initialPageParam: {} as ListPage,
    getNextPageParam: (last) => nextPage(last.folders, PAGE),
    placeholderData: keepPreviousData,
  });
  const folders = useMemo(() => q.data?.pages.flatMap((p) => p.folders) ?? [], [q.data]);
  return (
    <SettingsFrame item="ftp" onRefresh={() => q.refetch()} searchPlaceholder={t("Search receive folders")} onSearch={setTyped}>
      {q.error ? (
        <ErrorState message={q.error.message} onRetry={() => q.refetch()} />
      ) : !q.data ? (
        <p role="status">{t("Loading…")}</p>
      ) : folders.length === 0 ? (
        search ? (
          <EmptyState icon={SearchXIcon} title={t('No receive folders match "{query}"', { query: search })} />
        ) : (
          <p>{t("No FTP receive folders yet")}</p>
        )
      ) : (
        <>
          {folders.map((f) => (
            <FtpFolderCard key={f.id} folder={f} />
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
