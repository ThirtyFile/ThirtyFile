// The same thing is called the same everywhere: the interface, the server's messages, the guides and the README. Old
// names that crept back in fail here.
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, test } from "vitest";
import { ZH } from "@/lib/i18n/zh-TW";

/** The tests run in web/ */
const repo = join(process.cwd(), "..");

function files(dir: string, ext: RegExp, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) files(p, ext, out);
    else if (ext.test(name)) out.push(p);
  }
  return out;
}

/** Every place with text people read: the interface's English, the server, the guides and the README */
const sources = () => [
  ...files(join(repo, "web", "src"), /\.tsx?$/).filter((f) => !f.includes(join("i18n", "zh-TW"))),
  ...files(join(repo, "server", "src"), /\.rs$/),
  ...files(join(repo, "site"), /\.html$/),
  join(repo, "README.md"),
];

/** Where `pattern` appears, as "file: line" */
function find(pattern: RegExp): string[] {
  const hits: string[] = [];
  for (const f of sources()) {
    readFileSync(f, "utf8")
      .split("\n")
      .forEach((line, i) => {
        if (pattern.test(line)) hits.push(`${f.slice(repo.length + 1)}:${i + 1}`);
      });
  }
  return hits;
}

describe("wording", () => {
  test("single sign-on is never listed without OpenID Connect", () => {
    // The guides may name the three and then the OpenID Connect providers, on the same line
    expect(find(/Microsoft, Google or GitHub(?!.*OpenID Connect)/)).toEqual([]);
  });

  test("two-factor sign-in is called that, not two-step verification", () => {
    expect(find(/two-step verification/i)).toEqual([]);
  });

  test("the Control panel's General page is called General", () => {
    expect(find(/General settings/)).toEqual([]);
  });

  test("an app password's access is named the same when it is chosen and when it is listed", () => {
    expect(find(/t\("Read and write"\)|t\("Read only"\)/)).toEqual([]);
  });

  test("the Traditional Chinese texts address people informally everywhere, never with the formal you (U+60A8)", () => {
    expect(Object.entries(ZH).filter(([, zh]) => zh.includes("您"))).toEqual([]);
  });

  test("a backup's next run isn't called the next page", () => {
    expect(ZH["run::Next"]).toBeTruthy();
    expect(ZH["run::Next"]).not.toBe(ZH["Next"]);
  });
});
