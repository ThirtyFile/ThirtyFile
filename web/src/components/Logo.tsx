import { SiteName } from "@/components/SiteName";
import { logoUrl, useBranding } from "@/lib/branding";
import { cn } from "@/lib/utils";

/** Site logo and name (from branding settings; switches automatically when there's a dark-mode logo) */
export function Logo({ className, imgClassName = "h-7" }: { className?: string; imgClassName?: string }) {
  const b = useBranding();
  const img = cn("w-auto max-w-48 object-contain", imgClassName);
  return (
    <div className={cn("flex items-center gap-2 font-semibold", className)}>
      {!b.has_logo ? (
        <img src="/favicon.svg" alt="" className={cn(img, "aspect-square")} />
      ) : b.has_logo_dark ? (
        <>
          <img src={logoUrl(b)} alt={b.show_name ? "" : b.site_name} className={cn(img, "dark:hidden")} />
          <img src={logoUrl(b, true)} alt={b.show_name ? "" : b.site_name} className={cn(img, "hidden dark:block")} />
        </>
      ) : (
        <img src={logoUrl(b)} alt={b.show_name ? "" : b.site_name} className={img} />
      )}
      {(b.show_name || !b.has_logo) && <SiteName name={b.site_name} />}
    </div>
  );
}
