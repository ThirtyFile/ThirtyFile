import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronRightIcon, FolderIcon, FolderPlusIcon, HardDriveIcon, HomeIcon, Loader2Icon } from "lucide-react";
import { api, type Crumb } from "@/api";
import { keys } from "@/api/queryKeys";
import { ErrorState } from "@/components/ErrorState";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { useDrives } from "@/lib/drives";
import { t } from "@/lib/i18n";
import { refreshFiles } from "@/lib/queries";
import { cn } from "@/lib/utils";
import { useSubmit } from "@/lib/useSubmit";
import { NativeSelect } from "@/components/ui/native-select";
import { ErrorText, NameDialog } from "@/components/dialogs";

/** Pick a destination folder (move / copy) */
export function FolderPickerDialog(props: {
  title: string;
  confirmText: string;
  /** The folder to start in; null: the first of the person's spaces */
  startId: string | null;
  excludeIds: Set<string>;
  onPick(folderId: string): Promise<void>;
  onClose(): void;
}) {
  const drives = useDrives();
  // base: where browsing starts (a space root, or a folder accessed via sharing); null until it is known
  const [base, setBase] = useState<Crumb | null>(null);
  const [trail, setTrail] = useState<Crumb[]>([]);
  const current = trail.length ? trail[trail.length - 1].id : (base?.id ?? "");
  const [picked, setPicked] = useState(false);

  // Open the current folder by default
  const startId = props.startId;
  const start = useQuery({ queryKey: keys.node(startId), queryFn: () => api.node(startId!), enabled: !picked && !!startId });
  useEffect(() => {
    if (picked) return;
    if (start.data) {
      const d = start.data;
      if (d.via_share && d.path.length) {
        setBase(d.path[0]);
        setTrail(d.path.slice(1));
      } else {
        setBase({ id: d.drive.root_id, name: d.drive.name });
        setTrail(d.path);
      }
      setPicked(true);
    } else if ((!startId || start.isError) && drives.data?.length) {
      // No folder to start in (someone without "My files"), or it can't be opened: the first space
      setBase({ id: drives.data[0].root_id, name: drives.data[0].name });
      setTrail([]);
      setPicked(true);
    }
  }, [start.data, start.isError, startId, drives.data, picked]);

  // Someone without any space (and not starting in a shared folder) has nowhere to put things
  const nowhere = !current && drives.data?.length === 0 && (!startId || start.isError);

  const folders = useQuery({
    queryKey: keys.folders(current),
    queryFn: ({ signal }) => api.children(current, "name", "asc", true, signal),
    enabled: !!current,
  });
  const { busy, error, run } = useSubmit(() => props.onPick(current));
  const qc = useQueryClient();
  const [naming, setNaming] = useState(false);
  // Like Windows' Move to dialog: a new folder is made inside the folder being browsed, then opened so it's the destination
  const newFolderName = () => {
    const taken = new Set(folders.data?.map((f) => f.name.toLowerCase()));
    for (let i = 1; ; i++) {
      const name = i === 1 ? t("New folder") : `${t("New folder")} (${i})`;
      if (!taken.has(name.toLowerCase())) return name;
    }
  };
  const createFolder = async (name: string) => {
    const f = await api.createFolder(current, name);
    void refreshFiles(qc, { folders: [current], contents: true });
    setTrail([...trail, { id: f.id, name: f.name }]);
    setNaming(false);
  };

  return (
    <Dialog open onOpenChange={(o) => !o && props.onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{props.title}</DialogTitle>
        </DialogHeader>
        <label className="flex items-center gap-2 text-sm">
          <HardDriveIcon className="size-4 text-muted-foreground" />
          <NativeSelect
            className="flex-1"
            aria-label={t("Spaces")}
            value={base && drives.data?.some((d) => d.root_id === base.id) ? base.id : ""}
            onChange={(e) => {
              const d = drives.data?.find((x) => x.root_id === e.target.value);
              if (d) {
                setBase({ id: d.root_id, name: d.name });
                setTrail([]);
              }
            }}
          >
            {base && !drives.data?.some((d) => d.root_id === base.id) && <option value="">{t("Shared with me: {name}", { name: base.name })}</option>}
            {drives.data?.map((d) => (
              <option key={d.id} value={d.root_id}>
                {d.kind === "team" ? t("{name} (team space)", { name: d.name }) : d.kind === "company" ? t("{name} (company-wide)", { name: d.name }) : d.name}
              </option>
            ))}
          </NativeSelect>
        </label>
        <div className="flex flex-wrap items-center gap-0.5 text-sm">
          <button type="button" className="flex items-center gap-1 rounded px-1.5 py-0.5 hover:bg-muted" onClick={() => setTrail([])}>
            <HomeIcon className="size-3.5" /> {base?.name ?? "…"}
          </button>
          {trail.map((c, i) => (
            <span key={c.id} className="flex items-center gap-0.5">
              <ChevronRightIcon className="size-3.5 text-muted-foreground" />
              <button type="button" className="max-w-40 truncate rounded px-1.5 py-0.5 hover:bg-muted" onClick={() => setTrail(trail.slice(0, i + 1))}>
                {c.name}
              </button>
            </span>
          ))}
        </div>
        <div className="h-64 overflow-y-auto rounded-lg border">
          {nowhere ? (
            <div className="flex h-full items-center justify-center px-4 text-center text-sm text-muted-foreground">{t("You don't have access to any space to put it in.")}</div>
          ) : folders.isLoading || !current ? (
            <div className="flex h-full items-center justify-center text-muted-foreground">
              <Loader2Icon className="size-5 animate-spin" />
            </div>
          ) : folders.error ? (
            <ErrorState message={folders.error.message} onRetry={() => folders.refetch()} className="h-full min-h-0" />
          ) : folders.data?.length ? (
            folders.data.map((f) => {
              const disabled = props.excludeIds.has(f.id);
              return (
                <button
                  key={f.id}
                  type="button"
                  disabled={disabled}
                  onClick={() => setTrail([...trail, { id: f.id, name: f.name }])}
                  className={cn("flex w-full items-center gap-2 border-b border-border/60 px-3 py-2 text-left text-sm hover:bg-accent", disabled && "opacity-40")}
                >
                  <FolderIcon className="size-4 shrink-0 fill-amber-400/30 text-amber-500" />
                  <span className="truncate">{f.name}</span>
                  <ChevronRightIcon className="ml-auto size-4 text-muted-foreground" />
                </button>
              );
            })
          ) : (
            <div className="flex h-full items-center justify-center text-sm text-muted-foreground">{t("No subfolders")}</div>
          )}
        </div>
        <ErrorText>{error}</ErrorText>
        <DialogFooter>
          <Button variant="outline" className="sm:mr-auto" disabled={!folders.data || props.excludeIds.has(current)} onClick={() => setNaming(true)}>
            <FolderPlusIcon /> {t("New folder")}
          </Button>
          <Button variant="outline" onClick={props.onClose}>
            {t("Cancel")}
          </Button>
          <Button disabled={busy || !current || props.excludeIds.has(current)} onClick={() => run()}>
            {busy && <Loader2Icon className="animate-spin" />}
            {props.confirmText}
          </Button>
        </DialogFooter>
        {naming && (
          <NameDialog title={t("New folder")} label={t("Name")} initial={newFolderName()} confirmText={t("Create")} onSubmit={createFolder} onClose={() => setNaming(false)} />
        )}
      </DialogContent>
    </Dialog>
  );
}
