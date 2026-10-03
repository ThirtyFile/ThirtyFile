/**
 * The Mac style's symbols for its sidebar and toolbar, drawn for ThirtyFile on a 24-unit grid (no Apple artwork). They
 * are built with lucide's icon factory (ISC licence, already in the notices), so they take the same props and sizes as
 * the shared icons; mac.css draws them with a thinner line.
 */
import { createLucideIcon, type LucideIcon } from "lucide-react";

type Node = Parameters<typeof createLucideIcon>[1];

const path = (d: string): Node[number] => ["path", { d, key: d }];
const circle = (cx: number, cy: number, r: number, filled = false): Node[number] => [
  "circle",
  { cx: String(cx), cy: String(cy), r: String(r), key: `c${cx},${cy},${r}`, ...(filled ? { fill: "currentColor", stroke: "none" } : {}) },
];
const rect = (x: number, y: number, width: number, height: number, rx: number): Node[number] => [
  "rect",
  { x: String(x), y: String(y), width: String(width), height: String(height), rx: String(rx), key: `r${x},${y},${width},${height}` },
];
const symbol = (name: string, ...node: Node): LucideIcon => createLucideIcon(`mac-${name}`, node);

// A folder's outline, shared by the folder symbols
const FOLDER = "M3.5 7.5a2 2 0 0 1 2-2h3.6a1.6 1.6 0 0 1 1.2.5l1.7 1.9h6.5a2 2 0 0 1 2 2v8.6a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z";

export const MAC_SYMBOLS = {
  // Sidebar
  favorites: symbol("favorites", path("M12 3.8l2.5 5.1 5.6.8-4.05 3.95.95 5.55L12 16.6l-5 2.6.95-5.55L3.9 9.7l5.6-.8z")),
  spaces: symbol("spaces", path("M12 4.2 3.8 8.4 12 12.6l8.2-4.2z"), path("M3.8 12.2 12 16.4l8.2-4.2"), path("M3.8 16 12 20.2l8.2-4.2")),
  personal: symbol("personal", circle(12, 8.2, 3.6), path("M4.8 20c.9-3.7 3.7-5.7 7.2-5.7s6.3 2 7.2 5.7")),
  company: symbol(
    "company",
    rect(5, 3.8, 9.5, 16.7, 1.6),
    path("M14.5 9.3h3.3a1.7 1.7 0 0 1 1.7 1.7v9.5"),
    path("M8.3 7.6h2.9"),
    path("M8.3 11.2h2.9"),
    path("M8.3 14.8h2.9"),
    path("M3.5 20.5h17"),
  ),
  team: symbol("team", path("M5 6.2v11.6c0 1.5 3.1 2.7 7 2.7s7-1.2 7-2.7V6.2"), path("M5 12c0 1.5 3.1 2.7 7 2.7s7-1.2 7-2.7"), [
    "ellipse",
    { cx: "12", cy: "6.2", rx: "7", ry: "2.7", key: "top" },
  ]),
  folder: symbol("folder", path(FOLDER), path("M3.5 10.4h17")),
  smartFolder: symbol(
    "smart-folder",
    path("M11 19.4H5.5a2 2 0 0 1-2-2V7.5a2 2 0 0 1 2-2h3.6a1.6 1.6 0 0 1 1.2.5l1.7 1.9h6.5a2 2 0 0 1 2 2v1.6"),
    circle(15.6, 15, 2.9),
    path("m17.8 17.2 2.6 2.6"),
  ),
  sharedWithMe: symbol("shared", circle(9, 8.4, 3.1), path("M3.4 19.2c.6-3 2.8-4.7 5.6-4.7s5 1.7 5.6 4.7"), path("M15.2 5.6a2.9 2.9 0 0 1 0 5.6"), path("M17 14.6c2 .4 3.3 1.9 3.7 4.6")),
  shareLinks: symbol("links", path("M10.2 13.8a4 4 0 0 0 5.7 0l2.9-2.9a4 4 0 0 0-5.7-5.7l-.9.9"), path("M13.8 10.2a4 4 0 0 0-5.7 0l-2.9 2.9a4 4 0 0 0 5.7 5.7l.9-.9")),
  recent: symbol("recent", circle(12, 12, 8.4), path("M12 7.4V12l3.1 1.9")),
  trash: symbol(
    "trash",
    path("M4.5 6.6h15"),
    path("M9.4 6.6V5.1a1.6 1.6 0 0 1 1.6-1.6h2a1.6 1.6 0 0 1 1.6 1.6v1.5"),
    path("M6.4 6.6l.9 12.4a2 2 0 0 0 2 1.9h5.4a2 2 0 0 0 2-1.9l.9-12.4"),
    path("M10.1 10.6v6.2"),
    path("M13.9 10.6v6.2"),
  ),
  controlPanel: symbol(
    "control-panel",
    path("M4 7h8.6"),
    path("M17.4 7H20"),
    path("M4 12h2.6"),
    path("M11.4 12H20"),
    path("M4 17h10.6"),
    path("M19.4 17H20"),
    circle(15, 7, 2.4),
    circle(9, 12, 2.4),
    circle(17, 17, 2.4),
  ),

  // Toolbar
  back: symbol("back", path("M14.6 5.4 8 12l6.6 6.6")),
  forward: symbol("forward", path("M9.4 5.4 16 12l-6.6 6.6")),
  viewIcons: symbol("view-icons", rect(4, 4, 6.6, 6.6, 1.6), rect(13.4, 4, 6.6, 6.6, 1.6), rect(4, 13.4, 6.6, 6.6, 1.6), rect(13.4, 13.4, 6.6, 6.6, 1.6)),
  viewList: symbol("view-list", path("M9 6.6h11"), path("M9 12h11"), path("M9 17.4h11"), circle(5, 6.6, 1.2, true), circle(5, 12, 1.2, true), circle(5, 17.4, 1.2, true)),
  viewColumns: symbol("view-columns", rect(3.5, 4.5, 17, 15, 2.4), path("M9.2 4.5v15"), path("M14.8 4.5v15")),
  viewGallery: symbol("view-gallery", rect(3.5, 3.6, 17, 11.4, 2), rect(3.5, 17.4, 4.6, 3, 1), rect(9.7, 17.4, 4.6, 3, 1), rect(15.9, 17.4, 4.6, 3, 1)),
  sort: symbol("sort", path("M4 6.8h16"), path("M6.8 12h10.4"), path("M9.6 17.2h4.8")),
  share: symbol("share", path("M12 3.6v11"), path("M8.6 7 12 3.6 15.4 7"), path("M8.2 10.4H7a2 2 0 0 0-2 2v6.1a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-6.1a2 2 0 0 0-2-2h-1.2")),
  actions: symbol("actions", circle(12, 12, 8.4), circle(8.4, 12, 1.1, true), circle(12, 12, 1.1, true), circle(15.6, 12, 1.1, true)),
} satisfies Record<string, LucideIcon>;

export type MacSymbol = keyof typeof MAC_SYMBOLS;
