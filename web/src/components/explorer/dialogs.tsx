/** File explorer dialogs: move / copy, delete, share and access (creating and renaming are edited inline in the list) */
import { toast } from "sonner";
import { api } from "@/api";
import { AccessDialog } from "@/components/AccessDialog";
import { ConfirmDialog, FolderPickerDialog } from "@/components/dialogs";
import { ShareDialog } from "@/components/ShareDialog";
import { t } from "@/lib/i18n";
import type { ExplorerProps } from "../Explorer";
import type { ExplorerState } from "./state";
import type { ExplorerActions } from "./actions";

export function ExplorerDialogs({ p, s, a }: { p: ExplorerProps; s: ExplorerState; a: ExplorerActions }) {
  const { setSelected, dialog, setDialog } = s;
  const { refresh } = a;
  return (
    <>
      {(dialog?.t === "move" || dialog?.t === "copy") && (
        <FolderPickerDialog
          title={dialog.t === "move" ? t("Move {n} item to…|Move {n} items to…", { n: dialog.ids.length }) : t("Copy {n} item to…|Copy {n} items to…", { n: dialog.ids.length })}
          confirmText={dialog.t === "move" ? t("Move here") : t("Copy here")}
          startId={p.folderId ?? "root"}
          excludeIds={new Set(dialog.ids)}
          onClose={() => setDialog(null)}
          onPick={async (dest) => {
            if (dialog.t === "move") await api.move(dialog.ids, dest);
            else await api.copy(dialog.ids, dest);
            toast.success(dialog.t === "move" ? t("Moved") : t("Copied"));
            setDialog(null);
            setSelected(new Set());
            refresh();
          }}
        />
      )}
      {dialog?.t === "trash" && (
        <ConfirmDialog
          title={t("Move {n} item to trash?|Move {n} items to trash?", { n: dialog.ids.length })}
          description={t("Removed files stay in the trash and can be restored at any time.")}
          confirmText={t("Move to trash")}
          destructive
          onClose={() => setDialog(null)}
          onConfirm={async () => {
            await api.trash(dialog.ids);
            toast.success(t("Moved to trash"), {
              action: {
                label: t("Undo"),
                onClick: () =>
                  api
                    .restore(dialog.ids)
                    .then(() => {
                      toast.success(t("Restored"));
                      refresh();
                    })
                    // E.g. the original folder was deleted, a name conflict, or the space is full
                    .catch((e: Error) => toast.error(e.message)),
              },
            });
            setDialog(null);
            setSelected(new Set());
            refresh();
          }}
        />
      )}
      {dialog?.t === "share" && <ShareDialog node={dialog.node} onClose={() => setDialog(null)} />}
      {dialog?.t === "access" && <AccessDialog nodeId={dialog.nodeId} onClose={() => setDialog(null)} />}
    </>
  );
}
