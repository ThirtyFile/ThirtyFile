import { useQuery } from "@tanstack/react-query";
import { UsersRoundIcon } from "lucide-react";
import { api } from "@/api";
import { Explorer } from "@/components/Explorer";
import { t } from "@/lib/i18n";

/** Shared with me: folders and files others have shared with me */
export function SharedWithMePage() {
  const q = useQuery({ queryKey: ["shared-with-me"], queryFn: api.sharedWithMe });
  return (
    <Explorer
      items={q.data ?? []}
      loading={q.isLoading}
      error={q.error}
      showOwner
      upTo="/drives"
      icon={UsersRoundIcon}
      crumbs={[{ label: t("All spaces"), to: "/drives" }, { label: t("Shared with me") }]}
      empty={
        <div className="flex min-h-52 flex-col items-center justify-center gap-2 py-10 text-muted-foreground">
          <UsersRoundIcon className="size-9 stroke-[1.4]" />
          <p>{t("Nothing has been shared with you yet")}</p>
          <p className="text-xs">{t("When someone chooses \"Share with…\" on a folder and adds you (or a group you're in), it will show up here.")}</p>
        </div>
      }
    />
  );
}
