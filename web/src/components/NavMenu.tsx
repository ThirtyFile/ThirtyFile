import { useState, type ReactNode } from "react";
import { useNavigate } from "react-router";
import { CopyIcon, FolderOpenIcon, LinkIcon, PanelTopIcon, UsersRoundIcon } from "lucide-react";
import { toast } from "sonner";
import { ContextMenu, ContextMenuContent, ContextMenuTrigger } from "@/components/ui/context-menu";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { AccessDialog } from "@/components/AccessDialog";
import { copyText } from "@/lib/utils";
import { t } from "@/lib/i18n";
import { useTabActions } from "@/tabs";

/**
 * Context menu for navigation links (left-hand list, address bar path): open, open in new tab, access, copy link / path.
 * Wraps the original element with display: contents, so the layout isn't affected.
 */
export function NavMenu(props: {
  to: string;
  children: ReactNode;
  /** Id of a folder or space root: offers "Share with… / Space members" */
  nodeId?: string;
  isSpaceRoot?: boolean;
  /** Also offer "Copy path" */
  path?: string;
}) {
  const navigate = useNavigate();
  const tabs = useTabActions();
  const [access, setAccess] = useState(false);
  return (
    <>
      <ContextMenu>
        <ContextMenuTrigger className="contents">{props.children}</ContextMenuTrigger>
        <ContextMenuContent>
          <DropdownMenuItem onClick={() => navigate(props.to)}>
            <FolderOpenIcon /> {t("Open")}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={() => tabs.open(props.to, { reuse: true })}>
            <PanelTopIcon /> {t("Open in new tab")}
          </DropdownMenuItem>
          {props.nodeId && (
            <>
              <DropdownMenuSeparator />
              <DropdownMenuItem onClick={() => setAccess(true)}>
                <UsersRoundIcon /> {props.isSpaceRoot ? t("Space members") : t("Share with…")}
              </DropdownMenuItem>
            </>
          )}
          <DropdownMenuSeparator />
          {props.path && (
            <DropdownMenuItem
              onClick={async () => {
                await copyText(props.path!);
                toast.success(t("Path copied"));
              }}
            >
              <CopyIcon /> {t("Copy path")}
            </DropdownMenuItem>
          )}
          <DropdownMenuItem
            onClick={async () => {
              await copyText(location.origin + props.to);
              toast.success(t("Link copied (people need to sign in and have access to open it)"));
            }}
          >
            <LinkIcon /> {t("Copy link")}
          </DropdownMenuItem>
        </ContextMenuContent>
      </ContextMenu>
      {access && props.nodeId && <AccessDialog nodeId={props.nodeId} onClose={() => setAccess(false)} />}
    </>
  );
}
