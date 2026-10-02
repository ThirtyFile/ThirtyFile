//! Giving someone a personal space later, removing it, and deleting an account: what happens to the files in it

import { useState } from "react";
import { type QueryClient, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Loader2Icon } from "lucide-react";
import { toast } from "sonner";
import { api, type Drive, type UserRow } from "@/api";
import { affected, invalidate, keys, queries } from "@/api/queryKeys";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { ErrorText } from "@/components/dialogs";
import { LocationSelect, useDefaultLocationId, useLocationName } from "@/components/LocationSelect";
import { followJob } from "@/lib/jobs";
import { useMe } from "@/lib/session";
import { t } from "@/lib/i18n";
import { formatBytes } from "@/lib/utils";
import { NativeSelect } from "@/components/ui/native-select";

/**
 * What happens to the files in someone's "My files" when it goes (the user is deleted, or only their space): moved
 * into a folder in another space (the default), or deleted. Only its size is shown, never what is in it.
 */
function usePersonalFiles(user: UserRow) {
  const me = useMe();
  const drives = useQuery(queries.adminDrives);
  // Their own space ("My files": a folder on the server in new installs, whose folder is kept when it is removed)
  const own = drives.data?.find((d) => d.kind === "personal" && d.owner_name === user.username);
  // Spaces that can take the files: not the user's own, not turned off
  const targets = (drives.data ?? []).filter((d) => !d.disabled && d !== own);
  const mine = targets.find((d) => d.kind === "personal" && d.owner_name === me.username);
  const [choice, setChoice] = useState<"move" | "delete">("move");
  const [target, setTarget] = useState("");
  const moveTo = target || mine?.id || targets[0]?.id || "";
  return {
    user,
    own,
    targets,
    choice,
    setChoice,
    moveTo,
    setTarget,
    /** The query for the server */
    files: choice === "move" ? { move_to: moveTo } : { delete_files: true },
    /** A choice is complete (a space to move to was found) */
    ready: !user.personal_space || choice === "delete" || !!moveTo,
  };
}

function PersonalFilesChoice({ c }: { c: ReturnType<typeof usePersonalFiles> }) {
  const spaceLabel = (d: Drive) => (d.kind === "personal" ? t("My files of {name}", { name: d.owner_name }) : d.name);
  const { own, user } = c;
  return (
    <div className="grid gap-3" role="radiogroup" aria-label={t("Their files")}>
      <Label className="flex items-start gap-2 font-normal">
        <input type="radio" name="personal-files" className="mt-1 accent-brand" checked={c.choice === "move"} onChange={() => c.setChoice("move")} />
        <span className="grid min-w-0 flex-1 gap-1.5">
          <span>{t("Move their files to:")}</span>
          <NativeSelect aria-label={t("Move their files to:")} value={c.moveTo} disabled={c.choice !== "move"} onChange={(e) => c.setTarget(e.target.value)}>
            {c.targets.map((d) => (
              <option key={d.id} value={d.id}>
                {spaceLabel(d)}
              </option>
            ))}
          </NativeSelect>
          <span className="text-xs text-muted-foreground">
            {own?.mode === "folder"
              ? t('They go into a new folder named "{name}" at the top of that space, and count toward its size. Their trash stays in their folder on the server.', {
                  name: user.files_folder,
                })
              : t('They go into a new folder named "{name}" at the top of that space, and count toward its size. Their trash is emptied.', { name: user.files_folder })}
          </span>
        </span>
      </Label>
      <Label className="flex items-start gap-2 font-normal">
        <input type="radio" name="personal-files" className="mt-1 accent-brand" checked={c.choice === "delete"} onChange={() => c.setChoice("delete")} />
        {own?.mode === "folder" ? (
          <span className="min-w-0">
            {t("Remove their files from ThirtyFile")}
            <span className="block text-xs break-words text-muted-foreground">
              {own.source_path
                ? t("Their folder on the server, {path}, is kept with the files in it: delete it there when it's no longer needed.", { path: own.source_path })
                : // Administrators aren't told where someone else's personal space is kept
                  t("Their folder on the server is kept with the files in it: delete it there when it's no longer needed.")}
            </span>
          </span>
        ) : (
          <span>
            {t("Delete their files permanently")}
            <span className="block text-xs text-muted-foreground">{t("This can't be undone.")}</span>
          </span>
        )}
      </Label>
    </div>
  );
}

/**
 * After someone's "My files" is created or removed: the lists, and the administrator's own session and spaces (the
 * navigation pane), in case it was theirs
 */
function invalidatePersonal(qc: QueryClient) {
  void invalidate(qc, ...affected.personalSpace());
}

