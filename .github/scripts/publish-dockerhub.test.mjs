import assert from "node:assert/strict";
import test from "node:test";
import { imageTags, mirrorRelease, publishDockerHub } from "./publish-dockerhub.mjs";

const digest = `sha256:${"a".repeat(64)}`;
const otherDigest = `sha256:${"b".repeat(64)}`;
const source = "ghcr.io/thirtyfile/thirtyfile";
const target = "docker.io/thirtyfile/thirtyfile";
const commit = "c".repeat(40);

function registry({
  version = "0.6.0",
  exact,
  floats = {},
  failure,
  copiedDigest = digest,
  platforms = ["amd64", "arm64"],
  draft = false,
  revision = commit,
  attestationFailure = false,
  copyFailure = false,
  annotated = false,
} = {}) {
  const images = new Map([
    [`${source}@${digest}`, digest],
    [`${source}:${version}`, digest],
    ...imageTags(version)
      .slice(1)
      .map((tag) => [`${source}:${tag}`, floats[tag] ?? digest]),
  ]);
  if (exact) images.set(`${target}:${version}`, exact);
  const writes = [];
  const calls = [];
  function run(command, args) {
    calls.push([command, ...args]);
    if (command === "gh") {
      if (args[0] === "release") return { status: 0, stdout: JSON.stringify({ tagName: `v${version}`, isDraft: draft, isPrerelease: version.includes("-") }) };
      if (args[0] === "api") {
        const type = annotated && args[1].includes("/git/ref/") ? "tag" : "commit";
        return { status: 0, stdout: JSON.stringify({ object: { type, sha: commit } }) };
      }
      if (args[0] === "attestation") return { status: attestationFailure ? 1 : 0, stdout: "", stderr: "invalid provenance" };
    }
    if (command === "docker" && args[0] === "pull") return { status: 0, stdout: "" };
    if (command === "docker" && args[0] === "image") return { status: 0, stdout: revision };
    if (args[2] === "inspect") {
      if (args[3] === failure?.reference) return { status: 1, stdout: "", stderr: failure.message };
      const value = images.get(args[3]);
      if (!value) return { status: 1, stdout: "", stderr: "manifest unknown" };
      return {
        status: 0,
        stdout: JSON.stringify({
          digest: value,
          manifests: [
            ...platforms.map((architecture) => ({ digest: otherDigest, platform: { os: "linux", architecture } })),
            { digest: otherDigest, platform: { os: "unknown", architecture: "unknown" } },
          ],
        }),
      };
    }
    if (args[2] === "create") {
      if (copyFailure) return { status: 1, stdout: "", stderr: "connection interrupted" };
      assert.equal(args.at(-1), `${source}@${digest}`, "copies are pinned to the verified image index");
      for (let index = 0; index < args.length; index++) {
        if (args[index] === "--tag") {
          images.set(args[index + 1], copiedDigest);
          writes.push(args[index + 1]);
        }
      }
      return { status: 0, stdout: "" };
    }
    throw new Error(`Unexpected command: ${command} ${args.join(" ")}`);
  }
  return { run, writes, calls };
}

test("stable, major and pre-release tag policies", () => {
  assert.deepEqual(imageTags("0.6.0"), ["0.6.0", "0.6", "latest"]);
  assert.deepEqual(imageTags("1.2.3"), ["1.2.3", "1.2", "1", "latest"]);
  assert.deepEqual(imageTags("1.2.3-rc.1"), ["1.2.3-rc.1"]);
  for (const invalid of ["v0.6.0", "0.06.0", "0.6.0; echo bad", "latest", "", undefined]) assert.throws(() => imageTags(invalid));
});

test("publishes and verifies the same multi-platform index with stable tags", () => {
  const fake = registry();
  assert.deepEqual(publishDockerHub({ version: "0.6.0", digest, run: fake.run }).tags, ["0.6.0", "0.6", "latest"]);
  assert.deepEqual(
    fake.writes,
    ["0.6.0", "0.6", "latest"].map((tag) => `${target}:${tag}`),
  );
});

