import { useEffect, useMemo, useRef, useState, type DragEvent, type FormEvent } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronRightIcon, DownloadIcon, EyeIcon, FolderOpenIcon, Grid2X2Icon, InboxIcon, LinkIcon, ListIcon, Loader2Icon, LockIcon, UploadIcon } from "lucide-react";
import { toast } from "sonner";
import { ContextMenu, ContextMenuContent, ContextMenuTrigger } from "@/components/ui/context-menu";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { api, shareSource, shareUploadEndpoint, triggerDownload, type Node, type PublicShare } from "@/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { ErrorText } from "@/components/dialogs";
import { ErrorState } from "@/components/ErrorState";
import { FileList, Thumb, type ViewMode } from "@/components/FileList";
import { canPreview } from "@/components/FileViewer";
import { Preview } from "@/components/Preview";
import { Logo } from "@/components/Logo";
import { LanguageSwitch } from "@/components/LanguageSwitch";
import { UploadPanel } from "@/components/UploadPanel";
import { enqueue, filesFromDrop, filesFromInput, onUploadsLanded, type PickedFile } from "@/uploads";
import { cn, formatBytes, formatDate } from "@/lib/utils";
import { t, tc } from "@/lib/i18n";
import { refreshFirstPage, useAllPages } from "@/lib/pages";

export function PublicSharePage() {
  const { token = "" } = useParams();
  const info = useQuery({ queryKey: ["public", token], queryFn: () => api.publicShare(token), retry: false });

  let body;
  if (info.isLoading) body = <Skeleton className="h-64 w-full max-w-3xl" />;
  else if (info.error || !info.data)
    body = (
      <div className="flex flex-col items-center gap-3 py-20 text-center text-muted-foreground">
        <LinkIcon className="size-12 stroke-1" />
        <div>{info.error?.message ?? t("This share link doesn't exist or has expired")}</div>
      </div>
    );
  else if (info.data.needs_password) body = <Unlock token={token} />;
  else if (info.data.node!.kind === "file") body = <SharedFile share={info.data} node={info.data.node!} />;
  else if (info.data.drop_only) body = <DropBox share={info.data} root={info.data.node!} />;
  else body = <SharedFolder share={info.data} root={info.data.node!} />;

  return (
    <div className="flex h-full flex-col bg-sidebar">
      <header className="flex h-14 shrink-0 items-center border-b bg-background px-4">
        <Logo className="text-sm" />
        <LanguageSwitch className="order-last ml-3 shrink-0" />
        {info.data && !info.data.needs_password && (
          <span className="ml-auto truncate pl-4 text-xs text-muted-foreground">
            {t("Shared by {name}", { name: info.data.owner })}
            {info.data.expires_at && ` · ${tc("date", "Expires {date}", { date: formatDate(info.data.expires_at) })}`}
            {info.data.downloads_left !== null && ` · ${t("{n} download left|{n} downloads left", { n: info.data.downloads_left })}`}
          </span>
        )}
      </header>
      <main className="flex min-h-0 flex-1 justify-center overflow-y-auto p-4">
        {/* The password form has its own visible heading */}
        {!info.data?.needs_password && <h1 className="sr-only">{info.data?.node?.name ?? t("Share link")}</h1>}
        {body}
      </main>
      <div className="fixed right-4 bottom-4 z-40 w-[min(380px,calc(100vw-2rem))]">
        <UploadPanel visitor />
      </div>
    </div>
  );
}

function Unlock({ token }: { token: string }) {
  const qc = useQueryClient();
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api.unlockShare(token, password);
      await qc.invalidateQueries({ queryKey: ["public", token] });
    } catch (err) {
      setError(err instanceof Error ? err.message : t("Couldn't unlock"));
    } finally {
      setBusy(false);
    }
  };
  return (
    <form onSubmit={submit} className="mt-16 grid h-fit w-full max-w-sm gap-4 rounded-2xl border bg-card p-6 shadow-sm">
      <h1 className="flex items-center gap-2 font-medium">
        <LockIcon className="size-4" /> {t("This share requires a password")}
      </h1>
      <div className="grid gap-2">
        <Label htmlFor="share-password">{t("Password")}</Label>
        <Input
          id="share-password"
          type="password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          placeholder={t("Enter password")}
          autoFocus
        />
      </div>
      <ErrorText>{error}</ErrorText>
      <Button type="submit" disabled={busy || !password}>
        {busy && <Loader2Icon className="animate-spin" />}
        {t("Open")}
      </Button>
    </form>
  );
}

