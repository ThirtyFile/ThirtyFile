import { useEffect, useState } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { SearchXIcon } from "lucide-react";
import { SettingsFrame } from "@/admin/SettingsFrame";
import { FtpFolderCard, ftpFolderQuery } from "@/components/FtpFolders";
import { EmptyState } from "@/components/DataTable";
import { ErrorState } from "@/components/ErrorState";
import { t } from "@/lib/i18n";

export function FtpFoldersPage() {
  // Searched for on the server (the receive folder's name, its device username, its creator's or its folder's), as
  // the Users page does
  const [typed, setTyped] = useState("");
  const [search, setSearch] = useState("");
  useEffect(() => {
    const timer = setTimeout(() => setSearch(typed.trim()), 250);
    return () => clearTimeout(timer);
  }, [typed]);
  // The receive folders shown stay while another search loads, and with them a question open about one
  const q = useQuery({ ...ftpFolderQuery(undefined, search), placeholderData: keepPreviousData });
  return (
    <SettingsFrame item="ftp" onRefresh={() => q.refetch()} searchPlaceholder={t("Search receive folders")} onSearch={setTyped}>
      {q.error ? (
        <ErrorState message={q.error.message} onRetry={() => q.refetch()} />
      ) : !q.data ? (
        <p role="status">{t("Loading…")}</p>
      ) : q.data.folders.length === 0 ? (
        search ? (
          <EmptyState icon={SearchXIcon} title={t('No receive folders match "{query}"', { query: search })} />
        ) : (
          <p>{t("No FTP receive folders yet")}</p>
        )
      ) : (
        q.data.folders.map((f) => <FtpFolderCard key={f.id} folder={f} />)
      )}
    </SettingsFrame>
  );
}
