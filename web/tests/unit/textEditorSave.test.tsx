import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import TextEditor from "@/components/TextEditor";
import { ApiError, type FileSource, type Node } from "@/api";
import { toast } from "sonner";
import { getDraft, setDraft } from "@/lib/drafts";

const mock = vi.hoisted(() => ({ save: vi.fn<typeof import("@/api").api.saveContent>(), fetch: vi.fn<typeof import("@/api").fetchOk>(), change: (_value: string) => {} }));
vi.mock("@/api", async (original) => ({ ...(await original<typeof import("@/api")>()), api: { saveContent: mock.save }, fetchOk: mock.fetch }));
vi.mock("@/lib/theme", () => ({ useTheme: () => ({ dark: false }) }));
vi.mock("@/lib/errorReport", () => ({ reportShown: vi.fn<() => void>() }));
vi.mock("sonner", () => ({ toast: { success: vi.fn<() => void>(), error: vi.fn<() => void>(), warning: vi.fn<() => void>() } }));
vi.mock("@uiw/react-codemirror", () => ({
  default: (props: { value: string; onChange(value: string): void }) => {
    mock.change = props.onChange;
    return <div data-text>{props.value}</div>;
  },
}));
vi.mock("@codemirror/language-data", () => ({ languages: [] }));
vi.mock("@/components/ui/button", () => ({
  Button: (props: { children: ReactNode; onClick?(): void; disabled?: boolean }) => (
    <button onClick={props.onClick} disabled={props.disabled}>
      {props.children}
    </button>
  ),
}));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const node = (id: string): Node => ({
  id,
  name: `${id}.txt`,
  kind: "file",
  size: 10,
  mime: "text/plain",
  parent_id: "root",
  drive_id: "drive",
  owner_name: "admin",
  created_at: 1,
  updated_at: 1,
  trashed_at: null,
  is_favorite: false,
});
const a = node("save-a"),
  b = node("save-b");
const source = { contentUrl: (n: Node) => n.id } as FileSource;
let root: Root;
let el: HTMLDivElement;
let finish: (n: Node) => void;
const render = async (n: Node) => act(async () => root.render(<TextEditor node={n} source={source} editable embedded />));
const save = async () => {
  await act(async () => mock.change("edited A"));
  await act(async () => el.querySelector("button")!.click());
};

beforeEach(() => {
  vi.clearAllMocks();
  mock.fetch.mockImplementation(async (id: string) => new Response(id === a.id ? "original A" : "original B", { headers: { "x-version": "1" } }));
  mock.save.mockImplementation(
    () =>
      new Promise<Node>((resolve) => {
        finish = resolve;
      }),
  );
  el = document.createElement("div");
  document.body.append(el);
  root = createRoot(el);
});
afterEach(async () => {
  await act(async () => root.unmount());
  el.remove();
  setDraft(a.id, null);
  setDraft(b.id, null);
});

test("a delayed save cannot put the newly displayed file into its draft or dirty state", async () => {
  await render(a);
  await save();
  await render(b);
  await act(async () => finish({ ...a, updated_at: 2 }));
  expect(getDraft(a.id, "text")).toBeUndefined();
  expect(getDraft(b.id, "text")).toBeUndefined();
  expect(el.querySelector("[data-text]")?.textContent).toBe("original B");
  expect(el.textContent).not.toContain("Unsaved changes");
});

test("same-file edits typed during a save retain their new base and version", async () => {
  await render(a);
  await save();
  await act(async () => mock.change("edited again"));
  await act(async () => finish({ ...a, updated_at: 2 }));
  expect(getDraft(a.id, "text")).toEqual({ kind: "text", base: "edited A", text: "edited again", version: 2 });
});

test("a save finishing after unmount reconciles the draft of a reopened editor", async () => {
  await render(a);
  await save();
  await act(async () => root.render(null));
  await render(a);
  await act(async () => mock.change("reopened edits"));
  await act(async () => finish({ ...a, updated_at: 2 }));
  expect(getDraft(a.id, "text")).toEqual({ kind: "text", base: "edited A", text: "reopened edits", version: 2 });
});

test("a save finishing after discard does not resurrect the discarded draft", async () => {
  await render(a);
  await save();
  await act(async () => root.render(null));
  setDraft(a.id, null);
  await act(async () => finish({ ...a, updated_at: 2 }));
  expect(getDraft(a.id, "text")).toBeUndefined();
});

test("a stale conflict action cannot discard edits after leaving its editor", async () => {
  mock.save.mockRejectedValueOnce(new ApiError("Changed by someone else", 409));
  await render(a);
  await save();
  const options = vi.mocked(toast.error).mock.calls[0][1];
  const action = options?.action;
  if (!action || typeof action !== "object" || !("onClick" in action) || typeof action.onClick !== "function") throw new Error("No reload action");
  await render(b);
  const newer = { kind: "text" as const, text: "newer draft", base: "new base", version: 3 };
  setDraft(a.id, newer);
  await act(async () => {
    Reflect.apply(action.onClick, action, []);
  });
  expect(getDraft(a.id, "text")).toEqual(newer);
  expect(el.querySelector("[data-text]")?.textContent).toBe("original B");
});
