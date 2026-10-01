// The link from the version an administrator sees to that release's notes (lib/version.ts)
import { expect, test } from "vitest";
import { releaseNotesUrl } from "@/lib/version";

test("a release links to its notes on GitHub", () => {
  expect(releaseNotesUrl("0.5.0")).toBe("https://github.com/ThirtyFile/ThirtyFile/releases/tag/v0.5.0");
  expect(releaseNotesUrl("1.2.3-rc.1")).toBe("https://github.com/ThirtyFile/ThirtyFile/releases/tag/v1.2.3-rc.1");
});

test("a local build, or one named some other way, has no notes to link to", () => {
  const named = ["dev", "", "1.2", "v1.2.3", "1.2.3/../../evil", "1.2.3 ", "main"];
  expect(named.map(releaseNotesUrl)).toEqual(named.map(() => null));
});
