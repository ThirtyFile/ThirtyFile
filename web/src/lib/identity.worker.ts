/**
 * Computes files' content identities (lib/identity.ts) away from the page, so a large upload neither waits for it nor
 * makes the page stutter. Messages: `{ id, file }` starts one, `{ id, stop: true }` ends it early; each started one is
 * answered with `{ id, identity }` or `{ id, error }`.
 */
import { identityOf } from "./identity";

const stopped = new Set<number>();

self.onmessage = async (e: MessageEvent<{ id: number; file?: Blob; stop?: boolean }>) => {
  const { id, file, stop } = e.data;
  if (stop) {
    stopped.add(id);
    return;
  }
  if (!file) return;
  try {
    const identity = await identityOf(file, () => stopped.has(id));
    self.postMessage({ id, identity });
  } catch (err) {
    self.postMessage({ id, error: String(err) });
  } finally {
    stopped.delete(id);
  }
};
