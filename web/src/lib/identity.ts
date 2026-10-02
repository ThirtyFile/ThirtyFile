/**
 * A file's complete content identity: a versioned SHA-256 of every byte, read a part at a time so memory stays bounded.
 * It works on self-hosted HTTP sites too, where WebCrypto isn't available. Used by the hashing worker
 * (identity.worker.ts), and by the page itself where a worker can't run.
 */
import { sha256 } from "@noble/hashes/sha2.js";

/** Bytes read at a time */
export const IDENTITY_CHUNK = 4 * 1024 * 1024;
export const IDENTITY_PREFIX = "sha256-v1:";

/** `stopped` is asked between parts: true ends it with an error */
export async function identityOf(file: Blob, stopped: () => boolean = () => false): Promise<string> {
  const digest = sha256.create();
  for (let start = 0; start < file.size; start += IDENTITY_CHUNK) {
    if (stopped()) throw new Error("stopped");
    digest.update(new Uint8Array(await file.slice(start, start + IDENTITY_CHUNK).arrayBuffer()));
  }
  return IDENTITY_PREFIX + Array.from(digest.digest(), (b) => b.toString(16).padStart(2, "0")).join("");
}
