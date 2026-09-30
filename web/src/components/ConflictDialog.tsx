/**
 * "Replace or skip": asked before uploading, moving, copying or restoring items whose names the destination already
 * has, one item at a time as in Windows, with "Do this for the next conflicts"
 */
import { useState } from "react";
import { CopyIcon, FileIcon as FileGlyph, FolderIcon, ReplaceIcon, SkipForwardIcon } from "lucide-react";
import { api, type NameConflict } from "@/api";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { type Answer, type Clash, type Resolution, numberedName, resolveAll } from "@/lib/conflicts";
import { t } from "@/lib/i18n";
import { useMe } from "@/lib/session";
import { createStore, useStore } from "@/lib/store";
import { formatBytes, formatDateTime } from "@/lib/utils";

/** What is being done: the choices read a little differently for each */
export type ConflictOp = "upload" | "move" | "copy" | "restore";

interface Request {
  clash: Clash;
  remaining: number;
  op: ConflictOp;
  resolve(answer: Answer | null): void;
}

const current = createStore<Request | null>(null);

function ask(clash: Clash, remaining: number, op: ConflictOp): Promise<Answer | null> {
  current.get()?.resolve(null);
  return new Promise((resolve) => current.set({ clash, remaining, op, resolve }));
}

/** Asks about every clash (see `resolveAll`): the answers by key, or null when cancelled */
export function resolveConflicts(clashes: readonly Clash[], op: ConflictOp) {
  return resolveAll(clashes, (clash, remaining) => ask(clash, remaining, op));
}

/** The server's list as clashes keyed by item id */
export function clashesOf(list: readonly NameConflict[]): Clash[] {
  return list.map((c) => ({
    key: c.id ?? c.name,
    name: c.name,
    kind: c.kind ?? "file",
    size: c.kind === "folder" ? undefined : (c.size ?? undefined),
    modified: c.updated_at ?? undefined,
    existing: { kind: c.existing.kind, size: c.existing.size, updated_at: c.existing.updated_at },
  }));
}

/**
 * Checks `ids` against the destination (or, without one, against the folders they are restored to) and asks about
 * clashes: the answers to send with the request, or null when cancelled
 */
export async function askBeforeTransfer(op: Exclude<ConflictOp, "upload">, ids: string[], dest?: string): Promise<Record<string, Resolution> | null> {
  const found = await api.conflicts({ ids, dest_id: dest });
  if (!found.length) return {};
  const answers = await resolveConflicts(clashesOf(found), op);
  return answers && Object.fromEntries(answers);
}

function Details({ label, size, modified, folder }: { label: string; size?: number; modified?: number; folder: boolean }) {
  const Icon = folder ? FolderIcon : FileGlyph;
  return (
    <div className="flex min-w-0 items-start gap-2 rounded-md border px-3 py-2 text-sm">
      <Icon className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
      <div className="min-w-0">
        <div className="font-medium">{label}</div>
        <div className="text-xs text-muted-foreground">
          {[size !== undefined && !folder ? formatBytes(size) : null, modified ? t("Modified {date}", { date: formatDateTime(modified) }) : null]
            .filter(Boolean)
            .join(" · ") || " "}
        </div>
      </div>
    </div>
  );
}

function ConflictDialog({ req, onDone }: { req: Request; onDone(answer: Answer | null): void }) {
  const [forAll, setForAll] = useState(false);
  const versionKeep = useMe().version_keep;
  const { clash, op, remaining } = req;
  const folder = clash.existing.kind === "folder";
  const incomingFolder = clash.kind === "folder";
  // An uploaded folder is merged into the folder there; anything else takes the place of the item there
  const merge = op === "upload" && folder && incomingFolder;
  const choose = (choice: Resolution) => onDone({ choice, forAll });
  const options: { choice: Resolution; icon: typeof ReplaceIcon; title: string; hint?: string }[] = [
    {
      choice: "replace",
      icon: ReplaceIcon,
      title: merge
        ? t("Merge the folders, replacing files with the same names")
        : folder
          ? t("Replace the folder in the destination")
          : t("Replace the file in the destination"),
      hint:
        op !== "upload"
          ? t("The item there moves to the trash.")
          : merge || versionKeep <= 0
            ? undefined
            : t("Its current content is kept as an earlier version."),
    },
    { choice: "skip", icon: SkipForwardIcon, title: incomingFolder ? t("Skip this folder") : t("Skip this file") },
    {
      choice: "keep",
      icon: CopyIcon,
      title: merge ? t("Merge the folders, keeping both files when names clash") : t("Keep both"),
      hint: merge ? undefined : t("The new one is named \"{name}\".", { name: numberedName(clash.name, incomingFolder) }),
    },
  ];
  return (
    <Dialog open onOpenChange={(o) => !o && onDone(null)}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            {folder
              ? t("The destination already has a folder named \"{name}\"", { name: clash.name })
              : t("The destination already has a file named \"{name}\"", { name: clash.name })}
          </DialogTitle>
          <DialogDescription>{t("Choose what to do with this item.")}</DialogDescription>
        </DialogHeader>
        <div className="grid gap-2 sm:grid-cols-2">
          <Details label={op === "restore" ? t("From the trash") : t("New item")} size={clash.size} modified={clash.modified} folder={incomingFolder} />
          <Details label={t("In the destination")} size={clash.existing.size} modified={clash.existing.updated_at} folder={folder} />
        </div>
        <div className="grid gap-1.5">
          {options.map(({ choice, icon: Icon, title, hint }, i) => (
            <Button key={choice} variant="outline" className="h-auto justify-start py-2 text-left whitespace-normal" autoFocus={i === 0} onClick={() => choose(choice)}>
              <Icon />
              <span className="grid">
                <span>{title}</span>
                {hint && <span className="text-xs font-normal text-muted-foreground">{hint}</span>}
              </span>
            </Button>
          ))}
        </div>
        {remaining > 0 && (
          <Label className="flex items-center gap-2 font-normal">
            <Checkbox checked={forAll} onCheckedChange={(v) => setForAll(v === true)} />
            {t("Do this for the next {n} conflict|Do this for the next {n} conflicts", { n: remaining })}
          </Label>
        )}
        <DialogFooter>
          <Button variant="outline" onClick={() => onDone(null)}>
            {t("Cancel")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** Shows the questions asked with `resolveConflicts`; mounted once for the whole app */
export function ConflictHost() {
  const req = useStore(current);
  if (!req) return null;
  const done = (answer: Answer | null) => {
    if (current.get() === req) current.set(null);
    req.resolve(answer);
  };
  // A new key for each question, so "for all" starts unticked
  return <ConflictDialog key={`${req.clash.key}:${req.remaining}`} req={req} onDone={done} />;
}
