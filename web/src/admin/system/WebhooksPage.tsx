import { useId, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Loader2Icon, PlusIcon } from "lucide-react";
import { toast } from "sonner";
import { api, type FileWebhook, type FileWebhookChange, type FileWebhooksInfo } from "@/api";
import { keys } from "@/api/queryKeys";
import { Section, SettingsFrame } from "@/admin/SettingsFrame";
import { ErrorState } from "@/components/ErrorState";
import { ErrorText } from "@/components/dialogs";
import { actionLabel } from "@/components/logs/actions";
import { SecretInput } from "@/components/SecretField";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { confirm } from "@/lib/confirm";
import { t, tServer } from "@/lib/i18n";
import { useSubmit } from "@/lib/useSubmit";
import { errorMessage, formatDateTime, formatList } from "@/lib/utils";

/** Control panel › File webhooks: the other systems told when files change */
export function WebhooksPage() {
  const q = useQuery({ queryKey: keys.fileWebhooks(), queryFn: ({ signal }) => api.fileWebhooks(signal), refetchInterval: 15_000 });
  return (
    <SettingsFrame item="webhooks" onRefresh={() => q.refetch()}>
      {q.error ? <ErrorState message={q.error.message} onRetry={() => q.refetch()} /> : !q.data ? <p role="status">{t("Loading…")}</p> : <Webhooks info={q.data} />}
    </SettingsFrame>
  );
}

const changeOf = (w: FileWebhook): FileWebhookChange => ({ url: w.url, events: w.events, drives: w.drives, allow_private: w.allow_private, enabled: w.enabled });

