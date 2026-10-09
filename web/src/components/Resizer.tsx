import { useEffect, useRef } from "react";
import { cn } from "@/lib/utils";

/**
 * Drag handle on a pane edge (only changes the mouse cursor, no colored bar).
 * `edge` is the side the handle is on: a right-hand pane's handle is on its left edge (drag left to widen), a left-hand pane's on its right edge.
 * Double-click to restore the default width. Also resizes table columns (see FileList).
 */
export function Resizer(props: {
  width: number;
  onChange(width: number): void;
  min: number;
  max: number;
  defaultWidth: number;
  edge: "left" | "right";
  label: string;
  /** The width it has now, when `width` isn't it (a column taking the space left) */
  measure?(handle: HTMLElement): number;
  /** Double-click: instead of going back to `defaultWidth` */
  onReset?(): void;
  /** Not a Tab stop (reached another way: the Columns view's handles of the columns other than the open folder's) */
  untabbable?: boolean;
}) {
  // The drag in progress, if the panel goes away mid-drag (the listeners and the page cursor would otherwise stay)
  const release = useRef<(() => void) | null>(null);
  useEffect(() => () => release.current?.(), []);
  const clamp = (w: number) => Math.round(Math.min(props.max, Math.max(props.min, w)));
  const current = (handle: HTMLElement) => props.measure?.(handle) ?? props.width;
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label={props.label}
      aria-valuenow={props.width}
      aria-valuemin={props.min}
      aria-valuemax={props.max}
      tabIndex={props.untabbable ? -1 : 0}
      className={cn(
        // A bar where it is when it has the keyboard focus, ringed as buttons are (the mouse only changes the cursor)
        "absolute inset-y-0 z-10 w-1.5 cursor-col-resize rounded-full outline-none focus-visible:bg-brand focus-visible:ring-3 focus-visible:ring-ring max-md:hidden",
        props.edge === "right" ? "-right-[3px]" : "-left-[3px]",
        // The last column's handle stays inside the table: sticking out, it would make the list scroll sideways
        "[th:last-child>&]:right-0",
      )}
      onDoubleClick={() => (props.onReset ? props.onReset() : props.onChange(props.defaultWidth))}
      onKeyDown={(e) => {
        const grow = props.edge === "right" ? 1 : -1;
        if (e.key === "ArrowLeft") props.onChange(clamp(current(e.currentTarget) - 16 * grow));
        if (e.key === "ArrowRight") props.onChange(clamp(current(e.currentTarget) + 16 * grow));
      }}
      onPointerDown={(e) => {
        e.preventDefault();
        const startX = e.clientX;
        const startW = current(e.currentTarget);
        const dir = props.edge === "right" ? 1 : -1;
        // While dragging, keep the resize cursor on the whole page and avoid selecting text
        document.body.style.cursor = "col-resize";
        document.body.style.userSelect = "none";
        const move = (ev: PointerEvent) => props.onChange(clamp(startW + (ev.clientX - startX) * dir));
        const up = () => {
          document.body.style.cursor = "";
          document.body.style.userSelect = "";
          window.removeEventListener("pointermove", move);
          window.removeEventListener("pointerup", up);
          window.removeEventListener("pointercancel", up);
          release.current = null;
        };
        release.current = up;
        window.addEventListener("pointermove", move);
        window.addEventListener("pointerup", up);
        window.addEventListener("pointercancel", up);
      }}
    />
  );
}
