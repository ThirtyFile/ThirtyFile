/** Choosing the storage location of a new space: a team space, or someone's personal space ("My files") */
import { useQuery } from "@tanstack/react-query";
import { api, type StorageKind, type StorageLocation } from "@/api";
import { t } from "@/lib/i18n";
import { cn } from "@/lib/utils";

export const useStorageLocations = (enabled = true) => useQuery({ queryKey: ["storage-locations"], queryFn: api.storageLocations, enabled });

/** The kinds as the storage settings name them (not imported from there: that page is loaded only when opened) */
const kindLabel = (k: StorageKind) => ({ s3: t("S3-compatible"), sftp: "SFTP", ftp: t("FTP/FTPS"), local: t("Local folder") })[k];

/** How a location is named in the lists: its name and kind, and whether it can be reached right now */
export function locationLabel(l: StorageLocation) {
  const kind = kindLabel(l.kind);
  return l.connected ? t("{name} ({kind})", { name: l.name, kind }) : t("{name} ({kind}) — can't connect", { name: l.name, kind });
}

/** The name of a location by its id (the id itself when it is gone) */
export function useLocationName() {
  const locations = useStorageLocations();
  return (id: string | null | undefined) => (id ? (locations.data?.find((l) => l.id === id)?.name ?? id) : "");
}

/**
 * A list of the storage locations. With `blank`, its first entry ("") stands for a choice made later: `"default"`
 * for whatever the default location is when the space is created (the current default is named), or the given text.
 */
export function LocationSelect({
  id,
  value,
  onChange,
  blank,
  disabled,
  className,
  "aria-label": ariaLabel,
}: {
  id?: string;
  value: string;
  onChange(value: string): void;
  blank?: "default" | (string & {});
  disabled?: boolean;
  className?: string;
  "aria-label"?: string;
}) {
  const locations = useStorageLocations();
  const current = locations.data?.find((l) => l.is_default);
  const blankLabel = blank === "default" ? (current ? t("Default location ({name})", { name: current.name }) : t("Default location")) : blank;
  return (
    <select
      id={id}
      aria-label={ariaLabel}
      className={cn("h-9 min-w-0 rounded-md border bg-background px-2 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50", className)}
      value={value}
      disabled={disabled || !locations.data}
      onChange={(e) => onChange(e.target.value)}
    >
      {blankLabel && <option value="">{blankLabel}</option>}
      {/* A saved choice whose location is gone still shows, so the list doesn't silently change it */}
      {value && locations.data && !locations.data.some((l) => l.id === value) && <option value={value}>{value}</option>}
      {locations.data?.map((l) => (
        <option key={l.id} value={l.id}>
          {locationLabel(l)}
        </option>
      ))}
    </select>
  );
}
