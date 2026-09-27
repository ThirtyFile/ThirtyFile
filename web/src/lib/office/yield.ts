/**
 * Long synchronous work (parsing tens of thousands of cells, laying out hundreds of pages) is split with `await breathe()`:
 * every ~16 ms the loop hands control back to the browser, so the page keeps painting and responding, and the host's
 * time limit can stop a runaway document. DOM APIs aren't available in workers, so this is what keeps layout work off a frozen UI.
 */
let last = 0;

export function breathe(): Promise<void> | undefined {
  const now = performance.now();
  if (now - last < 16) return undefined;
  last = now;
  const sched = (globalThis as { scheduler?: { yield?: () => Promise<void> } }).scheduler;
  return sched?.yield ? sched.yield() : new Promise((r) => setTimeout(r, 0));
}
