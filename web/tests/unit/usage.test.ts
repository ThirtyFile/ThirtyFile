import { describe, expect, it } from "vitest";
import {
  alignSeries,
  byteTicks,
  countTicks,
  diskUsed,
  errorRate,
  historyCsv,
  niceTicks,
  perSecond,
  storedBytes,
  sumFigures,
  usedPercent,
  type UsageCapacity,
  type UsageHistory,
} from "@/lib/usage";

const cap = (over: Partial<UsageCapacity> = {}): UsageCapacity => ({
  location_id: "local",
  sampled_at: 1_700_000_000,
  live_bytes: 600,
  trash_bytes: 50,
  version_bytes: 30,
  store_bytes: 200,
  folder_bytes: 100,
  folder_version_bytes: 10,
  pending_deletes: 0,
  temp_bytes: 0,
  backup_bytes: null,
  replica_bytes: null,
  disk_free: 750,
  disk_total: 1000,
  disk_id: "1",
  online: true,
  took_ms: 3,
  ...over,
});

const figures = (count: number, errors = 0, timeouts = 0) => ({ count, errors, timeouts, bytes: count * 10, p50_ms: 2, p95_ms: 9, mean_ms: 3, max_ms: 12 });

describe("storage usage", () => {
  it("never derives a percentage from an unknown disk size", () => {
    expect(usedPercent(cap())).toBe(25);
    expect(diskUsed(cap())).toBe(250);
    expect(usedPercent(cap({ disk_total: null, disk_free: null }))).toBeNull();
    expect(diskUsed(cap({ disk_free: null }))).toBeNull();
    expect(usedPercent(cap({ disk_total: 0, disk_free: 0 }))).toBeNull();
    // Stored: the content store once, and folder spaces' files and versions; not the logical sizes
    expect(storedBytes(cap())).toBe(310);
  });

  it("gives no error rate or latency without operations", () => {
    expect(errorRate(figures(0))).toBeNull();
    expect(errorRate(figures(200, 3, 1))).toBe(2);
    expect(sumFigures([])).toBeNull();
    const both = sumFigures([figures(10, 1), { ...figures(30), p95_ms: 40, mean_ms: 5 }])!;
    expect([both.count, both.errors, both.bytes, both.p95_ms]).toEqual([40, 1, 400, 40]);
    expect(both.mean_ms).toBeCloseTo(4.5);
    expect(perSecond(3000, 300)).toBe(10);
  });

  it("makes round axis values", () => {
    expect(niceTicks(0)).toEqual([0, 1]);
    expect(niceTicks(87)).toEqual([0, 50, 100]);
    expect(niceTicks(9)).toEqual([0, 5, 10]);
    expect(niceTicks(0.37)).toEqual([0, 0.1, 0.2, 0.3, 0.4]);
    // Counts are whole numbers, also when there are only one or two
    expect(countTicks(2)).toEqual([0, 1, 2]);
    expect(countTicks(0)).toEqual([0, 1]);
    // Bytes step by whole KB, MB, GB…
    expect(byteTicks(3 * 1024 ** 3)).toEqual([0, 1, 2, 3].map((v) => v * 1024 ** 3));
    expect(byteTicks(700)).toEqual([0, 200, 400, 600, 800]);
  });

  it("aligns series by time and breaks lines over idle periods", () => {
    const a = [{ at: 0 }, { at: 300 }, { at: 1200 }];
    const b = [{ at: 300 }];
    const rows = alignSeries([a, b], 300);
    expect(rows.map((r) => [r.at, r.points.map((p) => (p ? p.at : null))])).toEqual([
      [0, [0, null]],
      [300, [300, 300]],
      // An idle stretch: a gap, not a line from 300 to 1200
      [600, [null, null]],
      [1200, [1200, null]],
    ]);
  });

  it("exports the history as CSV with unknown values left empty", () => {
    const h: UsageHistory = {
      location: "local",
      range: "2d",
      work: "foreground",
      from: 0,
      to: 1000,
      capacity_span: 900,
      ops_span: 300,
      capacity: [{ ...cap({ disk_free: null, disk_total: null }), at: 0 }],
      ops: [{ op: "read", points: [{ at: 300, ...figures(4, 1), p50_ms: null }] }],
      forecast: { days: null, reason: "short", window_days: 0, points: 0 },
    };
    const lines = historyCsv(h).trim().split("\n");
    expect(lines[0]).toMatch(/^period_start_utc,period_seconds,live_bytes/);
    expect(lines[1]).toBe("1970-01-01T00:00:00.000Z,900,600,50,30,310,,,0,true");
    expect(lines[4]).toBe("1970-01-01T00:05:00.000Z,300,read,foreground,4,1,0,40,,9,12");
  });
});
