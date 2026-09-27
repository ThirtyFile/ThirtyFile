import { DEFAULT_BRANDING } from "@/lib/branding";
import { cn } from "@/lib/utils";

/** Site name; the default product name is drawn as the two-tone "Thirty·File" wordmark */
export function SiteName({
  name,
  className,
  accentClassName = "text-brand",
  accentColor,
}: {
  name: string;
  className?: string;
  accentClassName?: string;
  /** Explicit accent color (for previews whose colors don't come from the page theme) */
  accentColor?: string;
}) {
  if (name !== DEFAULT_BRANDING.site_name) return <span className={cn("truncate", className)}>{name}</span>;
  return (
    <span className={cn("truncate tracking-tight", className)}>
      Thirty<span className={accentColor ? undefined : accentClassName} style={accentColor ? { color: accentColor } : undefined}>File</span>
    </span>
  );
}
