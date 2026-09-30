import { useState } from "react";
import { Link2Icon, Link2OffIcon } from "lucide-react";
import { EmptyState } from "@/components/DataTable";
import { ToolSeparator } from "@/components/Frame";
import { t } from "@/lib/i18n";
import { NativeSelect } from "@/components/ui/native-select";
import { ShareLinks } from "@/components/ShareLinks";

/** "My share links": the caller's own links, or every link they may manage (space managers, item owners) */
export function SharesPage() {
  const [scope, setScope] = useState<"mine" | "managed">("mine");
  return (
    <ShareLinks
      filter={{ scope }}
      frame={{ crumbs: [{ label: t("Files") }, { label: t("My share links") }], icon: Link2Icon }}
      showOwner={scope === "managed"}
      extraToolbar={
        <>
          <ToolSeparator />
          <NativeSelect
            size="xs"
            aria-label={t("Show")}
            value={scope}
            onChange={(e) => setScope(e.target.value as "mine" | "managed")}
          >
            <option value="mine">{t("Links I created")}</option>
            <option value="managed">{t("All links I can manage")}</option>
          </NativeSelect>
        </>
      }
      empty={
        scope === "mine" ? (
          <EmptyState icon={Link2OffIcon} title={t("You haven't shared any files yet")} hint={t("Select a file and click Create share link on the toolbar to create a public link.")} />
        ) : (
          <EmptyState icon={Link2OffIcon} title={t("No share links")} hint={t("Links that others create in spaces you manage, or on your files, appear here.")} />
        )
      }
    />
  );
}

