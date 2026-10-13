import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { SettingsFrame } from "@/admin/SettingsFrame";
import { FtpFolderCard, ftpFolderQuery } from "@/components/FtpFolders";
import { ErrorState } from "@/components/ErrorState";
import { Input } from "@/components/ui/input";
import { t } from "@/lib/i18n";

export function FtpFoldersPage() {
  const [search, setSearch] = useState("");
  const q = useQuery(ftpFolderQuery());
  return (
    <SettingsFrame item="ftp" onRefresh={() => q.refetch()}>
      <Input aria-label={t("Filter receive folders")} placeholder={t("Filter receive folders")} value={search} onChange={(e) => setSearch(e.target.value)} />
      {q.error ? (
        <ErrorState message={q.error.message} onRetry={() => q.refetch()} />
      ) : !q.data ? (
        <p role="status">{t("Loading…")}</p>
      ) : (
        <>
          {q.data.folders
            .filter((f) => `${f.name} ${f.creator} ${f.folder_name} ${f.username}`.toLocaleLowerCase().includes(search.toLocaleLowerCase()))
            .map((f) => (
              <FtpFolderCard key={f.id} folder={f} />
            ))}
          {q.data.folders.length === 0 && <p>{t("No FTP receive folders yet")}</p>}
        </>
      )}
    </SettingsFrame>
  );
}
