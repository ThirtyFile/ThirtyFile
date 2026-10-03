import type { LucideIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

// ───────────── Toolbar buttons ─────────────

export function ToolButton({
  icon: Icon,
  label,
  title,
  showLabel = false,
  phoneLabel = false,
  className,
  children,
  ...props
}: React.ComponentProps<typeof Button> & {
  icon: LucideIcon;
  label: string;
  title?: string;
  /** The label shows on wide screens (from 1024 pixels) */
  showLabel?: boolean;
  /** ...and on phones, where a toolbar has fewer buttons */
  phoneLabel?: boolean;
}) {
  return (
    <Button
      variant="ghost"
      title={title ?? label}
      aria-label={label}
      // Its size, corners, colours and hover are the style's (--tf-tool-*, style.css)
      className={cn(
        "h-(--tf-tool-h) gap-[5px] rounded-(--tf-tool-radius) px-2 text-(length:--tf-tool-text) leading-4 text-(--tf-tool-fg) hover:bg-(--tf-tool-hover) dark:hover:bg-(--tf-tool-hover) [&_svg:not([class*='size-'])]:size-(--tf-tool-icon)",
        className,
      )}
      {...props}
    >
      <Icon />
      {showLabel && <span className={phoneLabel ? "md:max-lg:hidden" : "max-lg:hidden"}>{label}</span>}
      {children}
    </Button>
  );
}

export function ToolSeparator({ className }: { className?: string }) {
  return <span className={cn("mx-1 h-5 border-l", className)} />;
}
