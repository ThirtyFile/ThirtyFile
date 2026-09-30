// The appearance is applied when the app loads, even when the inline script in index.html didn't run (lib/theme.ts)
import { beforeEach, expect, test, vi } from "vitest";

/** The module as a freshly loaded page has it, with the system in dark mode or not */
async function load(systemDark: boolean) {
  vi.resetModules();
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: systemDark && query.includes("dark"),
    addEventListener() {},
    removeEventListener() {},
  }));
  return import("@/lib/theme");
}

beforeEach(() => {
  localStorage.clear();
  document.documentElement.classList.remove("dark");
});

test("a chosen dark appearance is applied on load", async () => {
  localStorage.setItem("tf-theme", "dark");
  await load(false);
  expect(document.documentElement.classList.contains("dark")).toBe(true);
});

test("following the system is applied on load", async () => {
  await load(true);
  expect(document.documentElement.classList.contains("dark")).toBe(true);
});

test("a chosen light appearance stays light on a dark system", async () => {
  localStorage.setItem("tf-theme", "light");
  document.documentElement.classList.add("dark");
  await load(true);
  expect(document.documentElement.classList.contains("dark")).toBe(false);
});
