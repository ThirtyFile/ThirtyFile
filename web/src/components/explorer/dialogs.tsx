/** File explorer dialogs: move / copy, delete, share and access (creating and renaming are edited inline in the list) */
import { api } from "@/api";
import { toastWithUndo } from "@/lib/undo";
import { AccessDialog } from "@/components/AccessDialog";
import { ConfirmDialog, FolderPickerDialog } from "@/components/dialogs";
import { ShareDialog } from "@/components/ShareDialog";
import { t } from "@/lib/i18n";
import { homeFolder } from "@/lib/home";
import { useMe } from "@/lib/session";
import { trashHint } from "@/lib/utils";
import type { ExplorerProps } from "../Explorer";
import type { ExplorerState } from "./state";
import type { ExplorerActions } from "./actions";

export function ExplorerDialogs({ p, s, a }: { p: ExplorerProps; s: ExplorerState; a: ExplorerActions }) {
  const { setSelected, dialog, setDialog } = s;
  const { refreshContents, transfer } = a;
  const me = useMe();
  return (
    <>
      {(dialog?.t === "move" || dialog?.t === "copy") && (
        <FolderPickerDialog
          title={dialog.t === "move" ? t("Move {n} item to…|Move {n} items to…", { n: dialog.ids.length }) : t("Copy {n} item to…|Copy {n} items to…", { n: dialog.ids.length })}
          confirmText={dialog.t === "move" ? t("Move here") : t("Copy here")}
          startId={p.folderId ?? homeFolder(me, undefined) ?? null}
          excludeIds={new Set(dialog.ids)}
          onClose={() => setDialog(null)}
          onPick={async (dest) => {
            // Closed first: a question about names the destination already has may follow
            setDialog(null);
            if (dialog.t === "move") await transfer("move", dialog.ids, dest, () => t("Moved"), t("Couldn't move"));
            else await transfer("copy", dialog.ids, dest, () => t("Copied"), t("Couldn't copy"));
          }}
        />
      )}
      {dialog?.t === "trash" && (
        <ConfirmDialog
          title={t("Move {n} item to trash?|Move {n} items to trash?", { n: dialog.ids.length })}
          description={trashHint(me.trash_days)}
          confirmText={t("Move to trash")}
          destructive
          onClose={() => setDialog(null)}
          onConfirm={async () => {
            await api.trash(dialog.ids);
            // Restoring can fail, e.g. the original folder was deleted, a name conflict, or the space is full
            toastWithUndo(t("Moved to trash"), { undo: () => api.restore(dialog.ids), undoneText: t("Restored"), after: refreshContents });
            setDialog(null);
            setSelected(new Set());
            refreshContents();
          }}
        />
      )}
      {dialog?.t === "share" && <ShareDialog node={dialog.node} onClose={() => setDialog(null)} />}
      {dialog?.t === "access" && <AccessDialog nodeId={dialog.nodeId} onClose={() => setDialog(null)} />}
    </>
  );
}
