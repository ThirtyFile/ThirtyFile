import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  CheckCircle2Icon,
  CloudIcon,
  EllipsisIcon,
  FolderOpenIcon,
  HardDriveIcon,
  LayersIcon,
  ListChecksIcon,
  Loader2Icon,
  PencilIcon,
  PlugZapIcon,
  PlusIcon,
  SearchXIcon,
  ServerIcon,
  ShieldAlertIcon,
  StarIcon,
  Trash2Icon,
  XCircleIcon,
} from "lucide-react";
import { toast } from "sonner";
import { api, type StorageConfig, type StorageKind, type StorageLocation } from "@/api";
import { DRIVE_ICON, DRIVE_KIND_LABEL } from "@/lib/drives";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { ConfirmDialog, ErrorText } from "@/components/dialogs";
import { RowMenuArea } from "@/components/RowMenuArea";
import { LocationBrowseDialog, LocationTestDialog, UnusedContentDialog } from "@/components/StorageTools";
import { cn, formatBytes, formatDateTime } from "@/lib/utils";
import { t, tServer } from "@/lib/i18n";

/** Dropdown (same style as the inputs) */
const SELECT_CLASS =
  "h-8 w-full rounded-lg border border-input bg-transparent px-2.5 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring dark:bg-input/30 [&>option]:bg-popover [&>option]:text-popover-foreground";

export const STORAGE_KIND_LABEL: Record<StorageKind, string> = {
  s3: t("S3-compatible"),
  sftp: "SFTP",
  ftp: t("FTP/FTPS"),
  local: t("Local folder"),
};
const KINDS: StorageKind[] = ["s3", "sftp", "ftp", "local"];

/** Defaults when switching type */
const DEFAULTS: Record<StorageKind, StorageConfig> = {
  s3: {},
  sftp: { host: "", port: 22, username: "", path: "" },
  ftp: { host: "", port: 21, username: "", path: "", tls: true },
  local: { path: "" },
};

