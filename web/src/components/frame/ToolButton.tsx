import type { LucideIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

// ───────────── Toolbar buttons ─────────────

export function ToolButton({
  icon: Icon,
  label,
  title,
  showLabel = false,
  className,
  ...props
}: React.ComponentProps<typeof Button> & {
  icon: LucideIcon;
  label: string;
  title?: string;
  showLabel?: boolean;
}) {
  return (
    <Button variant="ghost" title={title ?? label} aria-label={label} className={cn("h-[30px] gap-[5px] px-2 text-xs", className)} {...props}>
      <Icon />
      {showLabel && <span className="max-lg:hidden">{label}</span>}
    </Button>
  );
}

export function ToolSeparator() {
  return <span className="mx-1 h-5 border-l" />;
}
