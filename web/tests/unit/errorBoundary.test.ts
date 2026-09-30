// A rendering error caught by the error boundary is shown, and reported to the error log (components/ErrorBoundary.tsx)
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { expect, test, vi } from "vitest";
import { ErrorBoundary } from "@/components/ErrorBoundary";
import type { ErrorReport } from "@/lib/errorReport";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function Exploding(): never {
  throw new Error("Controlled rendering failure");
}

test("a rendering error is shown and reported as a page error, and the page stays usable", async () => {
  const sent: ErrorReport[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn<typeof fetch>((_url, init) => {
      sent.push(JSON.parse(String(init?.body)));
      return Promise.resolve(new Response("{}", { status: 202 }));
    }),
  );
  vi.spyOn(console, "error").mockImplementation(() => {});
  const el = document.createElement("div");
  const root = createRoot(el);
  await act(async () => root.render(createElement(ErrorBoundary, null, createElement(Exploding))));
  expect(el.textContent).toContain("Something went wrong on this page");
  await vi.waitFor(() => expect(sent).toHaveLength(1));
  expect(sent[0]).toMatchObject({ kind: "render", message: "Controlled rendering failure" });
  expect(sent[0].stack).toContain("Exploding");
  await act(async () => root.unmount());
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});
