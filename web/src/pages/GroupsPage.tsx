import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Loader2Icon, PencilIcon, RefreshCwIcon, Trash2Icon, UsersRoundIcon } from "lucide-react";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { DataTable, EmptyState, type Column } from "@/components/DataTable";
import { toast } from "sonner";
import { api, type Group } from "@/api";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ConfirmDialog, ErrorText } from "@/components/dialogs";
import { Frame, ToolButton, ToolSeparator } from "@/components/Frame";
import { useSettingsSearch } from "@/lib/controlPanel";
import { t, tc } from "@/lib/i18n";
import { formatDate } from "@/lib/utils";

/** Group management (admins): groups can be space members or targets of folder sharing */
export function GroupsPage() {
  const q = useQuery({ queryKey: ["groups"], queryFn: api.groups });
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [editing, setEditing] = useState<Group | "new" | null>(null);
  const [deleting, setDeleting] = useState<Group | null>(null);
  const qc = useQueryClient();
  const groups = q.data ?? [];
  const selected = groups.find((g) => g.id === selectedId) ?? null;

  const toolbar = (
    <>
      <ToolButton icon={UsersRoundIcon} label={t("New group")} showLabel className="h-9 px-2.5 text-[13px]" onClick={() => setEditing("new")} />
      <ToolSeparator />
      <ToolButton
        icon={PencilIcon}
        label={t("Edit")}
        showLabel
        className="h-9 px-2.5 text-[13px]"
        disabled={!selected}
        onClick={() => selected && setEditing(selected)}
      />
      <ToolButton
        icon={Trash2Icon}
        label={t("Delete")}
        showLabel
        className="h-9 px-2.5 text-[13px]"
        disabled={!selected}
        onClick={() => selected && setDeleting(selected)}
      />
    </>
  );

  const searchSettings = useSettingsSearch();
  const columns: Column<Group>[] = [
    {
      header: t("Name"),
      className: "w-[200px]",
      cell: (g) => (
        <span className="flex items-center gap-2">
          <UsersRoundIcon className="size-4 shrink-0 text-muted-foreground" /> {g.name}
        </span>
      ),
    },
    {
      header: t("Members"),
      cellClassName: "text-muted-foreground",
      title: (g) => g.members.map((m) => m.username).join(t(", ")),
      cell: (g) =>
        g.members.length > 0
          ? t("{n} member: {names}|{n} members: {names}", { n: g.members.length, names: g.members.map((m) => m.username).join(t(", ")) })
          : tc("short", "{n} member|{n} members", { n: 0 }),
    },
    { header: t("Description"), className: "w-[200px] max-md:hidden", cellClassName: "text-muted-foreground", cell: (g) => g.description },
    { header: t("Date created"), className: "w-[110px]", cellClassName: "text-muted-foreground", cell: (g) => formatDate(g.created_at) },
  ];

  return (
    <Frame
      toolbar={toolbar}
      icon={UsersRoundIcon}
      crumbs={[{ label: t("Control panel"), to: "/admin" }, { label: t("Groups") }]}
      upTo="/admin"
      searchPlaceholder={t("Search settings")}
      onSearch={searchSettings}
      footer={<span>{t("{n} group|{n} groups", { n: groups.length })}</span>}
    >
      <DataTable
        rows={groups}
        rowKey={(g) => String(g.id)}
        columns={columns}
        fixed
        loading={q.isLoading}
        selectedKey={selectedId === null ? null : String(selectedId)}
        onSelect={(k) => setSelectedId(k ? Number(k) : null)}
        onOpen={(g) => setEditing(g)}
        empty={<EmptyState icon={UsersRoundIcon} title={t("No groups yet")} hint={t("Create a group to add a whole team to a space or shared folder at once.")} />}
        menu={() =>
          selected ? (
            <>
              <DropdownMenuItem onClick={() => setEditing(selected)}>
                <PencilIcon /> {t("Edit group and members")}
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem variant="destructive" onClick={() => setDeleting(selected)}>
                <Trash2Icon /> {t("Delete group")}
              </DropdownMenuItem>
            </>
          ) : (
            <>
              <DropdownMenuItem onClick={() => setEditing("new")}>
                <UsersRoundIcon /> {t("New group")}
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => qc.invalidateQueries({ queryKey: ["groups"] })}>
                <RefreshCwIcon /> {t("Refresh")}
              </DropdownMenuItem>
            </>
          )
        }
      />
      {editing && <GroupDialog group={editing === "new" ? null : editing} onClose={() => setEditing(null)} />}
      {deleting && (
        <ConfirmDialog
          title={t("Delete group \"{name}\"?", { name: deleting.name })}
          description={t("Group members will lose access to spaces and folders granted through this group.")}
          confirmText={t("Delete group")}
          destructive
          onClose={() => setDeleting(null)}
          onConfirm={async () => {
            await api.deleteGroup(deleting.id);
            toast.success(t("Group deleted"));
            setDeleting(null);
            setSelectedId(null);
            qc.invalidateQueries({ queryKey: ["groups"] });
          }}
        />
      )}
    </Frame>
  );
}

