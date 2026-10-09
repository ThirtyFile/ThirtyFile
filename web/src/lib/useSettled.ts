import { useEffect, useState } from "react";

/**
 * `value` once it has stayed the same for `ms`: moving through a list with the arrow keys doesn't ask the server about
 * every item passed (a held key would send hundreds of requests). The first value is there at once.
 */
export function useSettled<T>(value: T, ms: number): T {
  const [settled, setSettled] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setSettled(value), ms);
    return () => clearTimeout(timer);
  }, [value, ms]);
  return settled;
}
