import { useEffect, useRef } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api, moveActive, type MoveState, type MovesList, type SpaceMove } from "@/api";
import { keys } from "@/api/queryKeys";
import { invalidateFiles } from "@/lib/queries";

/** A move changing state between two refreshes of the list */
export interface MoveChange {
  move: SpaceMove;
  from: MoveState;
}

const busy = (s: MoveState) => s === "running" || s === "queued";

/**
 * The moves of spaces between storage locations (Control panel › Moves), refreshed every `interval` ms while one runs
 * or waits, and every `paused` ms while one is only paused or stopped (it may be resumed elsewhere; false: not then).
 * When a move stops running (done, cancelled, paused, failed), the spaces, the storage locations and the navigation
 * pane are refreshed, as a finished move changes where a space is and how full each location is. `onChange` is told
 * of every change of state.
 */
export function useMoves(interval: number, paused: number | false = false, onChange?: (changes: MoveChange[]) => void) {
  const qc = useQueryClient();
  const q = useQuery({
    queryKey: keys.moves(),
    queryFn: api.moves,
    refetchInterval: (query) => {
      const moves = query.state.data?.moves ?? [];
      return moves.some((m) => busy(m.state)) ? interval : moves.some(moveActive) ? paused : false;
    },
  });
  const seen = useRef<{ data: MovesList; states: Map<string, MoveState> } | null>(null);
  const report = useRef(onChange);
  useEffect(() => {
    report.current = onChange;
  });
  useEffect(() => {
    const data = q.data;
    if (!data || seen.current?.data === data) return;
    const before = seen.current?.states;
    seen.current = { data, states: new Map(data.moves.map((m) => [m.id, m.state])) };
    if (!before) return;
    const changes = data.moves.flatMap((m) => {
      const from = before.get(m.id);
      return from && from !== m.state ? [{ move: m, from }] : [];
    });
    if (!changes.length) return;
    if (changes.some((c) => busy(c.from) && !busy(c.move.state))) void invalidateFiles(qc, "admin-drives", "storage-locations");
    report.current?.(changes);
  }, [q.data, qc]);
  return q;
}
