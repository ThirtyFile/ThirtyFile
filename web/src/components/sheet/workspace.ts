/**
 * State and operations shared by the editor view's (Workspace) feature modules.
 * The main component assembles this object on each render and hands it to the format, clipboard, keyboard, mouse and other modules.
 */
import type { Dispatch, RefObject, SetStateAction } from "react";
import type { Node } from "@/api";
import type { Calculator } from "@/lib/sheet/formula";
import type { Axis, CellStyle, Range, Scalar, Sheet, Workbook } from "@/lib/sheet/model";
import type { View } from "./renderer";
import type { MenuTarget } from "./SheetMenu";
import type { Change, Entry, Layout, Sel, Session } from "./session";

export interface Editing {
  text: string;
  /** enter: started by typing directly, arrow keys commit and move; edit: double-click/F2, arrow keys move the caret */
  mode: "enter" | "edit";
  from: "cell" | "bar";
}

export type Drag =
  | { kind: "cell" | "row" | "col" }
  | { kind: "ref"; refStart: number }
  | { kind: "resize-col" | "resize-row"; index: number; start: number; size: number; before: Layout };

export interface WorkspaceCtx {
  node: Node;
  session: Session;
  book: Workbook;
  calc: Calculator;
  sheet: Sheet;
  sheetIdx: number;
  sel: Sel;
  range: Range;
  active: [number, number];
  /** Select whole rows/columns */
  wholeRows: boolean;
  wholeCols: boolean;
  editing: Editing | null;
  setEditing: Dispatch<SetStateAction<Editing | null>>;
  size: { w: number; h: number };
  rows: Axis;
  cols: Axis;
  view(): View;
  inputRef: RefObject<HTMLTextAreaElement | null>;
  scrollRef: RefObject<HTMLDivElement | null>;
  canvasRef: RefObject<HTMLCanvasElement | null>;
  drag: { current: Drag | null };
  refAnchor: { current: [number, number] };
  setSel(next: Sel, reveal?: boolean): void;
  setSelState: Dispatch<SetStateAction<Sel>>;
  setClip(g: Range | null): void;
  setMenuTarget(t: MenuTarget): void;
  cursor: string;
  setCursor(c: string): void;
  bumpLayout(): void;
  styleAt(r: number, c: number, s?: Sheet): CellStyle | undefined;
  ensureVisible(r: number, c: number): void;
  focusGrid(): void;
  commitChanges(changes: Change[], layout?: Entry["layout"]): void;
  changeAt(s: number, r: number, c: number, content: { v: Scalar; f?: string } | null, style?: number | null): Change;
  applyLayout(sheet: number, l: Layout): void;
  pushEntry(e: Entry): void;
  undo(): void;
  redo(): void;
  startEdit(text: string, mode: Editing["mode"], from?: Editing["from"]): void;
  endEdit(): void;
  commitEdit(move: [number, number] | null): void;
  expectsRef(): boolean;
  insertRef(ref: string, replaceFrom?: number): number;
  save(): Promise<void>;
}
