import { useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { ChevronRightIcon, FolderIcon, HardDriveIcon, HomeIcon, Loader2Icon } from "lucide-react";
import { api, type Crumb } from "@/api";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { useDrives } from "@/lib/drives";
import { t } from "@/lib/i18n";
import { cn } from "@/lib/utils";

function useSubmit(fn: () => Promise<void>) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = async (e?: FormEvent) => {
    e?.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await fn();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };
  return { busy, error, run };
}

export function ErrorText({ children }: { children: ReactNode }) {
  return children ? <p className="text-sm text-destructive">{children}</p> : null;
}

export function NameDialog(props: {
  title: string;
  label?: string;
  initial?: string;
  confirmText?: string;
  onSubmit(name: string): Promise<void>;
  onClose(): void;
}) {
  const [name, setName] = useState(props.initial ?? "");
  const ref = useRef<HTMLInputElement>(null);
  const { busy, error, run } = useSubmit(() => props.onSubmit(name.trim()));

  useEffect(() => {
    // When renaming, select only the base name so it can be typed over directly
    const timer = setTimeout(() => {
      const el = ref.current;
      if (!el) return;
      el.focus();
      const dot = el.value.lastIndexOf(".");
      el.setSelectionRange(0, dot > 0 ? dot : el.value.length);
    }, 50);
    return () => clearTimeout(timer);
  }, []);

  return (
    <Dialog open onOpenChange={(o) => !o && props.onClose()}>
      <DialogContent>
        <form onSubmit={run} className="grid gap-4">
          <DialogHeader>
            <DialogTitle>{props.title}</DialogTitle>
          </DialogHeader>
          <div className="grid gap-2">
            {props.label && <Label htmlFor="name-input">{props.label}</Label>}
            <Input id="name-input" ref={ref} value={name} onChange={(e) => setName(e.target.value)} autoComplete="off" />
            <ErrorText>{error}</ErrorText>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={props.onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={busy || !name.trim()}>
              {busy && <Loader2Icon className="animate-spin" />}
              {props.confirmText ?? t("OK")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

export function ConfirmDialog(props: {
  title: string;
  description?: ReactNode;
  confirmText?: string;
  destructive?: boolean;
  onConfirm(): Promise<void>;
  onClose(): void;
}) {
  const { busy, error, run } = useSubmit(props.onConfirm);
  return (
    <Dialog open onOpenChange={(o) => !o && props.onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{props.title}</DialogTitle>
          {props.description && <DialogDescription>{props.description}</DialogDescription>}
        </DialogHeader>
        <ErrorText>{error}</ErrorText>
        <DialogFooter>
          <Button variant="outline" onClick={props.onClose}>
            {t("Cancel")}
          </Button>
          <Button variant={props.destructive ? "destructive" : "default"} disabled={busy} onClick={() => run()} autoFocus>
            {busy && <Loader2Icon className="animate-spin" />}
            {props.confirmText ?? t("OK")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** Pick a destination folder (move / copy) */
export function FolderPickerDialog(props: {
  title: string;
  confirmText: string;
  startId: string;
  excludeIds: Set<string>;
  onPick(folderId: string): Promise<void>;
  onClose(): void;
}) {
  const drives = useDrives();
  // base: where browsing starts (a space root, or a folder accessed via sharing)
  const [base, setBase] = useState<Crumb>({ id: "root", name: t("My files") });
  const [trail, setTrail] = useState<Crumb[]>([]);
  const current = trail.length ? trail[trail.length - 1].id : base.id;
  const [picked, setPicked] = useState(false);

  // Open the current folder by default
  const start = useQuery({ queryKey: ["node", props.startId], queryFn: () => api.node(props.startId), enabled: !picked });
  useEffect(() => {
    if (start.data && !picked) {
      const d = start.data;
      if (d.via_share && d.path.length) {
        setBase(d.path[0]);
        setTrail(d.path.slice(1));
      } else {
        setBase({ id: d.drive.root_id, name: d.drive.name });
        setTrail(d.path);
      }
      setPicked(true);
    }
  }, [start.data, picked]);

  const folders = useQuery({
    queryKey: ["children", current, "folders"],
    queryFn: () => api.children(current, "name", "asc", true),
  });
  const { busy, error, run } = useSubmit(() => props.onPick(current));

  return (
    <Dialog open onOpenChange={(o) => !o && props.onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{props.title}</DialogTitle>
        </DialogHeader>
        <label className="flex items-center gap-2 text-sm">
          <HardDriveIcon className="size-4 text-muted-foreground" />
          <select
            className="h-8 flex-1 rounded-md border bg-background px-2 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring"
            aria-label={t("Spaces")}
            value={drives.data?.some((d) => d.root_id === base.id) ? base.id : ""}
            onChange={(e) => {
              const d = drives.data?.find((x) => x.root_id === e.target.value);
              if (d) {
                setBase({ id: d.root_id, name: d.name });
                setTrail([]);
              }
            }}
          >
            {!drives.data?.some((d) => d.root_id === base.id) && <option value="">{t("Shared with me: {name}", { name: base.name })}</option>}
            {drives.data?.map((d) => (
              <option key={d.id} value={d.root_id}>
                {d.name}
                {d.kind === "team" ? t(" (team space)") : d.kind === "company" ? t(" (company-wide)") : ""}
              </option>
            ))}
          </select>
        </label>
        <div className="flex flex-wrap items-center gap-0.5 text-sm">
          <button type="button" className="flex items-center gap-1 rounded px-1.5 py-0.5 hover:bg-muted" onClick={() => setTrail([])}>
            <HomeIcon className="size-3.5" /> {base.name}
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
          {folders.isLoading ? (
            <div className="flex h-full items-center justify-center text-muted-foreground">
              <Loader2Icon className="size-5 animate-spin" />
            </div>
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
          <Button variant="outline" onClick={props.onClose}>
            {t("Cancel")}
          </Button>
          <Button disabled={busy || props.excludeIds.has(current)} onClick={() => run()}>
            {busy && <Loader2Icon className="animate-spin" />}
            {props.confirmText}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function ChangePasswordDialog({ onClose }: { onClose(): void }) {
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [confirm, setConfirm] = useState("");
  const { busy, error, run } = useSubmit(async () => {
    if (next !== confirm) throw new Error(t("The new passwords don't match"));
    await api.changePassword(current, next);
    onClose();
  });
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent>
        <form onSubmit={run} className="grid gap-4">
          <DialogHeader>
            <DialogTitle>{t("Change password")}</DialogTitle>
            <DialogDescription>{t("After you change it, you'll be signed out on other devices.")}</DialogDescription>
          </DialogHeader>
          <div className="grid gap-2">
            <Label htmlFor="pw-cur">{t("Current password")}</Label>
            <Input id="pw-cur" type="password" value={current} onChange={(e) => setCurrent(e.target.value)} autoComplete="current-password" />
            <Label htmlFor="pw-new">{t("New password (at least 6 characters)")}</Label>
            <Input id="pw-new" type="password" value={next} onChange={(e) => setNext(e.target.value)} autoComplete="new-password" />
            <Label htmlFor="pw-cfm">{t("Confirm new password")}</Label>
            <Input id="pw-cfm" type="password" value={confirm} onChange={(e) => setConfirm(e.target.value)} autoComplete="new-password" />
            <ErrorText>{error}</ErrorText>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={busy || !current || !next}>
              {busy && <Loader2Icon className="animate-spin" />}
              {t("Change password")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
