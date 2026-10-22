// Secrets hidden until shown: a typed password, a private key of several lines, and a password shown once
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, test } from "vite-plus/test";
import { SecretInput, SecretTextarea } from "@/components/SecretField";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root;
beforeEach(() => {
  const host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(() => {
  act(() => root.unmount());
  document.body.replaceChildren();
});

const show = (el: React.ReactNode) => act(() => root.render(el));
const reveal = (name: string) => document.querySelector<HTMLButtonElement>(`button[aria-label="${name}"]`)!;

test("a password is dots until Show is pressed, and again after Hide; the button says whether it is shown", () => {
  show(<SecretInput id="pw" revealLabel="Show password" defaultValue="a-test-pass-1" autoComplete="new-password" />);
  const field = document.querySelector<HTMLInputElement>("#pw")!;
  const button = reveal("Show password");
  expect(field.type).toBe("password");
  expect(field.autocomplete).toBe("new-password");
  expect(button.getAttribute("aria-pressed")).toBe("false");
  expect(button.getAttribute("aria-controls")).toBe("pw");
  expect(button.type).toBe("button");
  act(() => button.click());
  expect(field.type).toBe("text");
  expect(field.value).toBe("a-test-pass-1");
  expect(button.getAttribute("aria-pressed")).toBe("true");
  act(() => button.click());
  expect(field.type).toBe("password");
});

test("shown anew (a dialog opened again, another item), a secret is hidden again", () => {
  show(<SecretInput key="a" readOnly value="shown-once-1" revealLabel="Show app password" aria-label="App password" />);
  act(() => reveal("Show app password").click());
  expect(document.querySelector("input")!.type).toBe("text");
  show(<SecretInput key="b" readOnly value="shown-once-2" revealLabel="Show app password" aria-label="App password" />);
  expect(document.querySelector("input")!.type).toBe("password");
  expect(document.querySelector("input")!.value).toBe("shown-once-2");
});

test("a field that can't be used can't be shown either", () => {
  show(<SecretInput id="pw" revealLabel="Show password" disabled />);
  expect(reveal("Show password").disabled).toBe(true);
});

test("a private key is hidden but kept as it is, line breaks and all, and Show draws it as text", () => {
  const key = "-----BEGIN OPENSSH PRIVATE KEY-----\nexample-test-key\n-----END OPENSSH PRIVATE KEY-----";
  show(<SecretTextarea id="key" revealLabel="Show private key" defaultValue={key} />);
  const area = document.querySelector<HTMLTextAreaElement>("#key")!;
  expect(area.value).toBe(key);
  expect(area.classList.contains("tf-secret-text")).toBe(true);
  act(() => reveal("Show private key").click());
  expect(area.classList.contains("tf-secret-text")).toBe(false);
  expect(area.value).toBe(key);
  expect(reveal("Show private key").getAttribute("aria-pressed")).toBe("true");
});
