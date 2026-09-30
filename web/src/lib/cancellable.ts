/**
 * Loading that an effect stops when it cleans up (another file is shown, the component goes away): `load` gets the
 * signal that stops its request, and `done` or `failed` is called only while the result is still wanted. Returns the
 * effect's cleanup.
 *
 *   useEffect(() => cancellable((signal) => fetchOk(url, { signal }), (res) => …, (e) => setError(e.message)), [url]);
 */
export function cancellable<T>(load: (signal: AbortSignal) => Promise<T>, done: (value: T) => void, failed: (e: Error) => void): () => void {
  let cancelled = false;
  const abort = new AbortController();
  load(abort.signal)
    .then((value) => {
      if (!cancelled) done(value);
    })
    .catch((e: Error) => {
      if (!cancelled) failed(e);
    });
  return () => {
    cancelled = true;
    abort.abort();
  };
}
