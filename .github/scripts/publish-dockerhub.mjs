import { spawnSync } from "node:child_process";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

const source = "ghcr.io/thirtyfile/thirtyfile";
const target = "docker.io/thirtyfile/thirtyfile";
const repository = "ThirtyFile/ThirtyFile";
const digestPattern = /^sha256:[0-9a-f]{64}$/;

export function imageTags(version) {
  if (!/^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$/.test(version)) {
    throw new Error("Use a version such as 0.6.0 or 0.6.0-rc.1, without v");
  }
  if (version.includes("-")) return [version];
  const major = version.split(".")[0];
  return [version, version.slice(0, version.lastIndexOf(".")), ...(major === "0" ? [] : [major]), "latest"];
}

function runCommand(command, args) {
  const result = spawnSync(command, args, { encoding: "utf8" });
  if (result.error) throw result.error;
  return result;
}

function commandOutput(run, command, args) {
  const result = run(command, args);
  if (result.status !== 0) throw new Error(`${command} failed: ${result.stderr || result.stdout}`);
  return result.stdout.trim();
}

function inspect(run, reference, allowMissing = false) {
  const result = run("docker", ["buildx", "imagetools", "inspect", reference, "--format", "{{json .Manifest}}"]);
  if (result.status !== 0) {
    const message = `${result.stderr || ""}\n${result.stdout || ""}`;
    if (allowMissing && !/unauthorized|denied|too many requests|rate.?limit/i.test(message) && /\bnot found\b|\bmanifest unknown\b|\bMANIFEST_UNKNOWN\b/i.test(message)) return null;
    throw new Error(`Cannot inspect ${reference}: ${message.trim()}`);
  }
  const manifest = JSON.parse(result.stdout);
  if (!digestPattern.test(manifest.digest)) throw new Error(`Invalid image digest: ${reference}`);
  return manifest;
}

function requirePlatforms(manifest) {
  for (const architecture of ["amd64", "arm64"]) {
    const matches = manifest.manifests?.filter((entry) => entry.platform?.os === "linux" && entry.platform.architecture === architecture);
    if (matches?.length !== 1 || !digestPattern.test(matches[0].digest)) throw new Error(`Expected one linux/${architecture} image`);
  }
}

export function publishDockerHub({ version, digest, released = false, run = runCommand }) {
  const tags = imageTags(version);
  if (!digestPattern.test(digest)) throw new Error("A valid source image digest is required");
  const image = inspect(run, `${source}@${digest}`);
  if (image.digest !== digest) throw new Error("The source image digest changed");
  requirePlatforms(image);

  // A backfill only follows floating tags that still name this release on GHCR.
  const selectedTags = [version];
  for (const tag of tags.slice(1)) {
    const current = inspect(run, `${source}:${tag}`, released);
    if (current?.digest === digest) selectedTags.push(tag);
    else if (!released) throw new Error(`GHCR tag ${tag} does not name the release image`);
  }

  const exact = `${target}:${version}`;
  const existing = inspect(run, exact, true);
  if (existing && existing.digest !== digest) throw new Error(`Refusing to overwrite the existing version ${exact}`);
  if (!existing) commandOutput(run, "docker", ["buildx", "imagetools", "create", "--prefer-index=true", "--tag", exact, `${source}@${digest}`]);
  if (inspect(run, exact).digest !== digest) throw new Error(`Copied image does not match the source: ${exact}`);

  if (selectedTags.length > 1) {
    commandOutput(run, "docker", ["buildx", "imagetools", "create", "--prefer-index=true", ...selectedTags.slice(1).flatMap((tag) => ["--tag", `${target}:${tag}`]), `${source}@${digest}`]);
  }
  for (const tag of selectedTags.slice(1)) {
    if (inspect(run, `${target}:${tag}`).digest !== digest) throw new Error(`Copied image does not match the source: ${target}:${tag}`);
  }
  return { digest, tags: selectedTags };
}

export function mirrorRelease({ version, run = runCommand }) {
  imageTags(version);
  const release = JSON.parse(commandOutput(run, "gh", ["release", "view", `v${version}`, "--repo", repository, "--json", "tagName,isDraft,isPrerelease"]));
  if (release.tagName !== `v${version}` || release.isDraft || release.isPrerelease !== version.includes("-")) throw new Error("The version must identify a published release");
  let object = JSON.parse(commandOutput(run, "gh", ["api", `repos/${repository}/git/ref/tags/v${version}`])).object;
  for (let depth = 0; object?.type === "tag" && depth < 5; depth++) {
    if (!/^[0-9a-f]{40}$/.test(object.sha)) throw new Error("Invalid annotated release tag");
    object = JSON.parse(commandOutput(run, "gh", ["api", `repos/${repository}/git/tags/${object.sha}`])).object;
  }
  if (object?.type !== "commit" || !/^[0-9a-f]{40}$/.test(object.sha)) throw new Error("The release tag has no valid commit");
  const commit = object.sha;
  const image = inspect(run, `${source}:${version}`);
  requirePlatforms(image);
  const platform = image.manifests.find((entry) => entry.platform?.os === "linux" && entry.platform.architecture === "amd64").digest;
  commandOutput(run, "docker", ["pull", "--quiet", `${source}@${platform}`]);
  const revision = commandOutput(run, "docker", ["image", "inspect", "--format", '{{index .Config.Labels "org.opencontainers.image.revision"}}', `${source}@${platform}`]);
  if (revision !== commit) throw new Error("The image revision does not match the released Git tag");
  commandOutput(run, "gh", [
    "attestation",
    "verify",
    `oci://${source}@${image.digest}`,
    "--repo",
    repository,
    "--signer-workflow",
    `${repository}/.github/workflows/release.yml`,
    "--source-digest",
    commit,
  ]);
  return publishDockerHub({ version, digest: image.digest, released: true, run });
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    const options = { version: process.env.VERSION, digest: process.env.DIGEST };
    const result = process.argv[2] === "--released" ? mirrorRelease(options) : publishDockerHub(options);
    console.log(`Docker Hub tags ${result.tags.join(", ")} match ${result.digest}`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
