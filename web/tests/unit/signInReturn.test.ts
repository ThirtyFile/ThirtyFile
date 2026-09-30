// Where the sign-in page goes afterwards, and the reason a sign-in with another account failed (lib/signInReturn.ts)
import { beforeEach, describe, expect, test, vi } from "vitest";
import { safeNext, takeSsoError } from "@/lib/signInReturn";

describe("the page after signing in", () => {
  const origin = "https://files.example.com";
  test("paths of this site are kept", () => {
    expect(safeNext("/files/abc?view=list#x", origin)).toBe("/files/abc?view=list#x");
    expect(safeNext("/shared", origin)).toBe("/shared");
  });

  test("anything that leads to another site, or isn't a path, goes to the files", () => {
    for (const next of [null, "", "files", "https://elsewhere.example/", "//elsewhere.example", "/\\elsewhere.example", "/\t/elsewhere.example", "/\n/elsewhere.example", "\t//elsewhere.example", "javascript:alert(1)"]) {
      expect(safeNext(next, origin)).toBe("/files");
    }
  });
});

describe("why a sign-in failed", () => {
  const clear = () => (document.cookie = "tf_sso_error=; Path=/; Max-Age=0");
  beforeEach(clear);

  test("the reason is what the server put in its cookie, read once", () => {
    vi.useFakeTimers();
    document.cookie = `tf_sso_error=${encodeURIComponent("This sign-in method isn't enabled")}; Path=/`;
    expect(takeSsoError(new URLSearchParams("sso_error=1"))).toBe("This sign-in method isn't enabled");
    expect(document.cookie).not.toContain("tf_sso_error=This");
    // The same page asking again right away gets the same answer; later, only the plain message
    expect(takeSsoError(new URLSearchParams("sso_error=1"))).toBe("This sign-in method isn't enabled");
    vi.advanceTimersByTime(5000);
    expect(takeSsoError(new URLSearchParams("sso_error=1"))).toBe("Sign-in failed");
    vi.useRealTimers();
  });

  test("text in the address is never shown", () => {
    const shown = takeSsoError(new URLSearchParams("sso_error=Your+account+is+locked.+Call+example.com"));
    expect(shown).toBe("Sign-in failed");
    expect(takeSsoError(new URLSearchParams(""))).toBeNull();
  });
});
