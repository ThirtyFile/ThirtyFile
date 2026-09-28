import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { toast } from "sonner";
import { cn } from "@/lib/utils";
import { t } from "@/lib/i18n";

/**
 * Inline rename (like Windows File Explorer): the name turns into an input with only the base name selected.
 * Enter or clicking elsewhere commits, Esc cancels; an unchanged or blank name counts as cancel; on failure show the reason and stay in edit mode.
 */
export function InlineRename({
  initial,
  onSubmit,
  onDone,
  multiline,
  selectAll,
  className,
}: {
  initial: string;
  onSubmit(name: string): Promise<void>;
  /** Called after committing or cancelling; `byKey` when that was Enter or Esc (not clicking elsewhere), so the caller can put the focus back */
  onDone(byKey: boolean): void;
  /** Icon view: input that wraps */
  multiline?: boolean;
  /** Select all (folder and space names have no extension) */
  selectAll?: boolean;
  className?: string;
}) {
  const [value, setValue] = useState(initial);
  const [busy, setBusy] = useState(false);
  const ref = useRef<HTMLInputElement & HTMLTextAreaElement>(null);
  // After submitting, or after Esc, losing focus doesn't submit again
  const finished = useRef(false);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.focus();
    el.scrollIntoView({ block: "nearest" });
    const dot = selectAll ? -1 : initial.lastIndexOf(".");
    el.setSelectionRange(0, dot > 0 ? dot : initial.length);
  }, [initial, selectAll]);

  // Icon view: height grows and shrinks with the content (like the Windows name box)
  useLayoutEffect(() => {
    const el = ref.current;
    if (!multiline || !el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight + 2}px`;
  }, [multiline, value]);

  const commit = async (byKey: boolean) => {
    if (finished.current || busy) return;
    const name = value.trim();
    if (!name || name === initial) {
      finished.current = true;
      onDone(byKey);
      return;
    }
    setBusy(true);
    try {
      await onSubmit(name);
      finished.current = true;
      onDone(byKey);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : t("Couldn't rename"));
      setBusy(false);
      // Stay in edit mode so the user can fix it
      requestAnimationFrame(() => ref.current?.focus());
    }
  };

  const onKeyDown = (e: KeyboardEvent) => {
    e.stopPropagation();
    if (e.key === "Enter" && !e.nativeEvent.isComposing) {
      e.preventDefault();
      void commit(true);
    } else if (e.key === "Escape") {
      e.preventDefault();
      finished.current = true;
      onDone(true);
    }
  };

  const props = {
    ref,
    value,
    disabled: busy,
    "aria-label": t("New name"),
    spellCheck: false,
    onChange: (e: { target: { value: string } }) => setValue(e.target.value.replace(/[\r\n]/g, "")),
    onKeyDown,
    onBlur: () => void commit(false),
    // Don't let clicks and drag-selecting text affect the outer selection and drag-and-drop
    onClick: (e: { stopPropagation(): void }) => e.stopPropagation(),
    onDoubleClick: (e: { stopPropagation(): void }) => e.stopPropagation(),
    onMouseDown: (e: { stopPropagation(): void }) => e.stopPropagation(),
    onContextMenu: (e: { stopPropagation(): void }) => e.stopPropagation(),
    onDragStart: (e: { preventDefault(): void; stopPropagation(): void }) => {
      e.preventDefault();
      e.stopPropagation();
    },
  };
  const base = "min-w-0 rounded-[3px] border border-brand bg-background px-1 text-foreground outline-none select-text disabled:opacity-60";
  return multiline ? (
    <textarea {...props} rows={1} className={cn(base, "w-full resize-none text-center text-xs leading-[18px] break-all", className)} />
  ) : (
    <input {...props} className={cn(base, "h-[22px] w-full text-xs", className)} />
  );
}
