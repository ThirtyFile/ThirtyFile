import { useState, type FormEvent } from "react";
import { errorMessage } from "@/lib/utils";

/**
 * Submitting a form that isn't a React Query mutation: `busy` while `fn` runs, and `error`, what went wrong, once it
 * failed (`fallback` when what was thrown isn't an Error). `run` is the form's onSubmit, or a button's onClick.
 */
export function useSubmit(fn: () => Promise<void>, fallback?: string) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = async (e?: FormEvent) => {
    e?.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await fn();
    } catch (err) {
      setError(errorMessage(err, fallback ?? String(err)));
    } finally {
      setBusy(false);
    }
  };
  return { busy, error, run };
}
