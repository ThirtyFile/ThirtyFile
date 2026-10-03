//! Coloured tags in the interface (lib/tags.ts): the dots after an item's name, the list of tags with their names, the
//! menu that puts tags on items and takes them off, and the dialog that makes or changes a tag. The same in every
//! interface style.

import { memo, useEffect, useId, useRef, useState } from "react";
import { useQueryClient, type QueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { Loader2Icon, MinusIcon, PlusIcon, TagIcon } from "lucide-react";
import type { Node, Tag, TagColor } from "@/api";
import { ErrorText, errorProps } from "@/components/dialogs";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import {
  DropdownMenuCheckboxItem,
  DropdownMenuItem,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { confirm } from "@/lib/confirm";
import { t } from "@/lib/i18n";
import type { Picked } from "@/lib/span";
import { useStore } from "@/lib/store";
import { MAX_TAG_NAME, TAG_COLORS, changeTags, colorOf, createTag, deleteTag, editTag, tagDialog, tagNames, tagState, tagsOn, updateTag, useTags } from "@/lib/tags";
import { useSubmit } from "@/lib/useSubmit";
import { cn, errorMessage } from "@/lib/utils";

/** A tag's colour, as a dot; its name is always given beside it or as the label of what holds it */
export function TagDot({ color, className }: { color: TagColor; className?: string }) {
  return <span aria-hidden className={cn("inline-block size-2.5 shrink-0 rounded-full ring-1 ring-background", colorOf(color).dot, className)} />;
}

/** Dots shown at most after a name; the label names every tag */
const MAX_DOTS = 3;

/**
 * The tags on an item, as dots after its name (the file list, and other views of items): read out and shown on
 * hovering as "Tags: Urgent, Later". Nothing when the item has none of the person's tags.
 */
export const TagDots = memo(function TagDots({ ids, className }: { ids?: readonly number[]; className?: string }) {
  const { byId } = useTags(!!ids?.length);
  const tags = tagsOn(ids, byId);
  if (!tags.length) return null;
  const label = t("Tags: {names}", { names: tagNames(tags) });
  return (
    <span role="img" aria-label={label} title={label} data-tags className={cn("flex shrink-0 items-center -space-x-1", className)}>
      {tags.slice(0, MAX_DOTS).map((tag) => (
        <TagDot key={tag.id} color={tag.color} />
      ))}
    </span>
  );
});

/** The tags on an item with their names (the details pane, the Tags column); `empty` when there are none */
export function TagNames({ ids, empty, className }: { ids?: readonly number[]; empty?: string; className?: string }) {
  const { byId } = useTags(!!ids?.length);
  const tags = tagsOn(ids, byId);
  if (!tags.length) return empty ? <span className={className}>{empty}</span> : null;
  return (
    <ul aria-label={t("Tags")} className={cn("flex min-w-0 flex-wrap items-center gap-x-2.5 gap-y-1", className)}>
      {tags.map((tag) => (
        <li key={tag.id} className="flex min-w-0 items-center gap-1">
          <TagDot color={tag.color} />
          <span className="truncate">{tag.name}</span>
        </li>
      ))}
    </ul>
  );
}

/**
 * Menu items putting the person's tags on the selected items or taking them off: one per tag, checked when every item
 * has it (a dash when some have it: choosing it puts it on all of them), then "New tag…", which makes a tag and puts it
 * on them
 */
export function TagMenuItems({ nodes, picked }: { nodes: readonly Node[]; picked: Picked }) {
  const qc = useQueryClient();
  const { tags } = useTags();
  const partly = !!picked.span;
  return (
    <>
      {tags.map((tag) => {
        const state = tagState(nodes, tag.id, partly);
        return (
          <DropdownMenuCheckboxItem
            key={tag.id}
            checked={state === true}
            aria-checked={state === "mixed" ? "mixed" : state}
            closeOnClick
            onCheckedChange={() => void changeTags(qc, picked, nodes, state === true ? [] : [tag.id], state === true ? [tag.id] : [])}
          >
            <TagDot color={tag.color} />
            <span className="truncate">{tag.name}</span>
            {state === "mixed" && <MinusIcon aria-hidden className="absolute right-2" />}
          </DropdownMenuCheckboxItem>
        );
      })}
      {tags.length > 0 && <DropdownMenuSeparator />}
      <DropdownMenuItem
        onClick={async () => {
          const tag = await editTag();
          if (tag) await changeTags(qc, picked, nodes, [tag.id], []);
        }}
      >
        <PlusIcon /> {t("New tag…")}
      </DropdownMenuItem>
    </>
  );
}

/** "Tags ›" with the tag items, for the context menu of items */
export function TagSubmenu({ nodes, picked }: { nodes: readonly Node[]; picked: Picked }) {
  return (
    <DropdownMenuSub>
      <DropdownMenuSubTrigger>
        <TagIcon /> {t("Tags")}
      </DropdownMenuSubTrigger>
      <DropdownMenuSubContent className="max-h-80 w-56 overflow-y-auto">
        <TagMenuItems nodes={nodes} picked={picked} />
      </DropdownMenuSubContent>
    </DropdownMenuSub>
  );
}

export const tagPath = (tag: Pick<Tag, "id">) => `/tags/${tag.id}`;

/** Asks, then deletes a tag (it comes off every item); leaves the tag's page when it is the one shown */
export async function askToDeleteTag(qc: QueryClient, tag: Tag, leave?: () => void) {
  const ok = await confirm({
    title: t('Delete the tag "{name}"?', { name: tag.name }),
    description: t("It comes off every item it is on. The items themselves stay as they are."),
    confirmText: t("Delete"),
    destructive: true,
  });
  if (!ok) return;
  try {
    await deleteTag(qc, tag.id);
    leave?.();
  } catch (e) {
    toast.error(errorMessage(e, t("Couldn't delete the tag")));
  }
}

/** Recolours a tag, saying so when it can't */
export async function recolor(qc: QueryClient, tag: Tag, color: Tag["color"]) {
  if (color === tag.color) return;
  try {
    await updateTag(qc, tag.id, { color });
  } catch (e) {
    toast.error(errorMessage(e, t("Couldn't change the tag")));
  }
}

/** The colours as radio items, the tag's own checked */
export function ColorItems({ tag }: { tag: Tag }) {
  const qc = useQueryClient();
  return (
    <DropdownMenuRadioGroup value={tag.color} onValueChange={(c) => void recolor(qc, tag, c as Tag["color"])}>
      {TAG_COLORS.map((c) => (
        <DropdownMenuRadioItem key={c.id} value={c.id} closeOnClick>
          <TagDot color={c.id} /> {c.label()}
        </DropdownMenuRadioItem>
      ))}
    </DropdownMenuRadioGroup>
  );
}

/** The colours to choose from, each with its name */
function ColorChoice({ value, onChange }: { value: TagColor; onChange(c: TagColor): void }) {
  const name = useId();
  return (
    <fieldset className="grid gap-2">
      <legend className="mb-2 text-sm font-medium">{t("Color")}</legend>
      <div className="flex flex-wrap gap-x-3 gap-y-2">
        {TAG_COLORS.map((c) => (
          <label key={c.id} className="flex cursor-pointer items-center gap-1.5 rounded-md px-1 py-0.5 text-sm has-focus-visible:ring-2 has-focus-visible:ring-ring">
            <input type="radio" name={name} value={c.id} checked={value === c.id} onChange={() => onChange(c.id)} className="sr-only" />
            <span
              aria-hidden
              className={cn("flex size-5 items-center justify-center rounded-full", c.dot, value === c.id && "ring-2 ring-foreground ring-offset-2 ring-offset-background")}
            />
            {c.label()}
          </label>
        ))}
      </div>
    </fieldset>
  );
}

/** Makes a tag, or renames and recolours `tag` */
function TagDialog({ tag, onDone }: { tag?: Tag; onDone(tag: Tag | null): void }) {
  const qc = useQueryClient();
  const [name, setName] = useState(tag?.name ?? "");
  const [color, setColor] = useState<TagColor>(tag?.color ?? "red");
  const ref = useRef<HTMLInputElement>(null);
  const errorId = useId();
  const nameId = useId();
  const { busy, error, run } = useSubmit(async () => {
    const saved = tag ? await updateTag(qc, tag.id, { name: name.trim(), color }) : await createTag(qc, name.trim(), color);
    onDone(saved);
  });
  useEffect(() => {
    const timer = setTimeout(() => ref.current?.select(), 50);
    return () => clearTimeout(timer);
  }, []);
  return (
    <Dialog open onOpenChange={(o) => !o && onDone(null)}>
      <DialogContent>
        <form onSubmit={run} className="grid gap-4">
          <DialogHeader>
            <DialogTitle>{tag ? t("Edit tag") : t("New tag")}</DialogTitle>
            <DialogDescription>{t("Only you see your tags, also on items others can open.")}</DialogDescription>
          </DialogHeader>
          <div className="grid gap-2">
            <Label htmlFor={nameId}>{t("Name")}</Label>
            <Input id={nameId} ref={ref} value={name} maxLength={MAX_TAG_NAME} onChange={(e) => setName(e.target.value)} autoComplete="off" {...errorProps(error, errorId)} />
          </div>
          <ColorChoice value={color} onChange={setColor} />
          <ErrorText id={errorId}>{error}</ErrorText>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onDone(null)}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={busy || !name.trim()}>
              {busy && <Loader2Icon className="animate-spin" />}
              {tag ? t("Save") : t("Create")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** Shows the dialog asked for with editTag() (lib/tags.ts); mounted once for the signed-in app */
export function TagDialogHost() {
  const req = useStore(tagDialog);
  if (!req) return null;
  return (
    <TagDialog
      key={req.tag?.id ?? "new"}
      tag={req.tag}
      onDone={(tag) => {
        if (tagDialog.get() === req) tagDialog.set(null);
        req.resolve(tag);
      }}
    />
  );
}
