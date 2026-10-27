// The tools the interface is built with are written in more than one place; this fails when the places disagree.
//   - Vite+ is installed twice in web/pnpm-workspace.yaml: `vite-plus` (the command line) and `vite`, an alias of its core
//     package. They have to be the same release, and a tool that raises one version number does not know about the other
//     (Dependabot raised `vite-plus` alone).
//   - Node: `.node-version` is the one source for a contributor's machine (`vp env`) and for the workflows
//     (`node-version-file`). The Dockerfile's web stage is pinned by tag and digest, which Dependabot updates, so it
//     can't read the file: it must be the same major and minor release (the patch may be ahead). `engines.node` in
//     web/package.json must allow the version.
// Usage: node scripts/check-toolchain.mjs
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";

/** The problems with the catalog text of pnpm-workspace.yaml (none: an empty list) */
export function catalogProblems(text) {
  const plus = /^ {2}vite-plus: *["']?([\w.+-]+)["']?\s*$/m.exec(text)?.[1];
  const core = /^ {2}vite: *["']?npm:@voidzero-dev\/vite-plus-core@([\w.+-]+)["']?\s*$/m.exec(text)?.[1];
  if (!plus) return ["the catalog has no `vite-plus: <version>` line"];
  if (!core) return ["the catalog has no `vite: npm:@voidzero-dev/vite-plus-core@<version>` line"];
  if (plus !== core) {
    return [`\`vite-plus\` is ${plus} but \`vite\` (vite-plus-core) is ${core}: raise both, in the same pull request`];
  }
  return [];
}

const parts = (v) => v.split(".").map(Number);

/** Whether `version` is allowed by one clause of an `engines.node` range: `^M.m.p` or `>=M.m.p` */
function allowedByClause(version, clause) {
  const m = /^(\^|>=)(\d+\.\d+\.\d+)$/.exec(clause.trim());
  if (!m) return false;
  const [major, minor, patch] = parts(m[2]);
  const [a, b, c] = parts(version);
  const atLeast = a > major || (a === major && (b > minor || (b === minor && c >= patch)));
  return m[1] === ">=" ? atLeast : a === major && atLeast;
}

/**
 * The problems with the Node versions: `nodeVersion` is the text of .node-version, `dockerfile` and `engines` are the
 * Dockerfile's text and `engines.node`, and `workflows` maps a workflow's file name to its text
 */
export function nodeProblems({ nodeVersion, dockerfile, engines, workflows }) {
  const version = nodeVersion.trim();
  if (!/^\d+\.\d+\.\d+$/.test(version)) {
    return [`.node-version has "${version}": write one exact version such as 26.12.1`];
  }
  const problems = [];
  const image = /^FROM\s+(?:--\S+\s+)*node:(\d+\.\d+\.\d+)-/m.exec(dockerfile)?.[1];
  if (!image) {
    problems.push("the Dockerfile has no `FROM node:<version>-…` line");
  } else if (image.split(".").slice(0, 2).join(".") !== version.split(".").slice(0, 2).join(".")) {
    problems.push(`the Dockerfile's node image is ${image} but .node-version is ${version}: they must be the same major and minor release`);
  }
  if (!engines.split("||").some((clause) => allowedByClause(version, clause))) {
    problems.push(`.node-version is ${version} but engines.node in web/package.json is "${engines}"`);
  }
  for (const [name, text] of Object.entries(workflows)) {
    if (!text.includes("actions/setup-node")) continue;
    if (/^\s*node-version:/m.test(text)) problems.push(`.github/workflows/${name} sets node-version: use node-version-file: .node-version`);
    else if (!/^\s*node-version-file: \.node-version\s*$/m.test(text)) problems.push(`.github/workflows/${name} runs setup-node without node-version-file: .node-version`);
  }
  return problems;
}

if (process.argv[1]?.endsWith("check-toolchain.mjs")) {
  const at = (path) => fileURLToPath(new URL(path, import.meta.url));
  const workflowDir = at("../../.github/workflows/");
  const workflows = Object.fromEntries(
    readdirSync(workflowDir)
      .filter((f) => f.endsWith(".yml"))
      .map((f) => [f, readFileSync(workflowDir + f, "utf8")]),
  );
  const problems = [
    ...catalogProblems(readFileSync(at("../pnpm-workspace.yaml"), "utf8")).map((p) => `web/pnpm-workspace.yaml: ${p}`),
    ...nodeProblems({
      nodeVersion: readFileSync(at("../../.node-version"), "utf8"),
      dockerfile: readFileSync(at("../../Dockerfile"), "utf8"),
      engines: JSON.parse(readFileSync(at("../package.json"), "utf8")).engines.node,
      workflows,
    }),
  ];
  for (const p of problems) console.error(p);
  process.exit(problems.length ? 1 : 0);
}
