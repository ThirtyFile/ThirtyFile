import { useEffect, useRef, useState, type MouseEvent as ReactMouseEvent } from "react";

interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** Distance (pixels) to move before marquee selection starts, so a plain click isn't treated as a marquee */
const THRESHOLD = 4;
/** Auto-scroll zone and speed when dragging to an edge */
const EDGE = 28;
const SPEED = 14;

/**
 * Mouse marquee selection (like Windows File Explorer): hold the left button on empty space and drag to select every item in the box; with Ctrl held, toggle the boxed items.
 * Items are marked with `data-node-id` and count as selected when any part is boxed; in list view `data-drag-handle` (the name) is for drag-moving, so marquee doesn't start there.
 * Put the returned props on the scrollable container (which must be position: relative); box is the selection box to draw (in container content coordinates).
 */
export function useMarquee({ selected, onSelect, enabled = true }: { selected: Set<string>; onSelect(ids: Set<string>): void; enabled?: boolean }) {
  const [box, setBox] = useState<Box | null>(null);
  const drag = useRef<{
    container: HTMLElement;
    start: { x: number; y: number };
    client: { x: number; y: number };
    base: Set<string>;
    toggle: boolean;
    active: boolean;
    frame: number;
    /** Item boxes in container content coordinates, measured once when the marquee starts (layout doesn't change during a drag) */
    boxes: { id: string; x: number; y: number; w: number; h: number }[] | null;
    /** A mousemove already scheduled an update for the next frame */
    scheduled: boolean;
  } | null>(null);
  const suppressClick = useRef(false);
  // Event handlers stay in use during the drag, so get the latest callbacks via a ref
  const select = useRef(onSelect);
  select.current = onSelect;
  const current = useRef(selected);
  current.current = selected;

  const update = () => {
    const d = drag.current;
    if (!d) return;
    const r = d.container.getBoundingClientRect();
    const x = d.client.x - r.left + d.container.scrollLeft;
    const y = d.client.y - r.top + d.container.scrollTop;
    if (!d.active) {
      if (Math.hypot(x - d.start.x, y - d.start.y) < THRESHOLD) return;
      d.active = true;
    }
    const b = { x: Math.min(x, d.start.x), y: Math.min(y, d.start.y), w: Math.abs(x - d.start.x), h: Math.abs(y - d.start.y) };
    setBox(b);
    if (!d.boxes) {
      // Measured once: thousands of getBoundingClientRect calls per mousemove would make large folders stutter
      const sl = d.container.scrollLeft - r.left;
      const st = d.container.scrollTop - r.top;
      d.boxes = Array.from(d.container.querySelectorAll<HTMLElement>("[data-node-id]"), (el) => {
        // In list view the whole row counts (like full-row selection in Windows Details view)
        const hit = el.getBoundingClientRect();
        return { id: el.dataset.nodeId!, x: hit.left + sl, y: hit.top + st, w: hit.width, h: hit.height };
      });
    }
    const next = new Set(d.base);
    for (const it of d.boxes) {
      if (it.x + it.w < b.x || it.x > b.x + b.w || it.y + it.h < b.y || it.y > b.y + b.h) continue;
      if (d.toggle && d.base.has(it.id)) next.delete(it.id);
      else next.add(it.id);
    }
    // Only re-render the list when the selection actually changed
    const cur = current.current;
    if (next.size !== cur.size || [...next].some((id) => !cur.has(id))) select.current(next);
  };

  // Auto-scroll when dragging to the top or bottom edge
  const autoScroll = () => {
    const d = drag.current;
    if (!d) return;
    if (d.active) {
      const r = d.container.getBoundingClientRect();
      const dy = d.client.y < r.top + EDGE ? -SPEED : d.client.y > r.bottom - EDGE ? SPEED : 0;
      if (dy) {
        const before = d.container.scrollTop;
        d.container.scrollTop += dy;
        if (d.container.scrollTop !== before) update();
      }
    }
    d.frame = requestAnimationFrame(autoScroll);
  };

  const stop = () => {
    const d = drag.current;
    if (!d) return;
    // A move still waiting for its frame: apply it now, the selection ends where the mouse was released
    if (d.scheduled) update();
    cancelAnimationFrame(d.frame);
    drag.current = null;
    if (d.active) {
      setBox(null);
      // The click after releasing the mouse shouldn't clear the selection again
      suppressClick.current = true;
      setTimeout(() => (suppressClick.current = false), 0);
    }
  };

  useEffect(() => {
    const move = (e: MouseEvent) => {
      const d = drag.current;
      if (!d) return;
      d.client = { x: e.clientX, y: e.clientY };
      // At most one update per frame, however often the mouse reports
      if (d.scheduled) return;
      d.scheduled = true;
      requestAnimationFrame(() => {
        if (drag.current === d) d.scheduled = false;
        update();
      });
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", stop);
    window.addEventListener("blur", stop);
    return () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", stop);
      window.removeEventListener("blur", stop);
      stop();
    };
    // update and stop only read refs
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const onMouseDown = (e: ReactMouseEvent<HTMLElement>) => {
    if (!enabled || e.button !== 0) return;
    const container = e.currentTarget;
    const target = e.target as HTMLElement;
    // Item names (drag-moving), icon-view items, buttons and inputs don't start a marquee
    if (target.closest("[data-drag-handle], button, input, textarea, a, th, [data-node-id]:not(tr)")) return;
    const r = container.getBoundingClientRect();
    // Clicked on the scrollbar
    if (e.clientX >= r.left + container.clientWidth || e.clientY >= r.top + container.clientHeight) return;
    e.preventDefault(); // Don't select text
    const toggle = e.ctrlKey || e.metaKey;
    drag.current = {
      container,
      start: { x: e.clientX - r.left + container.scrollLeft, y: e.clientY - r.top + container.scrollTop },
      client: { x: e.clientX, y: e.clientY },
      base: toggle ? new Set(selected) : new Set(),
      toggle,
      active: false,
      frame: requestAnimationFrame(autoScroll),
      boxes: null,
      scheduled: false,
    };
  };

  const onClickCapture = (e: ReactMouseEvent) => {
    if (suppressClick.current) {
      suppressClick.current = false;
      e.stopPropagation();
      e.preventDefault();
    }
  };

  return { box, containerProps: { onMouseDown, onClickCapture } };
}