/** Uploads files into a folder of a link that accepts them; files over the size limit are refused before sending */
function useShareUpload(share: PublicShare) {
  return (files: PickedFile[], parentId: string) => {
    const tooBig = share.max_upload ? files.filter((f) => f.file.size > share.max_upload) : [];
    if (tooBig.length) toast.error(t("{name} is larger than the upload size limit ({size})", { name: tooBig[0].file.name, size: formatBytes(share.max_upload) }));
    const ok = files.filter((f) => !tooBig.includes(f));
    if (ok.length) enqueue(ok, parentId, shareUploadEndpoint(share.token));
  };
}

/** Drag and drop of files from the computer: `onFiles` gets them, `dragging` is true while they're over the area */
function useFileDrop(enabled: boolean, onFiles: (files: PickedFile[]) => void) {
  const [dragging, setDragging] = useState(false);
  if (!enabled) return { dragging: false, props: {} };
  return {
    dragging,
    props: {
      onDragOver: (e: DragEvent) => {
        if (!e.dataTransfer.types.includes("Files")) return;
        e.preventDefault();
        e.dataTransfer.dropEffect = "copy";
        setDragging(true);
      },
      onDragLeave: (e: DragEvent) => {
        if (!e.currentTarget.contains(e.relatedTarget as globalThis.Node | null)) setDragging(false);
      },
      onDrop: async (e: DragEvent) => {
        if (!e.dataTransfer.types.includes("Files")) return;
        e.preventDefault();
        setDragging(false);
        const picked = await filesFromDrop(e.dataTransfer);
        if (picked.length) onFiles(picked);
      },
    },
  };
}

/** A hidden file picker and a function that opens it */
function useFilePicker(onFiles: (files: PickedFile[]) => void) {
  const ref = useRef<HTMLInputElement>(null);
  const input = (
    <input
      ref={ref}
      type="file"
      multiple
      hidden
      onChange={(e) => {
        if (e.target.files?.length) onFiles(filesFromInput(e.target.files));
        e.target.value = "";
      }}
    />
  );
  return { input, pick: () => ref.current?.click() };
}

/** A link that only accepts files: an upload area, nothing of what is already in the folder */
function DropBox({ share, root }: { share: PublicShare; root: Node }) {
  const upload = useShareUpload(share);
  const send = (files: PickedFile[]) => upload(files, root.id);
  const drop = useFileDrop(true, send);
  const picker = useFilePicker(send);
  return (
    <div
      {...drop.props}
      className={cn(
        "mt-10 flex h-fit w-full max-w-md flex-col items-center gap-4 rounded-2xl border-2 border-dashed bg-card p-10 text-center shadow-sm",
        drop.dragging && "border-brand bg-brand/5",
      )}
    >
      <InboxIcon className="size-12 stroke-1 text-muted-foreground" />
      <div>
        <div className="font-medium break-all">{t("Send files to “{name}”", { name: root.name })}</div>
        <p className="mt-1 text-sm text-muted-foreground">
          {t("Drag files here or choose them. You won't see what others have sent, and nothing can be downloaded here.")}
        </p>
      </div>
      <Button className="bg-brand text-brand-foreground hover:bg-brand/90" onClick={picker.pick}>
        <UploadIcon /> {t("Choose files")}
      </Button>
      {share.max_upload > 0 && <p className="text-xs text-muted-foreground">{t("Up to {size} per file", { size: formatBytes(share.max_upload) })}</p>}
      {picker.input}
    </div>
  );
}

/** Starts a download and then refreshes the share, so a download limit shows the downloads left */
function useShareDownload(token: string) {
  const qc = useQueryClient();
  return async (link: string | Promise<string>) => {
    try {
      triggerDownload(await link);
    } catch (e) {
      // The server refused the selection (too many items, the limit reached…)
      toast.error(e instanceof Error ? e.message : t("Download failed"));
      return;
    }
    // The browser downloads in the background; the server counts it when the download starts
    setTimeout(() => qc.invalidateQueries({ queryKey: ["public", token] }), 1500);
  };
}

