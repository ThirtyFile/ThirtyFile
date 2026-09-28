import { useRef } from "react";
import { useQuery } from "@tanstack/react-query";
import { XIcon } from "lucide-react";
import { api, privateSource, type FolderContents, type HistoryEntry, type Node } from "@/api";
import { Button } from "@/components/ui/button";
import { ErrorState } from "@/components/ErrorState";
import { FileIcon, canThumbnail, typeLabel } from "@/components/FileIcon";
import { Resizer } from "@/components/Resizer";
import { VersionsSection } from "@/components/VersionsSection";
import { ROLE_LABEL, actionLabel } from "@/lib/drives";
import { useMediaQuery, useOverlayFocus } from "@/lib/focus";
import { t, tServer } from "@/lib/i18n";
import { usePersisted } from "@/lib/session";
import { formatBytes, formatWinDate } from "@/lib/utils";

/** Right-hand "Details" pane (Windows 11 style) */
const PANE_DEFAULT_WIDTH = 280;

/** "3 files, 2 folders" */
function containsText(c: Pick<FolderContents, "files" | "folders">) {
  return t("{files}, {folders}", { files: t("{n} file|{n} files", { n: c.files }), folders: t("{n} folder|{n} folders", { n: c.folders }) });
}

const add = (a: FolderContents, b: FolderContents): FolderContents => ({ size: a.size + b.size, files: a.files + b.files, folders: a.folders + b.folders });

/** The server sends at most this many entries */
const HISTORY_LIMIT = 50;

/** Recent activity on the item: who uploaded, edited, renamed, moved or deleted it, or something inside the folder */
function History({ node, query }: { node: Node; query: { data?: HistoryEntry[]; error: Error | null; refetch(): unknown } }) {
  let body;
  if (query.error) body = <ErrorState compact message={query.error.message} onRetry={() => query.refetch()} />;
  else if (!query.data) body = <p className="text-xs text-muted-foreground">…</p>;
  else if (!query.data.length) body = <p className="text-xs text-muted-foreground">{t("No activity yet")}</p>;
  else
    body = (
      <ol className="grid gap-2 text-xs">
        {query.data.map((a) => {
          const detail = tServer(a.detail);
          return (
            <li key={a.id} className="grid gap-0.5">
              <div className="[overflow-wrap:anywhere]">
                <span className="font-medium">{a.username}</span> · {actionLabel(a.action)}
                {/* Entries about something inside the folder name it */}
                {a.node_id !== node.id && a.node_name && <> · {a.node_name}</>}
                {detail && <span className="text-muted-foreground"> {detail}</span>}
              </div>
              <div className="text-muted-foreground">{formatWinDate(a.at)}</div>
            </li>
          );
        })}
        {query.data.length >= HISTORY_LIMIT && <li className="text-muted-foreground">{t("Only the {n} most recent entries are shown", { n: HISTORY_LIMIT })}</li>}
      </ol>
    );
  return (
    <section aria-label={t("Activity")} className="grid gap-2 border-t pt-3">
      <h3 className="text-xs font-medium">{t("Activity")}</h3>
      {body}
    </section>
  );
}

