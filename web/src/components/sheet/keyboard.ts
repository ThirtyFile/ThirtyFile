/**
 * Keyboard: arrow and Ctrl+arrow navigation, direct typing, F2 to edit, shortcuts (undo, save, bold…) and CJK input methods (IME).
 */
import { type KeyboardEvent } from "react";
import { key, mergeAt, type CellStyle, type Range } from "@/lib/sheet/model";
import { HEADER_H } from "./renderer";
import { editText } from "./session";
import type { WorkspaceCtx } from "./workspace";

import { clipStore } from "./clipboard";

interface Deps {
  restyle(fn: (s: CellStyle, r: number, c: number, g: Range) => CellStyle): void;
  clearRange(): void;
}

export function createKeyboard(ctx: WorkspaceCtx, deps: Deps) {
  const {
    sheet,
    sheetIdx,
    sel,
    active,
    editing,
    setEditing,
    size,
    inputRef,
    scrollRef,
    setSel,
    setClip,
    styleAt,
    ensureVisible,
    commitChanges,
    changeAt,
    undo,
    redo,
    startEdit,
    endEdit,
    commitEdit,
    expectsRef,
    insertRef,
    save,
  } = ctx;
  const { restyle, clearRange } = deps;
  // ───── Keyboard ─────

  /** Ctrl+arrow: jump to the edge of the data block */
  const jump = ([r, c]: [number, number], dr: number, dc: number): [number, number] => {
    const has = (rr: number, cc: number) => {
      const cell = sheet.cells.get(key(rr, cc));
      return !!cell && (cell.v !== null || !!cell.f);
    };
    const limitR = dr > 0 ? sheet.maxRow : 0;
    const limitC = dc > 0 ? sheet.maxCol : 0;
    let nr = r;
    let nc = c;
    const step = () => {
      nr += dr;
      nc += dc;
    };
    const inside = () => nr >= 0 && nc >= 0 && (dr <= 0 || nr <= limitR) && (dc <= 0 || nc <= limitC);
    if (has(r, c) && has(r + dr, c + dc)) {
      while (inside() && has(nr + dr, nc + dc)) step();
      return [nr, nc];
    }
    step();
    while (inside() && !has(nr, nc)) step();
    if (!inside()) return dr > 0 || dc > 0 ? [dr ? Math.max(limitR, r) : r, dc ? Math.max(limitC, c) : c] : [Math.max(0, nr), Math.max(0, nc)];
    return [nr, nc];
  };

  const pageRows = () => Math.max(1, Math.floor((size.h - HEADER_H) / sheet.defaultRowHeight) - 1);

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.nativeEvent.isComposing || e.keyCode === 229) return;
    const mod = e.ctrlKey || e.metaKey;
    const k = e.key;
    if (mod && k.toLowerCase() === "s") {
      e.preventDefault();
      if (editing) commitEdit(null);
      void save();
      return;
    }
    if (editing) {
      if (k === "Enter" && !e.altKey) {
        e.preventDefault();
        commitEdit([e.shiftKey ? -1 : 1, 0]);
      } else if (k === "Enter" && e.altKey) {
        e.preventDefault();
        insertRef("\n");
      } else if (k === "Tab") {
        e.preventDefault();
        commitEdit([0, e.shiftKey ? -1 : 1]);
      } else if (k === "Escape") {
        e.preventDefault();
        endEdit();
      } else if (editing.mode === "enter" && k.startsWith("Arrow") && !expectsRef()) {
        e.preventDefault();
        commitEdit(k === "ArrowUp" ? [-1, 0] : k === "ArrowDown" ? [1, 0] : k === "ArrowLeft" ? [0, -1] : [0, 1]);
      } else if (k === "F2") {
        e.preventDefault();
        setEditing({ ...editing, mode: editing.mode === "enter" ? "edit" : "enter" });
      }
      return;
    }

    const [ar, ac] = sel.anchor;
    const [fr, fc] = sel.focus;
    const move = (dr: number, dc: number) => {
      e.preventDefault();
      const from: [number, number] = e.shiftKey ? [fr, fc] : [ar, ac];
      let to: [number, number];
      if (mod) to = jump(from, dr, dc);
      else {
        // When moving right/down out of a merged cell, skip the whole merged range
        const m = mergeAt(sheet, from[0], from[1]);
        to = [from[0] + (dr > 0 && m ? m.r2 - from[0] + 1 : dr), from[1] + (dc > 0 && m ? m.c2 - from[1] + 1 : dc)];
        const mt = mergeAt(sheet, to[0], to[1]);
        if (mt && !e.shiftKey) to = [mt.r1, mt.c1];
      }
      setSel(e.shiftKey ? { anchor: [ar, ac], focus: to } : { anchor: to, focus: to });
    };
    if (mod) {
      const lower = k.toLowerCase();
      const toggles: Record<string, "bold" | "italic" | "underline" | "strike"> = { b: "bold", i: "italic", u: "underline", "5": "strike" };
      if (toggles[lower]) {
        e.preventDefault();
        const prop = toggles[lower];
        const on = !styleAt(ar, ac)?.[prop];
        return restyle((s) => ({ ...s, [prop]: on }));
      }
      if (lower === "z") {
        e.preventDefault();
        return e.shiftKey ? redo() : undo();
      }
      if (lower === "y") {
        e.preventDefault();
        return redo();
      }
      if (lower === "a") {
        e.preventDefault();
        return setSel({ anchor: [0, 0], focus: [Math.max(sheet.maxRow, 0), Math.max(sheet.maxCol, 0)] }, false);
      }
    }
    switch (k) {
      case "ArrowUp":
        return move(-1, 0);
      case "ArrowDown":
        return move(1, 0);
      case "ArrowLeft":
        return move(0, -1);
      case "ArrowRight":
        return move(0, 1);
      case "Enter":
        return move(e.shiftKey ? -1 : 1, 0);
      case "Tab":
        e.preventDefault();
        return setSel({ anchor: [ar, ac + (e.shiftKey ? -1 : 1)], focus: [ar, ac + (e.shiftKey ? -1 : 1)] });
      case "PageDown":
      case "PageUp": {
        e.preventDefault();
        const n = pageRows() * (k === "PageDown" ? 1 : -1);
        const to: [number, number] = [Math.max(0, ar + n), ac];
        scrollRef.current?.scrollBy({ top: n * sheet.defaultRowHeight });
        return setSel({ anchor: to, focus: to });
      }
      case "Home": {
        e.preventDefault();
        const to: [number, number] = mod ? [0, 0] : [ar, 0];
        return setSel({ anchor: to, focus: to });
      }
      case "End":
        if (mod) {
          e.preventDefault();
          const to: [number, number] = [sheet.maxRow, sheet.maxCol];
          return setSel({ anchor: to, focus: to });
        }
        return;
      case "F2":
        e.preventDefault();
        return startEdit(editText(sheet.cells.get(key(ar, ac)), styleAt(ar, ac)), "edit");
      case "Delete":
        e.preventDefault();
        return clearRange();
      case "Backspace":
        e.preventDefault();
        commitChanges([changeAt(sheetIdx, ar, ac, null)]);
        return startEdit("", "enter");
      case "Escape":
        clipStore.current = null;
        return setClip(null);
    }
    // Other printable characters: let the textarea handle them; the input event starts editing
  };

  const onInput = () => {
    const el = inputRef.current!;
    if (!editing) {
      // Direct typing: replace the existing content with what was typed
      setEditing({ text: el.value, mode: "enter", from: "cell" });
      ensureVisible(active[0], active[1]);
    } else setEditing({ ...editing, text: el.value });
  };

  const onCompositionStart = () => {
    // CJK input method: show the editor before a candidate is chosen so the candidate window appears next to the cell
    if (!editing) setEditing({ text: "", mode: "enter", from: "cell" });
  };

  return { jump, pageRows, onKeyDown, onInput, onCompositionStart };
}
