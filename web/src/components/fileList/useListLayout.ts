//! Where the file list's rows go: the scroll container and the room the list has in it, the rows of the view (a
//! group's heading, one item in Details, a row of items in the icon views), and the rows rendered (virtualised)

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { defaultRangeExtractor, useVirtualizer } from "@tanstack/react-virtual";
import { GROUP_H, GROUP_ROW, HEAD, PAD, PHONE_GRID_W, ROW, TILED, evenLayout, groupedLayout, offsetIn, scrollParent, type Item, type ViewMode } from "@/components/fileList/layout";
import type { Group } from "@/lib/listView";

export interface ListLayoutOptions {
  view: ViewMode;
  /** Tablet width and up */
  wide: boolean;
  /** The items group by group, when they are grouped */
  groups: Group<Item>[] | null;
  /** How many items the list has */
  n: number;
  /** Items whose rows are rendered even out of view (the Tab stop, the focused item, the one being renamed) */
  keep: (number | undefined)[];
}

export function useListLayout({ view, wide, groups, n, keep }: ListLayoutOptions) {
  const [scroller, setScroller] = useState<HTMLElement | null>(null);
  /** Where the first row starts in the scroll container's content, the list's width, and the width it has in view */
  const [geo, setGeo] = useState({ top: 0, width: 0, room: 0 });
  const root = useRef<HTMLElement | null>(null);
  const head = useRef<HTMLTableSectionElement>(null);
  // Anything but an icon view (also a view saved by a later version) is Details
  const tile: (typeof TILED)[keyof typeof TILED] | null = view === "list" ? null : (TILED[view] ?? null);
  const grid = !!tile;

  // Phones: large icons a little narrower, three to a row rather than two with wide gaps
  const minTileW = tile && view === "grid" && !wide ? PHONE_GRID_W : tile?.w;
  const cols = tile ? Math.max(1, Math.floor((geo.width - 2 * PAD + tile.gap) / (minTileW! + tile.gap))) : 1;
  // The rows: in each group, its heading and then its items, `cols` to a row
  const layout = useMemo(
    () => (groups ? groupedLayout(groups, cols, tile ? tile.h + tile.gap : ROW, tile ? GROUP_H : GROUP_ROW) : evenLayout(n, cols, tile ? tile.h + tile.gap : ROW)),
    [groups, n, cols, tile],
  );
  const rowOf = (i: number) => layout.rowOf(i);
  const pinned = keep.filter((i): i is number => i !== undefined && i < n).map(rowOf);

  // Row sizes are cached per key: new keys whenever the rows change
  // oxlint-disable-next-line react-hooks/exhaustive-deps -- the layout is what invalidates the keys
  const rowKey = useCallback((i: number) => i, [layout]);
  const v = useVirtualizer({
    count: layout.count,
    getScrollElement: () => scroller,
    estimateSize: (i) => layout.row(i).size,
    getItemKey: rowKey,
    overscan: grid ? 2 : 12,
    scrollMargin: geo.top,
    // Keep rows scrolled to by the keyboard clear of the sticky column headers
    scrollPaddingStart: grid ? PAD : HEAD,
    rangeExtractor: (range) => [...new Set([...defaultRangeExtractor(range), ...pinned])].sort((a, b) => a - b),
    initialRect: { width: 0, height: typeof window === "undefined" ? 800 : window.innerHeight },
  });

  // Find the scroll container, and where the rows start in it: measured before the first paint of a view, then when the
  // list changes size (not after every render: that reads the layout on every frame of a marquee drag)
  const measureGeo = useCallback(() => {
    const el = root.current;
    if (!el) return;
    const s = scrollParent(el);
    setScroller((cur) => (cur === s ? cur : s));
    const top = offsetIn(el, s).top + (head.current ? head.current.offsetHeight : PAD);
    const width = el.clientWidth;
    const room = s === document.documentElement ? window.innerWidth : s.clientWidth;
    setGeo((g) => (g.top === top && g.width === width && g.room === room ? g : { top, width, room }));
  }, []);
  const empty = n === 0;
  useLayoutEffect(measureGeo, [measureGeo, grid, empty]);
  useEffect(() => {
    const el = root.current;
    if (!el) return;
    const ro = new ResizeObserver(measureGeo);
    ro.observe(el);
    // The list can be wider than the space it has (Details view scrolling sideways): that space is watched too
    const s = scrollParent(el);
    if (s !== document.documentElement) ro.observe(s);
    return () => ro.disconnect();
  }, [measureGeo, grid, empty]);

  return { root, head, scroller, geo, tile, grid, cols, layout, rowOf, v };
}
