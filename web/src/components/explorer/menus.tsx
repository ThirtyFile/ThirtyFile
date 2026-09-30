/** Context menu: item actions when items are selected; new, paste etc. when clicking empty space */
import {
  ClipboardPasteIcon,
  CopyIcon,
  DownloadIcon,
  FileArchiveIcon,
  PackageOpenIcon,
  EyeIcon,
  FilePlusIcon,
  FolderInputIcon,
  FolderOpenIcon,
  FolderPlusIcon,
  FolderUpIcon,
  InfoIcon,
  PanelTopIcon,
  PencilIcon,
  RefreshCwIcon,
  ScissorsIcon,
  Share2Icon,
  StarIcon,
  StarOffIcon,
  Trash2Icon,
  UploadIcon,
  UsersRoundIcon,
  CloudOffIcon,
} from "lucide-react";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import type { ExplorerProps } from "../Explorer";
import type { ExplorerState } from "./state";
import type { ExplorerActions } from "./actions";
import { Kbd } from "./ui";
import { t } from "@/lib/i18n";
import { isZip } from "@/components/FileIcon";

export function explorerMenus(p: ExplorerProps, s: ExplorerState, a: ExplorerActions) {
  const { caps, navigate, tabs, fileInput, dirInput, canCreate, canUpload, single, allFavorite, setDialog, setDetailsOpen } = s;
  const { refresh, open, download, compress, extract, toggleFavorite, cut, copy, canPaste, paste, createNew } = a;
  const newItems = (
    <>
      <DropdownMenuItem disabled={!canCreate} onClick={() => createNew("folder")}>
        <FolderPlusIcon /> {t("New folder")} <Kbd>Ctrl+Shift+N</Kbd>
      </DropdownMenuItem>
      <DropdownMenuItem disabled={!canUpload} onClick={() => createNew("file")}>
        <FilePlusIcon /> {t("New text document")}
      </DropdownMenuItem>
      <DropdownMenuSeparator />
      <DropdownMenuItem disabled={!canUpload} onClick={() => fileInput.current?.click()}>
        <UploadIcon /> {t("Upload files")}
      </DropdownMenuItem>
      <DropdownMenuItem disabled={!canUpload} onClick={() => dirInput.current?.click()}>
        <FolderUpIcon /> {t("Upload folder")}
      </DropdownMenuItem>
      {canCreate && p.offline && (
        <p className="flex items-start gap-1.5 px-2 pt-1 pb-1.5 text-[11px] leading-snug text-muted-foreground">
          <CloudOffIcon className="mt-px size-3.5 shrink-0" /> {t("Storage service offline. You can't upload or create files right now.")}
        </p>
      )}
    </>
  );

  const menuItems = s.count ? (
    <>
      {single && <DropdownMenuItem onClick={() => open(single)}>{single.kind === "folder" ? <FolderOpenIcon /> : <EyeIcon />} {t("Open")}</DropdownMenuItem>}
      {single?.kind === "folder" && (
        <DropdownMenuItem onClick={() => tabs.open(`/files/${single.id}`)}>
          <PanelTopIcon /> {t("Open in new tab")}
        </DropdownMenuItem>
      )}
      {single?.location !== undefined && (
        <DropdownMenuItem onClick={() => navigate(`/files/${single.parent_id}`)}>
          <FolderOpenIcon /> {t("Open file location")}
        </DropdownMenuItem>
      )}
      <DropdownMenuItem onClick={() => download(s.picked)}>
        <DownloadIcon /> {s.count > 1 || single?.kind === "folder" ? t("Download (ZIP)") : t("Download")}
      </DropdownMenuItem>
      {/* Made next to the items, so only where new files can be added (not in search results or other lists) */}
      {canUpload && (
        <DropdownMenuItem onClick={() => compress(s.picked)}>
          <FileArchiveIcon /> {t("Compress to ZIP file")}
        </DropdownMenuItem>
      )}
      {canUpload && single && isZip(single) && (
        <DropdownMenuItem onClick={() => extract(single)}>
          <PackageOpenIcon /> {t("Extract all")}
        </DropdownMenuItem>
      )}
      {single && (
        <DropdownMenuItem onClick={() => setDialog({ t: "access", nodeId: single.id })}>
          <UsersRoundIcon /> {t("Share with…")}
        </DropdownMenuItem>
      )}
      {single && caps.share && (
        <DropdownMenuItem onClick={() => setDialog({ t: "share", node: single })}>
          <Share2Icon /> {t("Create share link")}
        </DropdownMenuItem>
      )}
      <DropdownMenuItem onClick={toggleFavorite}>
        {allFavorite ? <StarOffIcon /> : <StarIcon />} {allFavorite ? t("Remove from favorites") : t("Add to favorites")}
      </DropdownMenuItem>
      <DropdownMenuSeparator />
      {caps.write && (
        <DropdownMenuItem onClick={cut}>
          <ScissorsIcon /> {t("Cut")} <Kbd>Ctrl+X</Kbd>
        </DropdownMenuItem>
      )}
      <DropdownMenuItem onClick={copy}>
        <CopyIcon /> {t("Copy")} <Kbd>Ctrl+C</Kbd>
      </DropdownMenuItem>
      {caps.write && (
        <>
          {single && (
            <DropdownMenuItem onClick={() => setDialog({ t: "rename", node: single })}>
              <PencilIcon /> {t("Rename")} <Kbd>F2</Kbd>
            </DropdownMenuItem>
          )}
          <DropdownMenuItem onClick={() => setDialog({ t: "move", picked: s.picked })}>
            <FolderInputIcon /> {t("Move to…")}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={() => setDialog({ t: "copy", picked: s.picked })}>
            <CopyIcon /> {t("Copy to…")}
          </DropdownMenuItem>
        </>
      )}
      {caps.del && (
        <DropdownMenuItem variant="destructive" onClick={() => setDialog({ t: "trash", picked: s.picked })}>
          <Trash2Icon /> {t("Delete")} <Kbd>Delete</Kbd>
        </DropdownMenuItem>
      )}
      <DropdownMenuSeparator />
      <DropdownMenuItem onClick={() => setDetailsOpen(true)}>
        <InfoIcon /> {t("Properties")} <Kbd>Alt+Enter</Kbd>
      </DropdownMenuItem>
    </>
  ) : (
    <>
      {canCreate && newItems}
      {canCreate && <DropdownMenuSeparator />}
      {canCreate && (
        <DropdownMenuItem disabled={!canPaste} onClick={paste}>
          <ClipboardPasteIcon /> {t("Paste")} <Kbd>Ctrl+V</Kbd>
        </DropdownMenuItem>
      )}
      <DropdownMenuItem onClick={refresh}>
        <RefreshCwIcon /> {t("Refresh")}
      </DropdownMenuItem>
      <DropdownMenuItem onClick={() => setDetailsOpen(true)}>
        <InfoIcon /> {t("Properties")}
      </DropdownMenuItem>
    </>
  );

  return { newItems, menuItems };
}