export function DetailsPane({ selected, folder, onClose }: { selected: Node[]; folder?: Node; onClose(): void }) {
  const [width, setWidth] = usePersisted("tf-details-width", PANE_DEFAULT_WIDTH);
  const node = selected.length === 1 ? selected[0] : selected.length === 0 ? folder : undefined;
  const info = useQuery({ queryKey: ["node", node?.id], queryFn: () => api.node(node!.id), enabled: !!node });
  // What folders hold is summed on the server (every level, not the trash): one folder, or the folders of a selection
  const folderIds = node ? (node.kind === "folder" ? [node.id] : []) : selected.filter((n) => n.kind === "folder").map((n) => n.id);
  const contents = useQuery({
    queryKey: node ? ["node", node.id, "contents"] : ["node", "contents", ...folderIds],
    queryFn: () => api.contents(folderIds),
    enabled: folderIds.length > 0,
  });
  const history = useQuery({ queryKey: ["node", node?.id, "history"], queryFn: () => api.history(node!.id), enabled: !!node });
  const shares = useQuery({ queryKey: ["shares", node?.id], queryFn: () => api.shares(node!.id), enabled: !!node && !!node.parent_id });
  // On narrow windows the pane covers the file list: focus moves into it, Esc closes it and focus goes back.
  // The list stays usable beside it, so focus isn't kept inside
  const ref = useRef<HTMLElement>(null);
  const overlay = useMediaQuery("(max-width: 63.99rem)");
  useOverlayFocus(ref, overlay, { onClose, modal: false });

  const header = (
    <div className="flex h-9 shrink-0 items-center justify-between border-b px-3 text-xs font-medium">
      {t("Details")}
      <Button variant="ghost" size="icon-xs" aria-label={t("Close details pane")} onClick={onClose}>
        <XIcon />
      </Button>
    </div>
  );

  let body;
  if (!node) {
    const selectedFiles = selected.filter((n) => n.kind === "file");
    const own = { size: selectedFiles.reduce((s, n) => s + n.size, 0), files: selectedFiles.length, folders: folderIds.length };
    // The selected files and folders plus everything inside the folders
    let summary: React.ReactNode = null;
    if (folderIds.length === 0) summary = `${t("{n} file|{n} files", { n: own.files })} · ${formatBytes(own.size)}`;
    else if (contents.data) summary = `${containsText(add(own, contents.data))} · ${formatBytes(own.size + contents.data.size)}`;
    else if (contents.error) summary = "—";
    else summary = t("Calculating size…");
    body = (
      <div className="flex flex-col items-center gap-2 px-4 py-10 text-center">
        <div className="relative size-20">
          {selected.slice(0, 3).map((n, i) => (
            <span key={n.id} className="absolute rounded-md bg-background" style={{ left: i * 12, top: i * 8 }}>
              <FileIcon node={n} className="size-12" />
            </span>
          ))}
        </div>
        <div className="text-sm">{t("{n} item selected|{n} items selected", { n: selected.length })}</div>
        <div className="text-xs text-muted-foreground">{summary}</div>
      </div>
    );
  } else {
    const root = info.data ? (info.data.via_share ? t("Shared with me") : info.data.drive.name) : "…";
    const isRoot = !node.parent_id;
    const location = info.data ? `/${root}` + info.data.path.slice(0, -1).map((c) => `/${c.name}`).join("") : info.error ? "—" : "…";
    // Values that couldn't be loaded show a dash, with the error and a way to try again below them
    const error = info.error ?? shares.error ?? contents.error;
    const pending = (q: { error: unknown }) => (q.error ? "—" : "…");
    const size = node.kind === "file" ? node.size : contents.data?.size;
    const rows: [string, React.ReactNode][] = [
      [t("Type"), typeLabel(node)],
      [t("Size"), size === undefined ? pending(contents) : t("{size} ({bytes} bytes)", { size: formatBytes(size), bytes: size })],
      ...(node.kind === "folder" ? ([[t("Contains"), contents.data ? containsText(contents.data) : pending(contents)]] as [string, string][]) : []),
      ...(isRoot ? [] : ([[t("Location"), location]] as [string, string][])),
      ...(info.data ? ([[t("My role"), ROLE_LABEL[info.data.role]]] as [string, string][]) : []),
      [t("Date modified"), formatWinDate(node.updated_at)],
      [t("Date created"), formatWinDate(node.created_at)],
      ...(isRoot ? [] : ([[t("Created by"), node.owner_name]] as [string, string][])),
      ...(isRoot ? [] : ([[t("Favorite"), node.is_favorite ? t("Yes") : t("No")]] as [string, string][])),
      ...(isRoot ? [] : ([[t("Share links"), shares.data ? (shares.data.length ? t("{n}", { n: shares.data.length }) : t("None")) : shares.error ? "—" : "…"]] as [string, string][])),
    ];
    body = (
      <>
        <div className="flex h-44 shrink-0 items-center justify-center border-b bg-muted/30 p-4">
          {node.kind === "file" && canThumbnail(node) ? (
            <img src={privateSource.thumbUrl(node)} alt="" className="max-h-full max-w-full rounded object-contain shadow" />
          ) : (
            <FileIcon node={node} className="size-16" />
          )}
        </div>
        <div className="grid gap-4 p-4">
          <div className="text-sm font-medium [overflow-wrap:anywhere]">{isRoot ? (info.data?.drive.name ?? "") : node.name}</div>
          <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-2 text-xs">
            {rows.map(([k, v]) => (
              <div key={k} className="contents">
                <dt className="text-muted-foreground">{k}</dt>
                <dd className="[overflow-wrap:anywhere] select-text">{v}</dd>
              </div>
            ))}
          </dl>
          <History node={node} query={history} />
          {error && (
            <ErrorState
              compact
              message={error.message}
              onRetry={() => Promise.all([info.error && info.refetch(), shares.error && shares.refetch(), contents.error && contents.refetch()])}
            />
          )}
          {node.kind === "file" && !node.trashed_at && (
            <VersionsSection node={node} canRestore={!!info.data && info.data.role !== "viewer" && !info.data.read_only} />
          )}
        </div>
      </>
    );
  }

  return (
    <aside
      ref={ref}
      aria-label={t("Details pane")}
      style={{ width, maxWidth: "85vw" }}
      className="relative flex shrink-0 flex-col border-l bg-background max-lg:absolute max-lg:inset-y-0 max-lg:right-0 max-lg:z-10 max-lg:shadow-xl"
    >
      <Resizer width={width} onChange={setWidth} min={220} max={640} defaultWidth={PANE_DEFAULT_WIDTH} edge="left" label={t("Resize details pane")} />
      {header}
      <div className="flex min-h-0 flex-1 flex-col overflow-y-auto">{body}</div>
    </aside>
  );
}
