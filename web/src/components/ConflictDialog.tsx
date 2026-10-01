/**
 * "Replace or skip": asked before uploading, moving, copying or restoring items whose names the destination already
 * has, one item at a time as in Windows, with "Do this for the next conflicts"
 */
import { useState } from "react";
import { CopyIcon, FileIcon as FileGlyph, FolderIcon, ReplaceIcon, SkipForwardIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { type Answer, type ConflictRequest, type Resolution, conflictRequest, numberedName } from "@/lib/conflicts";
import { t } from "@/lib/i18n";
import { useMe } from "@/lib/session";
import { useStore } from "@/lib/store";
import { formatBytes, formatDateTime } from "@/lib/utils";

function Details({ label, size, modified, folder }: { label: string; size?: number; modified?: number; folder: boolean }) {
  const Icon = folder ? FolderIcon : FileGlyph;
  return (
    <div className="flex min-w-0 items-start gap-2 rounded-md border px-3 py-2 text-sm">
      <Icon className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
      <div className="min-w-0">
        <div className="font-medium">{label}</div>
        <div className="text-xs text-muted-foreground">
          {[size !== undefined && !folder ? formatBytes(size) : null, modified ? t("Modified {date}", { date: formatDateTime(modified) }) : null].filter(Boolean).join(" · ") || " "}
        </div>
      </div>
    </div>
  );
}

function ConflictDialog({ req, onDone }: { req: ConflictRequest; onDone(answer: Answer | null): void }) {
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
      title: merge ? t("Merge the folders, replacing files with the same names") : folder ? t("Replace the folder in the destination") : t("Replace the file in the destination"),
      hint: op !== "upload" ? t("The item there moves to the trash.") : merge || versionKeep <= 0 ? undefined : t("Its current content is kept as an earlier version."),
    },
    { choice: "skip", icon: SkipForwardIcon, title: incomingFolder ? t("Skip this folder") : t("Skip this file") },
    {
      choice: "keep",
      icon: CopyIcon,
      title: merge ? t("Merge the folders, keeping both files when names clash") : t("Keep both"),
      hint: merge ? undefined : t('The new one is named "{name}".', { name: numberedName(clash.name, incomingFolder) }),
    },
  ];
  return (
    <Dialog open onOpenChange={(o) => !o && onDone(null)}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            {folder ? t('The destination already has a folder named "{name}"', { name: clash.name }) : t('The destination already has a file named "{name}"', { name: clash.name })}
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
  const req = useStore(conflictRequest);
  if (!req) return null;
  const done = (answer: Answer | null) => {
    if (conflictRequest.get() === req) conflictRequest.set(null);
    req.resolve(answer);
  };
  // A new key for each question, so "for all" starts unticked
  return <ConflictDialog key={`${req.clash.key}:${req.remaining}`} req={req} onDone={done} />;
}