function Webhooks({ info }: { info: FileWebhooksInfo }) {
  const qc = useQueryClient();
  /** The webhook being changed, or "new" */
  const [editing, setEditing] = useState<FileWebhook | "new" | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const saved = (next: FileWebhooksInfo) => qc.setQueryData(keys.fileWebhooks(), next);
  /** Runs one button's action, saying why when it fails */
  const act = async (key: string, action: () => Promise<void>) => {
    setBusy(key);
    try {
      await action();
    } catch (e) {
      toast.error(errorMessage(e, t("Operation failed")));
    } finally {
      setBusy(null);
    }
  };
  const remove = async (w: FileWebhook) => {
    const ok = await confirm({
      title: t("Remove this webhook?"),
      description: t("Messages waiting to be sent to it are deleted too."),
      confirmText: t("Remove"),
      destructive: true,
    });
    if (ok) await act(`remove:${w.id}`, async () => void saved(await api.deleteFileWebhook(w.id)));
  };
  const spaces = (w: FileWebhook) =>
    w.drives.length ? formatList(w.drives.map((id) => info.spaces.find((s) => s.id === id)?.name ?? t("A deleted space"))) : t("Every space but personal ones");

  return (
    <>
      <div className="grid gap-3 text-sm">
        <p className="text-muted-foreground">
          {t(
            "A webhook is told when files are uploaded, changed, moved, shared or deleted. Personal spaces and restricted folders are left out. Messages hold names and ids, never file contents, share link addresses or passwords.",
          )}
        </p>
        <div>
          <Button onClick={() => setEditing("new")}>
            <PlusIcon /> {t("Add webhook")}
          </Button>
        </div>
      </div>
      {info.webhooks.map((w) => (
        <Section key={w.id} title={w.enabled ? t("Webhook") : t("Webhook (paused)")}>
          <div className="grid gap-3 p-4 text-sm">
            <p className="font-medium break-all">{w.url}</p>
            <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
              <dt className="text-muted-foreground">{t("Events")}</dt>
              <dd>{formatList(w.events.map(actionLabel))}</dd>
              <dt className="text-muted-foreground">{t("Spaces")}</dt>
              <dd>{spaces(w)}</dd>
              <dt className="text-muted-foreground">{t("Waiting to be sent")}</dt>
              <dd>{w.pending}</dd>
            </dl>
            <div className="flex flex-wrap gap-2">
              <Button variant="outline" size="sm" onClick={() => setEditing(w)}>
                {t("Edit")}
              </Button>
              <Button
                variant="outline"
                size="sm"
                disabled={busy !== null}
                onClick={() =>
                  void act(`test:${w.id}`, async () => {
                    await api.testFileWebhook(w.id);
                    toast.success(t("A test message was sent to the webhook"));
                  })
                }
              >
                {busy === `test:${w.id}` && <Loader2Icon className="animate-spin" />}
                {t("Send a test to the webhook")}
              </Button>
              <Button
                variant="outline"
                size="sm"
                disabled={busy !== null}
                onClick={() => void act(`pause:${w.id}`, async () => void saved(await api.updateFileWebhook(w.id, { ...changeOf(w), enabled: !w.enabled })))}
              >
                {w.enabled ? t("Pause") : t("Resume")}
              </Button>
              <Button variant="outline" size="sm" className="text-destructive" disabled={busy !== null} onClick={() => void remove(w)}>
                {t("Remove")}
              </Button>
            </div>
          </div>
          {w.recent.length > 0 && (
            <details className="border-t text-sm">
              <summary className="cursor-pointer px-4 py-2 text-xs text-muted-foreground">{t("Latest messages")}</summary>
              <ul className="divide-y border-t">
                {w.recent.map((m) => (
                  <li key={m.id} className="grid gap-0.5 px-4 py-2">
                    <span className="flex flex-wrap items-baseline gap-x-3">
                      <span className="font-medium">{actionLabel(m.event)}</span>
                      <span className="text-xs text-muted-foreground">{formatDateTime(m.created_at)}</span>
                    </span>
                    <span className={m.failed ? "text-xs text-destructive" : "text-xs text-muted-foreground"}>
                      {m.done_at
                        ? t("Sent")
                        : m.failed
                          ? t("Given up after {n} tries: {error}", { n: m.attempts, error: m.error ? tServer(m.error) : "—" })
                          : m.attempts > 0
                            ? t("Tried {n} times, to be tried again: {error}", { n: m.attempts, error: m.error ? tServer(m.error) : "—" })
                            : t("Waiting to be sent")}
                    </span>
                  </li>
                ))}
              </ul>
            </details>
          )}
        </Section>
      ))}
      {!info.webhooks.length && <p className="text-sm text-muted-foreground">{t("None yet")}</p>}
      {editing && <WebhookDialog info={info} webhook={editing === "new" ? null : editing} onClose={() => setEditing(null)} onSaved={saved} />}
    </>
  );
}

/** A new secret: 32 random bytes as hexadecimal digits */
const makeSecret = () => Array.from(crypto.getRandomValues(new Uint8Array(32)), (b) => b.toString(16).padStart(2, "0")).join("");

