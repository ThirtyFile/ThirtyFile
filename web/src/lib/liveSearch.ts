/**
 * Search as you type, waiting for input methods (Zhuyin, Pinyin, Japanese, Korean...) to finish composing: text still
 * being composed is never searched, and starting to compose cancels a search that was about to run. Once the text is
 * committed it is searched like typed text. Detected from composition events, not from the characters typed.
 */
export interface LiveSearch {
  /** The text changed; `composing` when the browser says an input method is composing it */
  input(value: string, composing: boolean): void;
  compositionStart(): void;
  /** Composition finished (a candidate chosen, or cancelled): `value` is the text now in the box */
  compositionEnd(value: string): void;
  /** Whether an input method is composing: its keys (Enter to choose, Escape to cancel) are its own */
  composing(): boolean;
  /** Drop a search that is waiting to run (leaving the page) */
  cancel(): void;
}

/** `run` gets the text to search `delay()` ms after the last change, or at once when that is 0 */
export function liveSearch(run: (q: string) => void, delay: () => number): LiveSearch {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let composing = false;
  /** Text just searched when composition ended: some browsers report it once more as a plain change */
  let ended: string | undefined;
  const schedule = (v: string) => {
    clearTimeout(timer);
    const ms = delay();
    if (ms <= 0) run(v);
    else timer = setTimeout(() => run(v), ms);
  };
  return {
    input(value, isComposing) {
      if (composing || isComposing) return;
      const again = ended === value;
      ended = undefined;
      if (!again) schedule(value);
    },
    compositionStart() {
      composing = true;
      ended = undefined;
      clearTimeout(timer);
    },
    compositionEnd(value) {
      composing = false;
      ended = value;
      schedule(value);
    },
    composing: () => composing,
    cancel: () => clearTimeout(timer),
  };
}
