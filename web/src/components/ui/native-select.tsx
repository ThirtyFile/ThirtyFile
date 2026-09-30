import type * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "cn";

/**
 * The browser's own drop-down list, looking like the text fields: the same border (`border-input`, at least 3:1
 * against the page), focus ring and dark background. Sizes: xs for toolbars and filters, sm for compact forms, default,
 * and lg next to 36-pixel buttons.
 */
const nativeSelectVariants = cva(
  "min-w-0 cursor-pointer rounded-md border border-input bg-background text-foreground transition-colors outline-none focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50 aria-invalid:border-destructive dark:bg-input/30 [&>option]:bg-popover [&>option]:text-popover-foreground",
  {
    variants: {
      size: {
        xs: "h-7 px-1.5 text-xs",
        sm: "h-8 px-2 text-xs",
        default: "h-8 px-2.5 text-sm",
        lg: "h-9 px-2 text-sm",
      },
    },
    defaultVariants: { size: "default" },
  },
);

function NativeSelect({ className, size, ...props }: Omit<React.ComponentProps<"select">, "size"> & VariantProps<typeof nativeSelectVariants>) {
  return <select data-slot="native-select" className={cn(nativeSelectVariants({ size }), className)} {...props} />;
}

export { NativeSelect };
