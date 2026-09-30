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
    <Button variant="ghost" title={title ?? label} aria-label={label} className={cn("h-[30px] gap-[5px] px-2 text-xs", className)} {...props}>
      <Icon />
      {showLabel && <span className={phoneLabel ? "md:max-lg:hidden" : "max-lg:hidden"}>{label}</span>}
    </Button>
  );
}

export function ToolSeparator({ className }: { className?: string }) {
  return <span className={cn("mx-1 h-5 border-l", className)} />;
}