/** Creating someone's "My files" later, on a storage location */
export function AddPersonalDialog({ user, onClose }: { user: UserRow; onClose(): void }) {
  const qc = useQueryClient();
  const system = useQuery(queries.system);
  const locationName = useLocationName();
  // Preset from the system setting (Control panel › General); "" = the default location
  const [location, setLocation] = useState<string | null>(null);
  const value = location ?? system.data?.personal_location ?? "";
  // "Default location" is sent as the location it names: left out, the system setting would apply instead
  const defaultLocation = useDefaultLocationId();
  const add = useMutation({
    mutationFn: () => api.addPersonalSpace(user.id, value || defaultLocation),
    onSuccess: () => {
      toast.success(t('"My files" created'));
      invalidatePersonal(qc);
      onClose();
    },
  });
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-md">
        <form
          className="grid gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            add.mutate();
          }}
        >
          <DialogHeader>
            <DialogTitle>{t('Create "My files" for "{name}"', { name: user.username })}</DialogTitle>
            <DialogDescription>{t("A private space that only they can see.")}</DialogDescription>
          </DialogHeader>
          <div className="grid gap-1.5">
            <Label htmlFor="personal-add-location">{t("Storage location")}</Label>
            <LocationSelect id="personal-add-location" value={value} onChange={setLocation} blank="default" disabled={!system.data} />
            {user.personal_pending && (
              <p className="text-xs text-muted-foreground">
                {t("It's waiting for {location} to be available. Creating it now replaces the wait.", { location: locationName(user.personal_pending) })}
              </p>
            )}
          </div>
          <ErrorText>{add.error?.message}</ErrorText>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={add.isPending || !system.data}>
              {add.isPending && <Loader2Icon className="animate-spin" />}
              {t("Create")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** Removing someone's "My files" (or stopping the wait for one): they keep their account */
export function RemovePersonalDialog({ user, onClose }: { user: UserRow; onClose(): void }) {
  const qc = useQueryClient();
  const c = usePersonalFiles(user);
  const locationName = useLocationName();
  const remove = useMutation({
    mutationFn: () => api.removePersonalSpace(user.id, user.personal_space ? c.files : {}),
    // Moving the files to or from a folder on the server can take a while: the dialog closes, and a message follows it
    onSuccess: (job) => {
      onClose();
      void followJob(job, user.personal_space ? t('"My files" removed') : t('Stopped waiting to create "My files"'), () => invalidatePersonal(qc));
    },
  });
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <form
          className="grid gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            remove.mutate();
          }}
        >
          <DialogHeader>
            <DialogTitle>{t('Remove "My files" of "{name}"?', { name: user.username })}</DialogTitle>
            <DialogDescription>
              {user.personal_space
                ? t("Their personal space ({size}) is removed. They keep their account and their access to other spaces, and start in the first space they can use.", {
                    size: formatBytes(user.used_bytes),
                  })
                : t('Their "My files" is still waiting for {location} to be available. It won\'t be created.', { location: locationName(user.personal_pending) })}
            </DialogDescription>
          </DialogHeader>
          {user.personal_space && <PersonalFilesChoice c={c} />}
          <ErrorText>{remove.error?.message}</ErrorText>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" variant="destructive" disabled={remove.isPending || !c.ready}>
              {remove.isPending && <Loader2Icon className="animate-spin" />}
              {t('Remove "My files"')}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** Deleting a user: their personal space is moved into a folder in another space (the default) or deleted */
export function DeleteUserDialog({ user, onClose, onDeleted }: { user: UserRow; onClose(): void; onDeleted(): void }) {
  const qc = useQueryClient();
  const c = usePersonalFiles(user);

  const remove = useMutation({
    mutationFn: () => api.deleteUser(user.id, user.personal_space ? c.files : {}),
    // Moving the files to or from a folder on the server can take a while: the dialog closes, and a message follows it
    onSuccess: (job) => {
      onDeleted();
      void followJob(job, t("User deleted"), () => {
        void invalidate(qc, ...affected.accountDeleted());
      });
    },
  });
  const disable = useMutation({
    mutationFn: () => api.updateUser(user.id, { disabled: true }),
    onSuccess: () => {
      toast.success(t("Account disabled"));
      qc.invalidateQueries({ queryKey: keys.adminUsers() });
      onClose();
    },
  });

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <form
          className="grid gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            remove.mutate();
          }}
        >
          <DialogHeader>
            <DialogTitle>{t('Delete user "{name}"?', { name: user.username })}</DialogTitle>
          </DialogHeader>
          <div className="grid gap-2 text-sm text-muted-foreground">
            <p>
              {user.personal_space
                ? t(
                    'Their personal space "My files" ({size}) is removed. Files they added to other spaces, and team spaces they own, are transferred to you. Their share links are deleted.',
                    {
                      size: formatBytes(user.used_bytes),
                    },
                  )
                : t("Files they added to spaces, and team spaces they own, are transferred to you. Their share links are deleted.")}
            </p>
            {!user.disabled && (
              <p>
                {t("To stop them signing in and keep everything as it is, disable the account instead.")}{" "}
                <Button type="button" variant="link" className="h-auto p-0" disabled={disable.isPending} onClick={() => disable.mutate()}>
                  {t("Disable account")}
                </Button>
              </p>
            )}
          </div>
          {user.personal_space && <PersonalFilesChoice c={c} />}
          <ErrorText>{remove.error?.message ?? disable.error?.message}</ErrorText>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" variant="destructive" disabled={remove.isPending || !c.ready}>
              {remove.isPending && <Loader2Icon className="animate-spin" />}
              {t("Delete user")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
