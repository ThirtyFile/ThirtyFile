// Control panel › Dashboard: the banner, the alerts, the numbers, the charts' periods and the latest errors
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, describe, expect, test, vi } from "vite-plus/test";
import type { DashboardInfo, DashboardSeries } from "@/api";
import { DashboardPage, periods, values } from "@/admin/system/DashboardPage";

vi.mock("sonner", () => ({ toast: { error: vi.fn<(message: string) => void>(), success: vi.fn<(message: string) => void>() } }));
vi.mock("@/admin/SettingsFrame", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/admin/SettingsFrame")>()),
  SettingsFrame: ({ children }: { children: ReactNode }) => <main>{children}</main>,
}));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const NOW = 1_800_000_000;
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
const NAMES: DashboardSeries[] = ["requests", "errors", "latency_p95_ms", "sessions", "tasks_running", "uploads", "storage_ops", "storage_errors", "failed_sign_ins", "disk_free_percent"];

function info(over: Partial<DashboardInfo> = {}): DashboardInfo {
  const series = Object.fromEntries(NAMES.map((n) => [n, [] as [number, number][]])) as DashboardInfo["series"];
  return {
    now: NOW,
    span: 86400,
    step: 300,
    from: NOW - 86400,
    status: "ok",
    warnings: [],
    alerts: [],
    numbers: { sessions: 3, active_accounts: 7, tasks_running: 1, uploads: 0, disk_free_percent: 42.4, disk_free_bytes: 5 * 1024 ** 3 },
    series,
    errors: [],
    failed_sign_ins: [],
    monitoring: false,
    ...over,
  };
}

let root: Root | null = null;
let host: HTMLElement | undefined;
let asked: string[];

async function mount(answer: (span: number) => DashboardInfo) {
  asked = [];
  vi.stubGlobal(
    "fetch",
    vi.fn<typeof fetch>(async (input) => {
      const url = String(input);
      if (url.includes("/api/admin/dashboard")) {
        asked.push(url);
        return json(answer(Number(new URL(url, "http://x").searchParams.get("span"))));
      }
      return json({}, 404);
    }),
  );
  const el = document.createElement("div");
  document.body.append(el);
  host = el;
  root = createRoot(el);
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  await act(async () => {
    root!.render(
      <QueryClientProvider client={qc}>
        <MemoryRouter>
          <DashboardPage />
        </MemoryRouter>
      </QueryClientProvider>,
    );
  });
  await act(async () => {
    await vi.waitFor(() => expect(text()).toContain("Signed in now"));
  });
}

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = null;
  vi.unstubAllGlobals();
});

const text = () => host?.textContent ?? "";

describe("the periods of the charts", () => {
  test("start on a period boundary and run to now", () => {
    const i = info({ from: NOW - 86400 + 120, step: 300 });
    const times = periods(i);
    expect(times[0]).toBe(Math.floor(i.from / 300) * 300);
    expect(times.at(-1)).toBeLessThanOrEqual(NOW);
    expect(times.every((t, k) => k === 0 || t - times[k - 1] === 300)).toBe(true);
  });

  test("a figure is where its period is, and null where the server wrote none", () => {
    const i = info();
    const times = periods(i);
    i.series.requests = [
      [times[2], 10],
      [times[3], 0],
    ];
    const v = values(i, times, "requests");
    expect([v[0], v[1], v[2], v[3], v[4]]).toEqual([null, null, 10, 0, null]);
    expect(v).toHaveLength(times.length);
  });

  test("a week has half hours", () => {
    const week = info({ span: 7 * 86400, step: 1800, from: NOW - 7 * 86400 });
    const times = periods(week);
    expect(times[1] - times[0]).toBe(1800);
    expect(times.length).toBeGreaterThan(300);
  });
});

describe("Dashboard", () => {
  test("says all is well, with the numbers", async () => {
    await mount(() => info());
    expect(text()).toContain("Everything is working");
    expect(text()).toContain("Signed in now");
    expect(text()).toContain("42%");
    expect(text()).toContain("5.0 GB");
    expect(text()).not.toContain("Alerts firing now");
    expect(text()).toContain("To read these figures in Prometheus or Grafana, turn on monitoring.");
  });

  test("says what is wrong, and what is firing", async () => {
    await mount(() =>
      info({
        status: "degraded",
        warnings: ["data: 200.0 MB free", "storage location nas is offline"],
        alerts: [{ kind: "offline", params: { location: "NAS" }, since: NOW - 3600 }],
        monitoring: true,
      }),
    );
    expect(text()).toContain("Something needs a look");
    expect(text()).toContain("data: 200.0 MB free");
    expect(text()).toContain("storage location nas is offline");
    expect(text()).toContain("The storage location “NAS” can't be reached");
    expect(text()).toContain("The same figures, and more, are at /metrics for Prometheus.");
  });

  test("a database that can't be read is said as such", async () => {
    await mount(() => info({ status: "error" }));
    expect(text()).toContain("The database can't be read");
  });

  test("lists the latest errors and failed sign-ins, and says none when there are none", async () => {
    await mount(() => info());
    expect([...host!.querySelectorAll('section[aria-label^="Latest"]')].map((s) => s.textContent)).toEqual([expect.stringContaining("None"), expect.stringContaining("None")]);
    act(() => root?.unmount());
    host?.remove();
    await mount(() =>
      info({
        errors: [{ at: NOW - 60, kind: "server", route: "POST /api/nodes/{id}/rename", status: 500, count: 3, message: "A server error occurred" }],
        failed_sign_ins: [{ at: NOW - 30, event: "bad_password", username: "amy", ip: "203.0.113.9" }],
      }),
    );
    expect(text()).toContain("A server error occurred");
    expect(text()).toContain("POST /api/nodes/{id}/rename · 3 times");
    expect(text()).toContain("amy");
    expect(text()).toContain("bad_password · 203.0.113.9");
  });

  test("asks for a day first and for a week when told", async () => {
    await mount((span) => info({ span, step: span > 86400 ? 1800 : 300 }));
    expect(asked[0]).toContain("span=86400");
    const week = [...host!.querySelectorAll("button")].find((b) => b.textContent === "Last 7 days")!;
    expect(week.getAttribute("aria-pressed")).toBe("false");
    await act(async () => week.click());
    await act(async () => {
      await vi.waitFor(() => expect(asked.some((u) => u.includes("span=604800"))).toBe(true));
    });
    expect([...host!.querySelectorAll("button")].find((b) => b.textContent === "Last 7 days")!.getAttribute("aria-pressed")).toBe("true");
  });

  test("draws a chart for each kind of figure", async () => {
    const i = info();
    const times = periods(i);
    i.series.requests = times.slice(-5).map((t, k) => [t, 10 * (k + 1)]);
    await mount(() => i);
    for (const title of ["Requests", "Time to answer", "People and work", "Storage operations", "Failed sign-ins", "Free disk space"]) {
      expect(text().includes(title)).toBe(true);
    }
  });
});