function describe(l: StorageLocation) {
  if (l.kind === "local") return l.config.path || "—";
  if (l.kind === "sftp" || l.kind === "ftp") {
    const c = l.config;
    const scheme = l.kind === "sftp" ? "sftp" : c.tls ? "ftps" : "ftp";
    const port = c.port && c.port !== (l.kind === "sftp" ? 22 : 21) ? `:${c.port}` : "";
    return `${scheme}://${c.username ? `${c.username}@` : ""}${c.host}${port}${c.path ? (c.path.startsWith("/") ? c.path : `/${c.path}`) : ""}`;
  }
  const host = l.config.endpoint ? l.config.endpoint.replace(/^https?:\/\//, "") : "AWS S3";
  return `${host} · ${l.config.bucket}${l.config.prefix ? `/${l.config.prefix}` : ""}`;
}

/** System settings › Storage locations */
export function StorageLocations() {
  const qc = useQueryClient();
  // The server checks connection status every 30 seconds; the view refreshes every 30 seconds
  const q = useQuery({ queryKey: ["storage-locations"], queryFn: api.storageLocations, refetchInterval: 30_000 });
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [editing, setEditing] = useState<StorageLocation | "new" | null>(null);
  const [deleting, setDeleting] = useState<StorageLocation | null>(null);
  const [showing, setShowing] = useState<StorageLocation | null>(null);
  const [testing, setTesting] = useState<string | null>(null);
  const [tool, setTool] = useState<{ kind: "test" | "browse" | "unused"; location: StorageLocation } | null>(null);
  const list = q.data ?? [];
  const selected = list.find((l) => l.id === selectedId) ?? null;
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["storage-locations"] });
    qc.invalidateQueries({ queryKey: ["admin-drives"] });
  };

  const test = async (l: StorageLocation) => {
    setTesting(l.id);
    try {
      await api.testExistingStorage(l.id);
      toast.success(t("Connected to \"{name}\" successfully", { name: l.name }));
    } catch (e) {
      toast.error(e instanceof Error ? e.message : t("Connection failed"));
    } finally {
      setTesting(null);
      refresh();
    }
  };
  const makeDefault = async (l: StorageLocation) => {
    try {
      await api.setDefaultStorage(l.id);
      toast.success(t("New spaces will be stored in \"{name}\"", { name: l.name }));
      refresh();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : t("Operation failed"));
    }
  };

  const menu = (l: StorageLocation) => (
    <>
      <DropdownMenuItem onClick={() => setEditing(l)}>
        <PencilIcon /> {t("Edit")}
      </DropdownMenuItem>
      <DropdownMenuItem onClick={() => test(l)}>
        <PlugZapIcon /> {t("Test connection")}
      </DropdownMenuItem>
      <DropdownMenuItem onClick={() => setShowing(l)}>
        <LayersIcon /> {t("Spaces on this location")}
      </DropdownMenuItem>
      <DropdownMenuItem onClick={() => setTool({ kind: "test", location: l })}>
        <ListChecksIcon /> {t("Test step by step")}
      </DropdownMenuItem>
      <DropdownMenuItem onClick={() => setTool({ kind: "browse", location: l })}>
        <FolderOpenIcon /> {t("Browse")}
      </DropdownMenuItem>
      <DropdownMenuItem onClick={() => setTool({ kind: "unused", location: l })}>
        <SearchXIcon /> {t("Find unused content")}
      </DropdownMenuItem>
      <DropdownMenuItem disabled={l.is_default} onClick={() => makeDefault(l)}>
        <StarIcon /> {t("Set as default location")}
      </DropdownMenuItem>
      {!l.builtin && (
        <>
          <DropdownMenuSeparator />
          <DropdownMenuItem variant="destructive" onClick={() => setDeleting(l)}>
            <Trash2Icon /> {t("Delete")}
          </DropdownMenuItem>
        </>
      )}
    </>
  );

  return (
    <div>
      <div className="flex items-center justify-between gap-3 border-b px-4 py-3">
        <p className="text-xs leading-relaxed text-muted-foreground">
          {(() => {
            const [before, after] = t("Where spaces keep their files. New spaces are created on the {default}; changing it doesn't move existing spaces. A space's files can be moved in \"Space management\".").split("{default}");
            return (
              <>
                {before}
                <span className="text-foreground">{t("default location")}</span>
                {after}
              </>
            );
          })()}
        </p>
        <Button size="sm" className="shrink-0" onClick={() => setEditing("new")}>
          <PlusIcon /> {t("Add storage location")}
        </Button>
      </div>
      <RowMenuArea
        onTarget={setSelectedId}
        menu={
          selected ? (
            menu(selected)
          ) : (
            <DropdownMenuItem onClick={() => setEditing("new")}>
              <PlusIcon /> {t("Add storage location")}
            </DropdownMenuItem>
          )
        }
      >
        {q.isLoading ? (
          <div className="flex h-20 items-center justify-center text-muted-foreground">
            <Loader2Icon className="size-5 animate-spin" />
          </div>
        ) : (
          <div className="divide-y">
            {list.map((l) => {
              const Icon = l.kind === "s3" ? CloudIcon : l.kind === "local" ? HardDriveIcon : ServerIcon;
              return (
                <div
                  key={l.id}
                  data-row-id={l.id}
                  onClick={() => setSelectedId(l.id)}
                  onDoubleClick={() => setEditing(l)}
                  className={cn(
                    "flex items-center gap-3 px-4 py-3 select-none hover:bg-muted/50",
                    selectedId === l.id && "bg-selection shadow-[inset_3px_0_0_var(--color-brand)] hover:bg-selection",
                  )}
                >
                  <Icon
                    className={cn(
                      "size-5 shrink-0",
                      l.kind === "s3" ? "text-sky-500" : l.kind === "local" ? "text-muted-foreground" : "text-violet-500",
                    )}
                  />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-1.5 text-sm">
                      <span className="truncate">{l.name}</span>
                      {l.is_default && <span className="rounded bg-brand/15 px-1.5 py-px text-[11px] text-brand">{t("Default")}</span>}
                      {l.builtin && <span className="rounded bg-muted px-1.5 py-px text-[11px] text-muted-foreground">{t("Built-in")}</span>}
                    </div>
                    <div className="truncate text-xs text-muted-foreground">
                      {STORAGE_KIND_LABEL[l.kind]} · {describe(l)}
                    </div>
                    {!l.connected && l.health_error && (
                      <div className="mt-0.5 truncate text-xs text-destructive" title={tServer(l.health_error)}>
                        {tServer(l.health_error)}
                      </div>
                    )}
                    {l.pending_deletes > 0 && (
                      <div className="mt-0.5 text-xs text-amber-600 dark:text-amber-400">
                        {t("{n} deleted file hasn't been removed from here yet; it will be retried automatically once the connection is restored|{n} deleted files haven't been removed from here yet; they will be retried automatically once the connection is restored", { n: l.pending_deletes })}
                      </div>
                    )}
                  </div>
                  <div className="hidden w-48 shrink-0 text-right text-xs text-muted-foreground sm:block">
                    <div className="tabular-nums" title={l.folder_bytes > 0 ? t("{size} in folder spaces", { size: formatBytes(l.folder_bytes) }) : undefined}>
                      {t("{size} used", { size: formatBytes(l.used_bytes) })}
                    </div>
                    {l.disk_total_bytes != null && l.disk_free_bytes != null && (
                      <div className="tabular-nums">{t("{free} free of {total}", { free: formatBytes(l.disk_free_bytes), total: formatBytes(l.disk_total_bytes) })}</div>
                    )}
                    <div>
                      {t("{n} file|{n} files", { n: l.blob_count })} ·{" "}
                      <button
                        type="button"
                        className="underline-offset-2 hover:text-foreground hover:underline"
                        onClick={(e) => {
                          e.stopPropagation();
                          setShowing(l);
                        }}
                      >
                        {t("{n} space|{n} spaces", { n: l.drive_count })}
                      </button>
                    </div>
                  </div>
                  <span
                    title={l.checked_at ? t("Last checked: {time} (checked automatically every 30 seconds)", { time: formatDateTime(l.checked_at) }) : undefined}
                    className={cn(
                      "flex w-20 shrink-0 items-center gap-1 text-xs",
                      l.connected ? "text-emerald-600 dark:text-emerald-400" : "text-destructive",
                    )}
                  >
                    {testing === l.id ? (
                      <Loader2Icon className="size-3.5 animate-spin" />
                    ) : l.connected ? (
                      <CheckCircle2Icon className="size-3.5" />
                    ) : (
                      <XCircleIcon className="size-3.5" />
                    )}
                    {l.connected ? t("Connected") : t("Unreachable")}
                  </span>
                  <DropdownMenu>
                    <DropdownMenuTrigger
                      render={<Button size="icon-sm" variant="ghost" aria-label={t("More actions")} onClick={(e) => e.stopPropagation()} />}
                    >
                      <EllipsisIcon />
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">{menu(l)}</DropdownMenuContent>
                  </DropdownMenu>
                </div>
              );
            })}
          </div>
        )}
      </RowMenuArea>
      {editing && (
        <StorageDialog
          location={editing === "new" ? null : editing}
          onClose={() => setEditing(null)}
          onSaved={() => {
            setEditing(null);
            refresh();
          }}
        />
      )}
      {showing && <SpacesDialog location={showing} onClose={() => setShowing(null)} />}
      {tool?.kind === "test" && <LocationTestDialog location={tool.location} onClose={() => setTool(null)} />}
      {tool?.kind === "browse" && <LocationBrowseDialog location={tool.location} onClose={() => setTool(null)} />}
      {tool?.kind === "unused" && <UnusedContentDialog location={tool.location} onClose={() => setTool(null)} />}
      {deleting && (
        <ConfirmDialog
          title={t("Delete storage location \"{name}\"?", { name: deleting.name })}
          description={t("This only removes the setting; data in the storage service isn't deleted. A location can't be deleted while files or spaces are still using it.")}
          confirmText={t("Delete")}
          destructive
          onClose={() => setDeleting(null)}
          onConfirm={async () => {
            await api.deleteStorage(deleting.id);
            toast.success(t("Storage location deleted"));
            setDeleting(null);
            setSelectedId(null);
            refresh();
          }}
        />
      )}
    </div>
  );
}

