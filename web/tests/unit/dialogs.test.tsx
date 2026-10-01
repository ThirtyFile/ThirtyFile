// The simple dialogs (components/dialogs.tsx) and the questions asked with confirm(): what they send, what they show
// while busy, and the error that stays when an action fails
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, test, vi } from "vitest";
import { ConfirmDialog, NameDialog } from "@/components/dialogs";
import { ConfirmHost } from "@/components/confirm";
import { confirm } from "@/lib/confirm";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
function mount(element: React.ReactNode) {
  const el = document.createElement("div");
  document.body.append(el);
  root = createRoot(el);
  act(() => root!.render(element));
}
afterEach(() => {
  act(() => root?.unmount());
  root = null;
  document.body.replaceChildren();
});

/** Dialogs open in a portal: they are found in the whole page */
const dialog = () => document.querySelector<HTMLElement>('[role="dialog"], [role="alertdialog"]');
const button = (name: string) => [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === name)!;
const settle = () => act(async () => void (await new Promise((r) => setTimeout(r, 20))));
/** Types into a controlled input the way React notices */
function type(input: HTMLInputElement, text: string) {
  const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
  act(() => {
    set.call(input, text);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

describe("dialogs", () => {
  test("a name dialog sends the name without surrounding spaces, and shows why it was refused", async () => {
    let refuse = true;
    const onSubmit = vi.fn<(name: string) => Promise<void>>(() => (refuse ? Promise.reject(new Error("A file with this name already exists")) : Promise.resolve()));
    mount(<NameDialog title="New folder" initial="New folder" onSubmit={onSubmit} onClose={() => {}} />);
    await settle();
    expect(dialog()?.textContent).toContain("New folder");
    const input = dialog()!.querySelector("input")!;
    type(input, "  Reports  ");
    act(() => void input.form!.requestSubmit());
    await settle();
    expect(onSubmit).toHaveBeenLastCalledWith("Reports");
    const alert = document.querySelector('[role="alert"]');
    expect(alert?.textContent).toBe("A file with this name already exists");
    expect(input.getAttribute("aria-invalid")).toBe("true");

    refuse = false;
    act(() => void input.form!.requestSubmit());
    await settle();
    expect(document.querySelector('[role="alert"]')).toBeNull();
  });

  test("a confirmation runs the action once, and the dialog stays open with the error when it fails", async () => {
    const onConfirm = vi.fn<() => Promise<void>>(() => Promise.reject(new Error("The item is locked")));
    const onClose = vi.fn<() => void>();
    mount(<ConfirmDialog title="Delete for good?" confirmText="Delete" destructive onConfirm={onConfirm} onClose={onClose} />);
    await settle();
    act(() => button("Delete").click());
    await settle();
    expect(onConfirm).toHaveBeenCalledTimes(1);
    expect(document.querySelector('[role="alert"]')?.textContent).toBe("The item is locked");
    expect(onClose).not.toHaveBeenCalled();
  });

  test("confirm() asks through the host, and a second question answers the first with no", async () => {
    mount(<ConfirmHost />);
    const first = confirm({ title: "Discard unsaved changes?", confirmText: "Discard" });
    await settle();
    expect(dialog()?.textContent).toContain("Discard unsaved changes?");
    const second = confirm({ title: "Close 2 tabs?", confirmText: "Close" });
    await expect(first).resolves.toBe(false);
    await settle();
    act(() => button("Close").click());
    await expect(second).resolves.toBe(true);
    await settle();
    expect(dialog()).toBeNull();
  });
});