function SharedFile({ share, node }: { share: PublicShare; node: Node }) {
  const source = useMemo(() => shareSource(share.token), [share.token]);
  const download = useShareDownload(share.token);
  const [previewing, setPreviewing] = useState(false);
  const exhausted = share.downloads_left === 0;
  const canDownload = share.allow_download && !exhausted;
  return (
    <>
      <ContextMenu>
        <ContextMenuTrigger className="mt-10 flex h-fit w-full max-w-md flex-col items-center gap-5 rounded-2xl border bg-card p-8 text-center shadow-sm">
          <div className="flex size-40 items-center justify-center overflow-hidden rounded-xl bg-muted">
            <Thumb node={node} source={source} className="size-full" iconClass="size-16" />
          </div>
          <div>
            <div className="font-medium break-all">{node.name}</div>
            <div className="text-sm text-muted-foreground">{formatBytes(node.size)}</div>
          </div>
          <div className="flex gap-2">
            {canPreview(node) && !exhausted && (
              <Button variant="outline" onClick={() => setPreviewing(true)}>
                <EyeIcon /> {t("Preview")}
              </Button>
            )}
            {share.allow_download && (
              <Button
                className="bg-brand text-brand-foreground hover:bg-brand/90"
                disabled={exhausted}
                onClick={() => download(source.contentUrl(node, true))}
              >
                <DownloadIcon /> {exhausted ? t("Download limit reached") : t("Download")}
              </Button>
            )}
          </div>
          {!share.allow_download && (
            <p className="text-xs text-muted-foreground">{canPreview(node) ? t("This link is for viewing only.") : t("This link is for viewing only, and this type of file can't be previewed.")}</p>
          )}
        </ContextMenuTrigger>
        <ContextMenuContent>
          {canPreview(node) && !exhausted && (
            <DropdownMenuItem onClick={() => setPreviewing(true)}>
              <EyeIcon /> {t("Preview")}
            </DropdownMenuItem>
          )}
          {share.allow_download && (
            <DropdownMenuItem disabled={exhausted} onClick={() => download(source.contentUrl(node, true))}>
              <DownloadIcon /> {t("Download")}
            </DropdownMenuItem>
          )}
        </ContextMenuContent>
      </ContextMenu>
      {previewing && (
        <Preview
          files={[node]}
          index={0}
          source={source}
          editable={false}
          allowDownload={canDownload}
          onIndexChange={() => {}}
          onClose={() => setPreviewing(false)}
        />
      )}
    </>
  );
}