test("pre-releases never update floating tags", () => {
  const fake = registry({ version: "0.6.0-rc.1" });
  publishDockerHub({ version: "0.6.0-rc.1", digest, run: fake.run });
  assert.deepEqual(fake.writes, [`${target}:0.6.0-rc.1`]);
});

test("retries preserve an existing matching exact-version image", () => {
  const fake = registry({ exact: digest });
  publishDockerHub({ version: "0.6.0", digest, run: fake.run });
  assert.deepEqual(fake.writes, [`${target}:0.6`, `${target}:latest`]);
});

test("an immutable-version conflict aborts before any writes", () => {
  const fake = registry({ exact: otherDigest });
  assert.throws(() => publishDockerHub({ version: "0.6.0", digest, run: fake.run }), /Refusing to overwrite/);
  assert.deepEqual(fake.writes, []);
});

test("a mismatched copy never advances floating tags", () => {
  const fake = registry({ copiedDigest: otherDigest });
  assert.throws(() => publishDockerHub({ version: "0.6.0", digest, run: fake.run }), /does not match/);
  assert.deepEqual(fake.writes, [`${target}:0.6.0`]);
});

test("authentication, rate-limit and network failures cannot be mistaken for missing tags", () => {
  for (const message of ["unauthorized: repository not found", "429 Too Many Requests", "connection timed out", "500 Internal Server Error"]) {
    const fake = registry({ failure: { reference: `${target}:0.6.0`, message } });
    assert.throws(() => publishDockerHub({ version: "0.6.0", digest, run: fake.run }), /Cannot inspect/);
    assert.deepEqual(fake.writes, []);
  }
});

test("missing architectures and invalid source digests abort before writes", () => {
  const fake = registry({ platforms: ["amd64"] });
  assert.throws(() => publishDockerHub({ version: "0.6.0", digest, run: fake.run }), /linux\/arm64/);
  assert.throws(() => publishDockerHub({ version: "0.6.0", digest: "invalid", run: fake.run }), /valid source/);
  assert.deepEqual(fake.writes, []);
});

test("backfills cannot roll back newer release-line or latest tags", () => {
  const fake = registry({ floats: { 0.6: otherDigest, latest: otherDigest } });
  const result = publishDockerHub({ version: "0.6.0", digest, released: true, run: fake.run });
  assert.deepEqual(result.tags, ["0.6.0"]);
  assert.deepEqual(fake.writes, [`${target}:0.6.0`]);
});

test("a release cannot silently omit a changed floating tag", () => {
  const fake = registry({ floats: { latest: otherDigest } });
  assert.throws(() => publishDockerHub({ version: "0.6.0", digest, run: fake.run }), /GHCR tag latest/);
  assert.deepEqual(fake.writes, []);
});

test("manual mirroring verifies released commit and signed provenance before copying", () => {
  const fake = registry();
  mirrorRelease({ version: "0.6.0", run: fake.run });
  const attestation = fake.calls.findIndex((call) => call[0] === "gh" && call[1] === "attestation");
  const copy = fake.calls.findIndex((call) => call[3] === "create");
  assert.ok(attestation >= 0 && copy > attestation);
  assert.ok(fake.calls[attestation].includes(commit));
});

test("drafts, wrong revisions and invalid provenance cannot be mirrored", () => {
  for (const options of [{ draft: true }, { revision: "d".repeat(40) }, { attestationFailure: true }]) {
    const fake = registry(options);
    assert.throws(() => mirrorRelease({ version: "0.6.0", run: fake.run }));
    assert.deepEqual(fake.writes, []);
  }
});

test("a failed registry copy cannot advance floating tags", () => {
  const fake = registry({ copyFailure: true });
  assert.throws(() => publishDockerHub({ version: "0.6.0", digest, run: fake.run }), /connection interrupted/);
  assert.deepEqual(fake.writes, []);
});

test("annotated release tags resolve to their commit before provenance verification", () => {
  const fake = registry({ annotated: true });
  mirrorRelease({ version: "0.6.0", run: fake.run });
  assert.ok(fake.calls.some((call) => call[0] === "gh" && call[2].includes(`/git/tags/${commit}`)));
});
