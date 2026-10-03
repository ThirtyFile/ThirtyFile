// Local Mac window preferences never reach another account or the Windows column store.
import { act, useLayoutEffect } from "react";
import { createRoot } from "react-dom/client";
import { expect, test } from "vitest";
import type { Me } from "@/api";
import { MeContext } from "@/lib/session";
import { StyleChoiceContext } from "@/lib/style";
import { columnShown, showColumn, useColumnPrefs, useColumnScope } from "@/lib/listView";
import { useMacWindowPrefs } from "@/components/style/mac/windowPrefs";
import { listColumns } from "@/components/fileList/columns";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

test("Mac bar and column settings follow the account; changing users and styles preserves each preference", () => {
  const el = document.createElement("div");
  document.body.append(el);
  const root = createRoot(el);
  let bars: ReturnType<typeof useMacWindowPrefs>;
  let scope = "";
  let owner = false;
  function Window() {
    const currentBars = useMacWindowPrefs();
    const currentScope = useColumnScope();
    const currentOwner = columnShown(useColumnPrefs(currentScope), "owner", currentScope.startsWith("tf-columns-mac-"));
    useLayoutEffect(() => {
      bars = currentBars;
      scope = currentScope;
      owner = currentOwner;
    });
    return null;
  }
  const render = (id: number, style: "mac" | "windows" = "mac") =>
    act(() =>
      root.render(
        <MeContext value={{ id } as Me}>
          <StyleChoiceContext value={style}>
            <Window />
          </StyleChoiceContext>
        </MeContext>,
      ),
    );
  try {
    render(4101);
    expect(bars![0]).toEqual({ path: false, status: false });
    expect(owner).toBe(false);
    act(() => {
      bars![1]({ path: true, status: true });
      showColumn("owner", true, scope);
    });
    expect(owner).toBe(true);
    render(4102);
    expect(bars![0]).toEqual({ path: false, status: false });
    expect(owner).toBe(false);
    render(4101);
    expect(bars![0]).toEqual({ path: true, status: true });
    expect(owner).toBe(true);
    render(4101, "windows");
    expect(scope).toBe("tf-columns");
    expect(owner).toBe(true);
    expect(localStorage.getItem("tf-columns")).toBeNull();
  } finally {
    act(() => root.unmount());
    el.remove();
    localStorage.clear();
  }
});

test("Mac default columns put Size before Kind and keep Uploaded by available", () => {
  const p = { showOwner: true };
  const prefs = { visible: {}, widths: {} };
  const columns = listColumns(p, true);
  expect(columns.filter((c) => columnShown(prefs, c.id, true)).map((c) => c.id)).toEqual(["date", "size", "type"]);
  expect(columns.some((c) => c.id === "owner")).toBe(true);
  expect(columnShown({ ...prefs, visible: { owner: true } }, "owner", true)).toBe(true);
  expect(
    listColumns(p)
      .filter((c) => columnShown(prefs, c.id))
      .map((c) => c.id),
  ).toEqual(["date", "type", "size", "owner"]);
});

test("a Mac public list can read its column preferences without a signed-in account", () => {
  const el = document.createElement("div");
  const root = createRoot(el);
  function PublicList() {
    const scope = useColumnScope();
    const prefs = useColumnPrefs(scope);
    return (
      <span>
        {scope}:{String(columnShown(prefs, "date", true))}
      </span>
    );
  }
  try {
    act(() =>
      root.render(
        <StyleChoiceContext value="mac">
          <PublicList />
        </StyleChoiceContext>,
      ),
    );
    expect(el.textContent).toBe("tf-columns-mac-visitor:true");
  } finally {
    act(() => root.unmount());
  }
});