/** The spaces on a location: name, kind, owner and size (nothing of what is in them) */
function SpacesDialog({ location, onClose }: { location: StorageLocation; onClose(): void }) {
  const q = useQuery({ queryKey: ["storage-location-spaces", location.id], queryFn: () => api.storageLocationSpaces(location.id) });
  const spaces = q.data ?? [];
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("Spaces on \"{name}\"", { name: location.name })}</DialogTitle>
          <DialogDescription>{t("Spaces stay on the location they were created on until they're moved.")}</DialogDescription>
        </DialogHeader>
        {q.isLoading ? (
          <div className="flex h-20 items-center justify-center text-muted-foreground">
            <Loader2Icon className="size-5 animate-spin" />
          </div>
        ) : q.error ? (
          <ErrorText>{q.error instanceof Error ? q.error.message : t("Operation failed")}</ErrorText>
        ) : spaces.length === 0 ? (
          <p className="py-4 text-center text-sm text-muted-foreground">{t("No spaces are on this location.")}</p>
        ) : (
          <ul className="max-h-80 divide-y overflow-y-auto rounded-md border text-sm">
            {spaces.map((s) => {
              const Icon = DRIVE_ICON[s.kind];
              return (
                <li key={s.id} className="flex items-center gap-2.5 px-3 py-2">
                  <Icon className="size-4 shrink-0 text-muted-foreground" />
                  <div className="min-w-0 flex-1">
                    <div className="truncate">{s.kind === "personal" && s.owner_name ? `${s.name} · ${s.owner_name}` : s.name}</div>
                    <div className="truncate text-xs text-muted-foreground">
                      {DRIVE_KIND_LABEL[s.kind]}
                      {s.mode === "folder" && ` · ${t("Folder on the server")}`}
                    </div>
                  </div>
                  <span className="shrink-0 text-xs text-muted-foreground tabular-nums">{formatBytes(s.used_bytes)}</span>
                </li>
              );
            })}
          </ul>
        )}
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {t("Close")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

const PRESETS: { key: string; label: string; config: StorageConfig; hint: string }[] = [
  {
    key: "aws",
    label: "AWS S3",
    config: { endpoint: "", region: "", path_style: false, allow_http: false },
    hint: t("Leave the endpoint blank. The region can also be left blank; the bucket's region is detected automatically when you save."),
  },
  {
    key: "r2",
    label: "Cloudflare R2",
    config: { endpoint: t("https://<account ID>.r2.cloudflarestorage.com"), region: "auto", path_style: true, allow_http: false },
    hint: t("Replace the endpoint with your account ID (for the EU jurisdiction, <account ID>.eu.r2…). Don't use the public r2.dev URL. R2 has no egress fees."),
  },
  {
    key: "selfhost",
    label: t("Self-hosted (RustFS / MinIO)"),
    config: { endpoint: "http://rustfs:9000", region: "us-east-1", path_style: true, allow_http: true },
    hint: t("Internal http endpoints require \"Allow http\". MinIO is no longer freely distributed; RustFS (Apache 2.0) is recommended for new setups."),
  },
  {
    key: "other",
    label: t("Other S3-compatible service"),
    config: { endpoint: "", region: "", path_style: true, allow_http: false },
    hint: t("For example Backblaze B2, Wasabi, or Ceph."),
  },
];

function StorageDialog({ location, onClose, onSaved }: { location: StorageLocation | null; onClose(): void; onSaved(): void }) {
  const [name, setName] = useState(location?.name ?? "");
  const [kind, setKind] = useState<StorageKind>(location?.kind ?? "s3");
  const [cfg, setCfg] = useState<StorageConfig>(location?.config ?? { ...PRESETS[0].config });
  const [preset, setPreset] = useState(location ? "" : "aws");
  const [tested, setTested] = useState<"ok" | string | null>(null);
  // SFTP: host key provided by the server on the first connection test (for the admin to verify)
  const [seenKey, setSeenKey] = useState<string | null>(null);
  const [auth, setAuth] = useState<"password" | "key">("password");
  const builtin = !!location?.builtin;
  const set = (patch: Partial<StorageConfig>) => {
    setCfg({ ...cfg, ...patch });
    setTested(null);
  };

  const test = useMutation({
    mutationFn: () => api.testStorage({ id: location?.id, kind, config: cfg }),
    onSuccess: (r) => {
      setTested("ok");
      setSeenKey(r.host_key ?? null);
    },
    onError: (e) => setTested(e.message),
  });
  const save = useMutation({
    mutationFn: async () => {
      if (location) await api.updateStorage(location.id, { name: name.trim(), ...(builtin ? {} : { config: cfg }) });
      else await api.createStorage({ name: name.trim(), kind, config: cfg });
    },
    onSuccess: () => {
      toast.success(location ? t("Storage location updated") : t("Storage location added"));
      onSaved();
    },
  });

  const field = (key: keyof StorageConfig, label: string, props: React.ComponentProps<typeof Input> = {}) => (
    <div className="grid gap-1.5">
      <Label htmlFor={`st-${key}`}>{label}</Label>
      <Input id={`st-${key}`} value={(cfg[key] as string) ?? ""} onChange={(e) => set({ [key]: e.target.value })} autoComplete="off" {...props} />
    </div>
  );
  const check = (key: "path_style" | "allow_http", label: string) => (
    <label className="flex items-center gap-2 text-sm">
      <input type="checkbox" className="accent-brand" checked={!!cfg[key]} onChange={(e) => set({ [key]: e.target.checked })} />
      {label}
    </label>
  );
  const presetHint = PRESETS.find((p) => p.key === preset)?.hint;

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <form
          className="grid gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            save.mutate();
          }}
        >
          <DialogHeader>
            <DialogTitle>{location ? t("Edit \"{name}\"", { name: location.name }) : t("Add storage location")}</DialogTitle>
            <DialogDescription>{t("A connection test (writing, reading back, and deleting a small file) runs before saving.")}</DialogDescription>
          </DialogHeader>
          <div className="grid max-h-[60vh] gap-3 overflow-y-auto pr-1">
            <div className="grid gap-1.5">
              <Label htmlFor="st-name">{t("Name")}</Label>
              <Input id="st-name" value={name} onChange={(e) => setName(e.target.value)} placeholder={t("For example: Company RustFS")} autoFocus={!location} />
            </div>
            {builtin ? (
              <p className="rounded-md bg-muted/60 p-3 text-xs text-muted-foreground">
                {t("The built-in location's folder is set on the server with THIRTYFILE_STORAGE. Only its name can be changed.")}
              </p>
            ) : (
              <>
                {!location && (
                  <div className="grid gap-1.5">
                    <Label htmlFor="st-kind">{t("Type")}</Label>
                    <select
                      id="st-kind"
                      className={SELECT_CLASS}
                      value={kind}
                      onChange={(e) => {
                        const k = e.target.value as StorageKind;
                        setKind(k);
                        setCfg(k === "s3" ? { ...PRESETS[0].config } : { ...DEFAULTS[k] });
                        setSeenKey(null);
                        setPreset(k === "s3" ? "aws" : "");
                        setTested(null);
                      }}
                    >
                      {KINDS.map((k) => (
                        <option key={k} value={k}>
                          {STORAGE_KIND_LABEL[k]}
                        </option>
                      ))}
                    </select>
                  </div>
                )}
                {kind === "local" ? (
                  <>{field("path", t("Folder path (absolute path on the server; can be a NAS mount point)"), { placeholder: t("/mnt/nas/thirtyfile or D:\\thirtyfile") })}</>
                ) : kind === "sftp" || kind === "ftp" ? (
                  <RemoteFields
                    kind={kind}
                    cfg={cfg}
                    set={set}
                    hasSecret={!!location?.has_secret}
                    auth={auth}
                    setAuth={setAuth}
                    seenKey={seenKey}
                  />
                ) : (
                  <>
                    {!location && (
                      <div className="grid gap-1.5">
                        <Label htmlFor="st-preset">{t("Service")}</Label>
                        <select
                          id="st-preset"
                          className={SELECT_CLASS}
                          value={preset}
                          onChange={(e) => {
                            const p = PRESETS.find((x) => x.key === e.target.value);
                            if (!p) return;
                            setPreset(p.key);
                            setCfg({ ...cfg, ...p.config });
                            setTested(null);
                          }}
                        >
                          {PRESETS.map((p) => (
                            <option key={p.key} value={p.key}>
                              {p.label}
                            </option>
                          ))}
                        </select>
                        {presetHint && <p className="text-xs text-muted-foreground">{presetHint}</p>}
                      </div>
                    )}
                    {field("endpoint", t("Endpoint (leave blank for AWS)"), { placeholder: "https://s3.example.com" })}
                    <div className="grid grid-cols-2 gap-3">
                      {field("bucket", "Bucket")}
                      {field("region", t("Region"), { placeholder: t("Leave blank to auto-detect on AWS") })}
                    </div>
                    {field("prefix", t("Path prefix (optional; lets multiple systems share a bucket)"), { placeholder: "thirtyfile" })}
                    {field("access_key_id", "Access Key ID")}
                    {field("secret_access_key", "Secret Access Key", {
                      type: "password",
                      placeholder: location?.has_secret ? t("Already set; leave blank to keep it") : "",
                      autoComplete: "new-password",
                    })}
                    <div className="flex flex-wrap gap-x-5 gap-y-1.5">
                      {check("path_style", t("Path-style URLs (recommended for self-hosted services and R2)"))}
                      {check("allow_http", t("Allow http (internal networks only)"))}
                    </div>
                  </>
                )}
                {tested && (
                  <p
                    className={cn(
                      "flex items-center gap-1.5 text-sm",
                      tested === "ok" ? "text-emerald-600 dark:text-emerald-400" : "text-destructive",
                    )}
                  >
                    {tested === "ok" ? <CheckCircle2Icon className="size-4" /> : <XCircleIcon className="size-4" />}
                    {tested === "ok" ? t("Connection test succeeded") : tested}
                  </p>
                )}
              </>
            )}
            <ErrorText>{save.error?.message}</ErrorText>
          </div>
          <DialogFooter>
            {!builtin && (
              <Button type="button" variant="outline" className="sm:mr-auto" disabled={test.isPending} onClick={() => test.mutate()}>
                {test.isPending ? <Loader2Icon className="animate-spin" /> : <PlugZapIcon />} {t("Test connection")}
              </Button>
            )}
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={save.isPending || !name.trim()}>
              {save.isPending && <Loader2Icon className="animate-spin" />}
              {location ? t("Save") : t("New")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** Connection fields for SFTP / FTP */
function RemoteFields({
  kind,
  cfg,
  set,
  hasSecret,
  auth,
  setAuth,
  seenKey,
}: {
  kind: "sftp" | "ftp";
  cfg: StorageConfig;
  set(patch: Partial<StorageConfig>): void;
  hasSecret: boolean;
  auth: "password" | "key";
  setAuth(a: "password" | "key"): void;
  seenKey: string | null;
}) {
  const keep = hasSecret ? t("Already set; leave blank to keep it") : "";
  const text = (key: keyof StorageConfig, label: string, props: React.ComponentProps<typeof Input> = {}) => (
    <div className="grid gap-1.5">
      <Label htmlFor={`st-${key}`}>{label}</Label>
      <Input id={`st-${key}`} value={(cfg[key] as string) ?? ""} onChange={(e) => set({ [key]: e.target.value })} autoComplete="off" {...props} />
    </div>
  );
  const box = (key: "tls" | "tls_insecure", label: string) => (
    <label className="flex items-center gap-2 text-sm">
      <input type="checkbox" className="accent-brand" checked={!!cfg[key]} onChange={(e) => set({ [key]: e.target.checked })} />
      {label}
    </label>
  );
  return (
    <>
      <div className="grid grid-cols-[1fr_96px] gap-3">
        {text("host", t("Host"), { placeholder: kind === "sftp" ? t("nas.local or 192.168.1.10") : "ftp.example.com" })}
        <div className="grid gap-1.5">
          <Label htmlFor="st-port">{t("Port")}</Label>
          <Input
            id="st-port"
            inputMode="numeric"
            value={cfg.port ?? ""}
            onChange={(e) => set({ port: Number(e.target.value.replace(/\D/g, "")) || undefined })}
            placeholder={kind === "sftp" ? "22" : "21"}
          />
        </div>
      </div>
      {text("username", t("Username"))}
      {kind === "sftp" && (
        <div className="grid gap-1.5">
          <Label htmlFor="st-auth">{t("Sign-in methods")}</Label>
          <select id="st-auth" className={SELECT_CLASS} value={auth} onChange={(e) => setAuth(e.target.value as "password" | "key")}>
            <option value="password">{t("Password")}</option>
            <option value="key">{t("Private key (recommended)")}</option>
          </select>
        </div>
      )}
      {kind === "sftp" && auth === "key" ? (
        <>
          <div className="grid gap-1.5">
            <Label htmlFor="st-private_key">{t("Private key")}</Label>
            <Textarea
              id="st-private_key"
              rows={4}
              className="font-mono text-xs"
              value={cfg.private_key ?? ""}
              onChange={(e) => set({ private_key: e.target.value })}
              placeholder={keep || "-----BEGIN OPENSSH PRIVATE KEY-----\n…"}
              spellCheck={false}
            />
          </div>
          {text("key_passphrase", t("Private key passphrase (leave blank if none)"), { type: "password", autoComplete: "new-password", placeholder: keep })}
        </>
      ) : (
        text("password", t("Password"), { type: "password", autoComplete: "new-password", placeholder: keep })
      )}
      {text("path", t("Storage folder (blank = default folder after sign-in)"), { placeholder: kind === "sftp" ? "/volume1/thirtyfile" : "/thirtyfile" })}
      {kind === "ftp" && (
        <div className="grid gap-1.5">
          <div className="flex flex-wrap gap-x-5 gap-y-1.5">
            {box("tls", t("Use FTPS encryption (AUTH TLS, recommended)"))}
            {cfg.tls && box("tls_insecure", t("Skip certificate validation (self-signed certificate)"))}
          </div>
          {!cfg.tls && (
            <p className="flex items-start gap-1.5 rounded-md bg-amber-500/10 p-2 text-xs text-amber-700 dark:text-amber-300">
              <ShieldAlertIcon className="mt-px size-3.5 shrink-0" />
              {t("Not encrypted: usernames, passwords, and file contents are sent in plain text. Use only on a trusted internal network.")}
            </p>
          )}
          {cfg.tls && cfg.tls_insecure && (
            <p className="text-xs text-muted-foreground">{t("The connection is still encrypted, but the server's identity can't be verified. Use only on an internal network.")}</p>
          )}
        </div>
      )}
      {kind === "sftp" && (cfg.host_key || seenKey) && (
        <div className="grid gap-1 rounded-md bg-muted/60 p-2.5 text-xs">
          <div className="flex items-center gap-2">
            <span className="font-medium">{t("Host key")}</span>
            {cfg.host_key && (
              <Button
                type="button"
                size="sm"
                variant="ghost"
                className="ml-auto h-6 px-2 text-xs"
                title={t("Use after the server is reinstalled or its key changes: the current key is recorded again when you save")}
                onClick={() => set({ host_key: "" })}
              >
                {t("Reset")}
              </Button>
            )}
          </div>
          <code className="break-all text-muted-foreground">{cfg.host_key || seenKey}</code>
          <p className="text-muted-foreground">
            {cfg.host_key
              ? t("Every future connection is checked against this key and refused if it doesn't match, protecting against impostor servers.")
              : t("First connection: verify this fingerprint with the server administrator (ssh-keygen -lf). It will be recorded once you add the location.")}
          </p>
        </div>
      )}
    </>
  );
}
