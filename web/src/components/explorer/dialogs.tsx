/** File explorer dialogs: move / copy, delete, share and access (creating and renaming are edited inline in the list) */
import { AccessDialog } from "@/components/AccessDialog";
import { ConfirmDialog } from "@/components/dialogs";
import { FolderPickerDialog } from "@/components/FolderPickerDialog";
import { ShareDialog } from "@/components/ShareDialog";
import { t } from "@/lib/i18n";
import { homeFolder } from "@/lib/home";
import { useMe } from "@/lib/session";
import { trashHint } from "@/lib/utils";
import type { ExplorerProps } from "../Explorer";
import type { ExplorerState } from "./state";
import type { ExplorerActions } from "./actions";

export function ExplorerDialogs({ p, s, a }: { p: ExplorerProps; s: ExplorerState; a: ExplorerActions }) {
  const { dialog, setDialog } = s;
  const { transfer, trash } = a;
  const me = useMe();
  return (
    <>
      {(dialog?.t === "move" || dialog?.t === "copy") && (
        <FolderPickerDialog
          title={dialog.t === "move" ? t("Move {n} item to…|Move {n} items to…", { n: dialog.picked.count }) : t("Copy {n} item to…|Copy {n} items to…", { n: dialog.picked.count })}
          confirmText={dialog.t === "move" ? t("Move here") : t("Copy here")}
          startId={p.folderId ?? homeFolder(me, undefined) ?? null}
          excludeIds={new Set(dialog.picked.ids)}
          onClose={() => setDialog(null)}
          onPick={async (dest) => {
            // Closed first: a question about names the destination already has may follow
            setDialog(null);
            if (dialog.t === "move") await transfer("move", dialog.picked, dest, () => t("Moved"), t("Couldn't move"));
            else await transfer("copy", dialog.picked, dest, () => t("Copied"), t("Couldn't copy"));
          }}
        />
      )}
      {dialog?.t === "trash" && (
        <ConfirmDialog
          title={t("Move {n} item to trash?|Move {n} items to trash?", { n: dialog.picked.count })}
          description={trashHint(me.trash_days)}
          confirmText={t("Move to trash")}
          destructive
          onClose={() => setDialog(null)}
          onConfirm={async () => {
            const picked = dialog.picked;
            setDialog(null);
            await trash(picked);
          }}
        />
      )}
      {dialog?.t === "share" && <ShareDialog node={dialog.node} onClose={() => setDialog(null)} />}
      {dialog?.t === "access" && <AccessDialog nodeId={dialog.nodeId} onClose={() => setDialog(null)} />}
    </>
  );
}