function WebhookDialog({ info, webhook, onClose, onSaved }: { info: FileWebhooksInfo; webhook: FileWebhook | null; onClose(): void; onSaved(info: FileWebhooksInfo): void }) {
  const [url, setUrl] = useState(webhook?.url ?? "");
  const [secret, setSecret] = useState("");
  const [events, setEvents] = useState<string[]>(webhook?.events ?? info.events);
  const [chosen, setChosen] = useState(!!webhook?.drives.length);
  // A space deleted since isn't kept
  const [drives, setDrives] = useState<string[]>(webhook?.drives.filter((id) => info.spaces.some((s) => s.id === id)) ?? []);
  const [allowPrivate, setAllowPrivate] = useState(webhook?.allow_private ?? false);
  const id = useId();
  const secretShort = !!secret && secret.length < 16;
  const ready = !!url.trim() && !secretShort && (!!webhook || !!secret) && events.length > 0 && (!chosen || drives.length > 0);
  const { busy, error, run } = useSubmit(async () => {
    if (!ready) return;
    const change = { url: url.trim(), secret: secret || undefined, events, drives: chosen ? drives : [], allow_private: allowPrivate, enabled: webhook?.enabled ?? true };
    onSaved(webhook ? await api.updateFileWebhook(webhook.id, change) : await api.createFileWebhook(change));
    onClose();
    toast.success(t("Saved"));
  }, t("Couldn't make the change"));
  const toggle = (list: string[], value: string, on: boolean) => (on ? [...list, value] : list.filter((x) => x !== value));

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <form onSubmit={run} className="grid min-w-0 gap-4">
          <DialogHeader>
            <DialogTitle>{webhook ? t("Edit webhook") : t("Add webhook")}</DialogTitle>
          </DialogHeader>
          <div className="grid gap-2">
            <Label htmlFor={`${id}-url`}>{t("Webhook address")}</Label>
            <Input id={`${id}-url`} type="url" placeholder="https://example.com/thirtyfile-files" value={url} onChange={(e) => setUrl(e.target.value)} />
          </div>
          <div className="grid gap-2">
            <Label htmlFor={`${id}-secret`}>{t("Shared secret")}</Label>
            <div className="flex gap-2">
              <SecretInput
                id={`${id}-secret`}
                revealLabel={t("Show shared secret")}
                autoComplete="new-password"
                placeholder={webhook ? t("Leave blank to keep the current secret") : undefined}
                value={secret}
                onChange={(e) => setSecret(e.target.value)}
              />
              <Button type="button" variant="outline" onClick={() => setSecret(makeSecret())}>
                {t("Generate")}
              </Button>
            </div>
            <p className={secretShort ? "text-xs text-destructive" : "text-xs text-muted-foreground"}>
              {secretShort
                ? t("The shared secret needs at least {n} characters", { n: 16 })
                : t("The receiver needs the same secret to check that messages come from here. Copy it before saving: it isn't shown again.")}
            </p>
          </div>
          <fieldset className="grid gap-2">
            <legend className="mb-1 text-sm font-medium">{t("Events")}</legend>
            <div className="grid gap-2 sm:grid-cols-2">
              {info.events.map((event) => (
                <Label key={event} className="flex items-center gap-2 font-normal">
                  <Checkbox checked={events.includes(event)} onCheckedChange={(on) => setEvents(toggle(events, event, on === true))} />
                  {actionLabel(event)}
                </Label>
              ))}
            </div>
          </fieldset>
          <fieldset className="grid gap-2">
            <legend className="mb-1 text-sm font-medium">{t("Spaces")}</legend>
            <Label className="flex items-center gap-2 font-normal">
              <input type="radio" className="size-4 accent-brand" checked={!chosen} onChange={() => setChosen(false)} />
              {t("Every space but personal ones")}
            </Label>
            <Label className="flex items-center gap-2 font-normal">
              <input type="radio" className="size-4 accent-brand" checked={chosen} onChange={() => setChosen(true)} />
              {t("Only the spaces chosen here")}
            </Label>
            {chosen && (
              <div className="grid max-h-40 gap-2 overflow-y-auto rounded-md border p-3">
                {info.spaces.map((s) => (
                  <Label key={s.id} className="flex items-center gap-2 font-normal">
                    <Checkbox checked={drives.includes(s.id)} onCheckedChange={(on) => setDrives(toggle(drives, s.id, on === true))} />
                    {s.name}
                  </Label>
                ))}
              </div>
            )}
          </fieldset>
          <Label className="flex items-center gap-2 font-normal">
            <Checkbox checked={allowPrivate} onCheckedChange={(on) => setAllowPrivate(on === true)} />
            {t("Allow a webhook on the private network or on this computer")}
          </Label>
          <ErrorText>{error}</ErrorText>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={busy || !ready}>
              {busy && <Loader2Icon className="animate-spin" />}
              {t("Save")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