function SharedFolder({ share, root }: { share: PublicShare; root: Node }) {
  const download = useShareDownload(share.token);
  const { nodeId } = useParams();
  const current = nodeId ?? root.id;
  const navigate = useNavigate();
  const source = useMemo(() => shareSource(share.token), [share.token]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [anchor, setAnchor] = useState<string | null>(null);
  const [view, setView] = useState<ViewMode>("list");
  const [previewId, setPreviewId] = useState<string | null>(null);
  // This stays mounted from one folder to the next: what was selected or previewed belongs to the folder left
  const [shownFolder, setShownFolder] = useState(current);
  if (shownFolder !== current) {
    setShownFolder(current);
    setSelected(new Set());
    setAnchor(null);
    setPreviewId(null);
  }

  const info = useQuery({ queryKey: ["public-node", share.token, current], queryFn: () => api.publicNode(share.token, current) });
  const children = useAllPages(["public-children", share.token, current], (limit, after, signal) =>
    api.publicChildrenPage(share.token, current, limit, after, signal),
  );
  const items = children.items;
  const files = useMemo(() => items.filter((n) => n.kind === "file"), [items]);
  const previewIndex = previewId ? files.findIndex((f) => f.id === previewId) : -1;
  const exhausted = share.downloads_left === 0;

  const downloadIds = selected.size ? [...selected] : [current];
  const selectedNodes = items.filter((n) => selected.has(n.id));
  const qc = useQueryClient();
  const upload = useShareUpload(share);
  const send = (files: PickedFile[]) => upload(files, current);
  const drop = useFileDrop(share.allow_upload, send);
  const picker = useFilePicker(send);
  // Files that finish uploading appear in the list: the first page of the folder while uploads run, every page (the
  // loading of one under way started again, so it can't miss them) when they end
  useEffect(
    () =>
      onUploadsLanded((parentIds, final) => {
        if (final) void qc.invalidateQueries({ queryKey: ["public-children", share.token] });
        else
          for (const id of parentIds)
            void refreshFirstPage(qc, ["public-children", share.token, id], (_, limit) => api.publicChildrenPage(share.token, id, limit));
      }),
    [qc, share.token],
  );

  return (
    <div className="flex h-fit min-h-full w-full max-w-5xl flex-col overflow-hidden rounded-2xl border bg-background shadow-sm">
      <div className="flex min-h-14 flex-wrap items-center gap-2 border-b px-4 py-2">
        <nav className="flex min-w-0 flex-1 items-center gap-0.5">
          {(info.data?.path ?? [{ id: root.id, name: root.name }]).map((c, i, arr) => (
            <span key={c.id} className="flex min-w-0 items-center gap-0.5">
              {i > 0 && <ChevronRightIcon className="size-4 shrink-0 text-muted-foreground" />}
              {i === arr.length - 1 ? (
                <span className="truncate px-1.5 font-semibold">{c.name}</span>
              ) : (
                <Link
                  to={`/share/${share.token}/${c.id}`}
                  className="truncate rounded-md px-1.5 py-1 text-muted-foreground hover:bg-muted hover:text-foreground"
                >
                  {c.name}
                </Link>
              )}
            </span>
          ))}
        </nav>
        <div className="flex rounded-lg border p-0.5">
          {(["list", "grid"] as const).map((v) => (
            <button
              key={v}
              type="button"
              aria-pressed={view === v}
              onClick={() => setView(v)}
              className={cn("rounded-md px-2 py-0.5 text-xs", view === v ? "bg-secondary" : "text-muted-foreground")}
            >
              {v === "list" ? t("Details") : t("Large icons")}
            </button>
          ))}
        </div>
        {share.allow_upload && (
          <Button size="sm" variant="outline" onClick={picker.pick}>
            <UploadIcon /> {t("Upload")}
          </Button>
        )}
        {share.allow_download && (
          <Button
            size="sm"
            className="bg-brand text-brand-foreground hover:bg-brand/90"
            disabled={exhausted}
            onClick={() => download(source.downloadLink(downloadIds))}
          >
            <DownloadIcon /> {selected.size ? t("Download {n} item|Download {n} items", { n: selected.size }) : t("Download all")}
          </Button>
        )}
        {picker.input}
      </div>
      {share.allow_upload && (
        <div className="border-b bg-muted/40 px-4 py-1.5 text-xs text-muted-foreground">{t("You can add files here: drag them onto the list or click Upload.")}</div>
      )}
      {children.isLoading ? (
        <div className="grid gap-2 p-4">
          {[0, 1, 2, 3].map((i) => (
            <Skeleton key={i} className="h-9" />
          ))}
        </div>
      ) : children.error && items.length === 0 ? (
        // The listing failed: not "This folder is empty"
        <ErrorState message={children.error.message} onRetry={() => children.refetch()} />
      ) : (
        <ContextMenu>
          <ContextMenuTrigger
            {...drop.props}
            className={cn("min-h-40 flex-1", drop.dragging && "bg-brand/5 ring-2 ring-brand/40 ring-inset")}
            onContextMenuCapture={(e) => !(e.target as HTMLElement).closest("[data-node-id]") && setSelected(new Set())}
            onClick={(e) => !(e.target as HTMLElement).closest("[data-node-id]") && setSelected(new Set())}
          >
            <FileList
              items={items}
              view={view}
              source={source}
              selected={selected}
              anchor={anchor}
              onSelect={(s, a) => {
                setSelected(s);
                if (a !== undefined) setAnchor(a);
              }}
              onOpen={(n) => {
                setSelected(new Set());
                if (n.kind === "folder") navigate(`/share/${share.token}/${n.id}`);
                else setPreviewId(n.id);
              }}
              empty={<div className="py-16 text-center text-sm text-muted-foreground">{t("This folder is empty")}</div>}
            />
          </ContextMenuTrigger>
          <ContextMenuContent>
            {selectedNodes.length === 1 && (
              <DropdownMenuItem
                onClick={() => {
                  const n = selectedNodes[0];
                  setSelected(new Set());
                  if (n.kind === "folder") navigate(`/share/${share.token}/${n.id}`);
                  else setPreviewId(n.id);
                }}
              >
                {selectedNodes[0].kind === "folder" ? <FolderOpenIcon /> : <EyeIcon />} {selectedNodes[0].kind === "folder" ? t("Open") : t("Preview")}
              </DropdownMenuItem>
            )}
            {share.allow_download && (
              <DropdownMenuItem disabled={exhausted} onClick={() => download(source.downloadLink(downloadIds))}>
                <DownloadIcon /> {selected.size ? t("Download {n} item|Download {n} items", { n: selected.size }) : t("Download all (ZIP)")}
              </DropdownMenuItem>
            )}
            {share.allow_upload && (
              <DropdownMenuItem onClick={picker.pick}>
                <UploadIcon /> {t("Upload files")}
              </DropdownMenuItem>
            )}
            <DropdownMenuSeparator />
            <DropdownMenuItem onClick={() => setView(view === "list" ? "grid" : "list")}>
              {view === "list" ? <Grid2X2Icon /> : <ListIcon />} {view === "list" ? t("Icon view") : t("List view")}
            </DropdownMenuItem>
          </ContextMenuContent>
        </ContextMenu>
      )}
      {previewIndex >= 0 && (
        <Preview
          files={files}
          index={previewIndex}
          source={source}
          editable={false}
          allowDownload={!exhausted && share.allow_download}
          onIndexChange={(i) => setPreviewId(files[i].id)}
          onClose={() => setPreviewId(null)}
        />
      )}
    </div>
  );
}
