/**
 * Whether the details pane is open. On large screens it is beside the list and remembered. Narrower than a laptop it
 * covers the list: there it starts closed and isn't remembered (opened on a large screen, it would otherwise cover the
 * list on a phone from the start).
 */
import { useState } from "react";
import { useMediaQuery } from "@/lib/focus";
import { usePersisted } from "@/lib/session";

export function useDetailsPane(): [boolean, (open: boolean) => void] {
  const [kept, setKept] = usePersisted("tf-details-pane", false);
  const narrow = useMediaQuery("(max-width: 63.99rem)");
  const [over, setOver] = useState(false);
  return narrow ? [over, setOver] : [kept, setKept];
}