function GroupDialog({ group, onClose }: { group: Group | null; onClose(): void }) {
  const qc = useQueryClient();
  const users = useQuery({ queryKey: ["admin-users"], queryFn: api.users });
  const [name, setName] = useState(group?.name ?? "");
  const [description, setDescription] = useState(group?.description ?? "");
  const [members, setMembers] = useState<Set<number>>(new Set(group?.members.map((m) => m.id)));
  const [filter, setFilter] = useState("");

  const save = useMutation({
    mutationFn: () => {
      const req = { name: name.trim(), description, members: [...members] };
      return group ? api.updateGroup(group.id, req) : api.createGroup(req);
    },
    onSuccess: () => {
      toast.success(group ? t("Group updated") : t("Group created"));
      qc.invalidateQueries({ queryKey: ["groups"] });
      onClose();
    },
  });

  const shown = (users.data ?? []).filter((u) => u.username.toLowerCase().includes(filter.toLowerCase()));

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-md">
        <form
          className="grid gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            save.mutate();
          }}
        >
          <DialogHeader>
            <DialogTitle>{group ? t("Edit group \"{name}\"", { name: group.name }) : t("New group")}</DialogTitle>
            <DialogDescription>{t("Groups can be added as space members or shared folder recipients.")}</DialogDescription>
          </DialogHeader>
          <div className="grid gap-2">
            <Label htmlFor="g-name">{t("Name")}</Label>
            <Input id="g-name" value={name} onChange={(e) => setName(e.target.value)} placeholder={t("ESG team")} autoFocus />
            <Label htmlFor="g-desc">{t("Description (optional)")}</Label>
            <Input id="g-desc" value={description} onChange={(e) => setDescription(e.target.value)} />
            <Label>{t("Members ({n} selected)", { n: members.size })}</Label>
            <Input value={filter} onChange={(e) => setFilter(e.target.value)} placeholder={t("Filter accounts")} className="h-8 text-sm" />
            <div className="max-h-52 overflow-y-auto rounded-md border">
              {users.isLoading && <Loader2Icon className="m-3 size-4 animate-spin" />}
              {shown.map((u) => (
                <label
                  key={u.id}
                  className="flex cursor-pointer items-center gap-2 border-b border-border/50 px-3 py-1.5 text-sm last:border-0 hover:bg-muted/60"
                >
                  <input
                    type="checkbox"
                    className="accent-brand"
                    checked={members.has(u.id)}
                    onChange={(e) => {
                      const next = new Set(members);
                      if (e.target.checked) next.add(u.id);
                      else next.delete(u.id);
                      setMembers(next);
                    }}
                  />
                  {u.username}
                  {u.disabled && <span className="text-xs text-muted-foreground">{t("(disabled)")}</span>}
                </label>
              ))}
            </div>
            <ErrorText>{save.error?.message}</ErrorText>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={save.isPending || !name.trim()}>
              {save.isPending && <Loader2Icon className="animate-spin" />}
              {group ? t("Save") : t("Create")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
