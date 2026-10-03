/**
 * Coloured tags: each person's own labels for files and folders (server/src/tags.rs). Only the person who made a tag
 * sees it. The server sends each item with the ids of the person's tags on it; the names and colours come from the
 * list of their tags (`useTags`), read once and kept up to date by the changes made here.
 *
 * Colour is never the only way to tell tags apart: wherever a dot shows, its tag's name is there too (as text, or as the
 * dot's label and tooltip).
 */
import { useMemo } from "react";
import { useQuery, type QueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { api, type Node, type Tag, type TagColor } from "@/api";
import { keys, queries } from "@/api/queryKeys";
import { reportShown } from "@/lib/errorReport";
import { t } from "@/lib/i18n";
import { refreshFiles } from "@/lib/queries";
import { eachBatch, type Picked } from "@/lib/span";
import { createStore } from "@/lib/store";
import { errorMessage, nameCollator } from "@/lib/utils";

/** The palette, in the order offered, with each colour's name and the classes that draw its dot */
export const TAG_COLORS: readonly { id: TagColor; label: () => string; dot: string }[] = [
  { id: "red", label: () => t("Red"), dot: "bg-red-500" },
  { id: "orange", label: () => t("Orange"), dot: "bg-orange-500" },
  { id: "yellow", label: () => t("Yellow"), dot: "bg-yellow-400" },
  { id: "green", label: () => t("Green"), dot: "bg-green-500" },
  { id: "blue", label: () => t("Blue"), dot: "bg-blue-500" },
  { id: "purple", label: () => t("Purple"), dot: "bg-purple-500" },
  { id: "gray", label: () => t("Gray"), dot: "bg-gray-400" },
];

export const colorOf = (c: TagColor) => TAG_COLORS.find((x) => x.id === c) ?? TAG_COLORS[TAG_COLORS.length - 1];

/** Longest name the server takes */
export const MAX_TAG_NAME = 64;

/** Tags in the order they are listed: by name, as the server sorts them */
export const byName = (a: Tag, b: Tag) => nameCollator.compare(a.name, b.name) || a.id - b.id;

/** The person's tags, by name, and each by its id */
export function useTags(enabled = true) {
  const q = useQuery({ ...queries.tags, enabled });
  return useMemo(() => {
    const tags = q.data ?? [];
    return { tags, byId: new Map(tags.map((tag) => [tag.id, tag])), loading: q.isLoading, error: q.error };
  }, [q.data, q.isLoading, q.error]);
}

/** The tags on an item that are known, in the order of the list (an id of a tag just deleted is left out) */
export function tagsOn(ids: readonly number[] | undefined, byId: ReadonlyMap<number, Tag>): Tag[] {
  return (ids ?? []).flatMap((id) => byId.get(id) ?? []).sort(byName);
}

/** "Urgent, Later": the names of tags, for a label or a tooltip */
export const tagNames = (tags: readonly Tag[]) => tags.map((tag) => tag.name).join(", ");

/**
 * Whether a selection has a tag: every item (true), some (mixed), or none. With a span of a large folder, items that
 * aren't loaded aren't known: a tag on some of the loaded items, or on none, shows as not all of them.
 */
export function tagState(nodes: readonly Pick<Node, "tags">[], tag: number, partly: boolean): boolean | "mixed" {
  const on = nodes.filter((n) => n.tags?.includes(tag)).length;
  if (on === 0) return false;
  return on === nodes.length && !partly ? true : "mixed";
}

/** An item's tags once `add` are put on and `remove` taken off */
export function withTags(ids: readonly number[] | undefined, add: readonly number[], remove: readonly number[]): number[] {
  const out = (ids ?? []).filter((id) => !remove.includes(id));
  for (const id of add) if (!out.includes(id) && !remove.includes(id)) out.push(id);
  return out;
}

/**
 * Puts tags on the selected items, or takes them off, a batch at a time for a large selection. The loaded items show the
 * change at once; the lists of tagged items load again. Resolves with whether it worked (the error is shown).
 */
export async function changeTags(qc: QueryClient, picked: Picked, nodes: readonly Node[], add: number[], remove: number[]): Promise<boolean> {
  try {
    await eachBatch(picked, t("Changing tags…"), (ids) => api.applyTags(ids, add, remove));
    void refreshFiles(qc, { updated: nodes.map((n) => ({ id: n.id, tags: withTags(n.tags, add, remove) })), tags: true, folders: [picked.span?.folder] });
    return true;
  } catch (e) {
    toast.error(errorMessage(e, t("Couldn't change the tags")));
    reportShown("tags", e);
    // Some batches may be done: what shows loads again
    void refreshFiles(qc, { folders: [picked.span?.folder, ...nodes.map((n) => n.parent_id)], tags: true, nodes: nodes.map((n) => n.id) });
    return false;
  }
}

/** Keeps the list of tags in order after a change the server answered with */
function keep(qc: QueryClient, change: (tags: Tag[]) => Tag[]) {
  qc.setQueryData<Tag[]>(keys.tags(), (tags) => (tags ? change(tags).sort(byName) : tags));
}

/** Makes a new tag (the server says why a name can't be used) */
export async function createTag(qc: QueryClient, name: string, color: TagColor): Promise<Tag> {
  const tag = await api.createTag(name, color);
  keep(qc, (tags) => [...tags.filter((x) => x.id !== tag.id), tag]);
  return tag;
}

/** Renames or recolours a tag */
export async function updateTag(qc: QueryClient, id: number, change: { name?: string; color?: TagColor }): Promise<Tag> {
  const tag = await api.updateTag(id, change);
  keep(qc, (tags) => tags.map((x) => (x.id === tag.id ? tag : x)));
  return tag;
}

/** The dialog that makes a tag, or changes `tag`, asked for with editTag() and shown by <TagDialogHost /> */
export interface TagDialogRequest {
  tag?: Tag;
  resolve(tag: Tag | null): void;
}

export const tagDialog = createStore<TagDialogRequest | null>(null);

/** Asks for a new tag (or a new name and colour for `tag`): the tag made or changed, null when cancelled */
export function editTag(tag?: Tag): Promise<Tag | null> {
  tagDialog.get()?.resolve(null);
  return new Promise((resolve) => tagDialog.set({ tag, resolve }));
}

/** Deletes a tag: it comes off every item */
export async function deleteTag(qc: QueryClient, id: number): Promise<void> {
  await api.deleteTag(id);
  keep(qc, (tags) => tags.filter((x) => x.id !== id));
  qc.removeQueries({ queryKey: ["tagged", id] });
}
