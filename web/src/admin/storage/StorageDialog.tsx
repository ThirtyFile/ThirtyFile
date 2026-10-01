//! Adding a storage location, or changing one: its kind, connection and folder

import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { CheckCircle2Icon, Loader2Icon, PlugZapIcon, ShieldAlertIcon, XCircleIcon } from "lucide-react";
import { toast } from "sonner";
import { api, type StorageConfig, type StorageKind, type StorageLocation } from "@/api";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { ErrorText } from "@/components/dialogs";
import { cn } from "@/lib/utils";
import { t } from "@/lib/i18n";
import { NativeSelect } from "@/components/ui/native-select";

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
    hint: t('Internal http endpoints require "Allow http". MinIO is no longer freely distributed; RustFS (Apache 2.0) is recommended for new setups.'),
  },
  {
    key: "other",
    label: t("Other S3-compatible service"),
    config: { endpoint: "", region: "", path_style: true, allow_http: false },
    hint: t("For example Backblaze B2, Wasabi, or Ceph."),
  },
];

export function StorageDialog({ location, onClose, onSaved }: { location: StorageLocation | null; onClose(): void; onSaved(): void }) {
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
            <DialogTitle>{location ? t('Edit "{name}"', { name: location.name }) : t("Add storage location")}</DialogTitle>
            <DialogDescription>{t("A connection test (writing, reading back, and deleting a small file) runs before saving.")}</DialogDescription>
          </DialogHeader>
          {/* Room around the fields inside the scrolling area, so their focus rings aren't cut off at its edges */}
          <div className="-m-1 grid max-h-[60vh] gap-3 overflow-y-auto p-1">
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
                    <NativeSelect
                      id="st-kind"
                      className="w-full"
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
                    </NativeSelect>
                  </div>
                )}
                {kind === "local" ? (
                  <>
                    {field("path", t("Folder path (absolute path on the server; can be a NAS mount point)"), { placeholder: t("/mnt/nas/thirtyfile or D:\\thirtyfile") })}
                    <p className="text-xs text-muted-foreground">
                      {t(
                        "Saving creates the folder with a .thirtyfile-location file in it. Later, the location is used only while that file is there, so a disk or share that isn't mounted is never written to.",
                      )}
                    </p>
                  </>
                ) : kind === "sftp" || kind === "ftp" ? (
                  <RemoteFields kind={kind} cfg={cfg} set={set} hasSecret={!!location?.has_secret} auth={auth} setAuth={setAuth} seenKey={seenKey} />
                ) : (
                  <>
                    {!location && (
                      <div className="grid gap-1.5">
                        <Label htmlFor="st-preset">{t("Service")}</Label>
                        <NativeSelect
                          id="st-preset"
                          className="w-full"
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
                        </NativeSelect>
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
                  <p className={cn("flex items-center gap-1.5 text-sm", tested === "ok" ? "text-emerald-600 dark:text-emerald-400" : "text-destructive")}>
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
          <NativeSelect id="st-auth" className="w-full" value={auth} onChange={(e) => setAuth(e.target.value as "password" | "key")}>
            <option value="password">{t("Password")}</option>
            <option value="key">{t("Private key (recommended)")}</option>
          </NativeSelect>
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
                title={t("Use after the server is reinstalled or its key changes: enter the password or key again, and the current key is recorded when you save")}
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
