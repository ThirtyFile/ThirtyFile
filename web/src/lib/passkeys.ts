//! Passkeys in the browser (Web Authentication): turning what the server asks into what `navigator.credentials` takes, and
//! what it answers into what the server takes. Everything crosses the wire as base64url.

import type { PasskeyAnswer, PasskeyCreated, PasskeyCreationOptions, PasskeyRequestOptions } from "@/api";

/** The person closed the browser's prompt, or let it time out: nothing to say but that it wasn't used */
export class PasskeyCancelled extends Error {}

/** Whether this browser can use passkeys at all (it needs a secure page) */
export function passkeysSupported(): boolean {
  return typeof window !== "undefined" && window.isSecureContext && typeof PublicKeyCredential !== "undefined" && !!navigator.credentials;
}

export function toBytes(b64url: string): ArrayBuffer {
  const b64 = b64url.replace(/-/g, "+").replace(/_/g, "/");
  const bin = atob(b64 + "=".repeat((4 - (b64.length % 4)) % 4));
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out.buffer;
}

export function toBase64Url(buffer: ArrayBuffer): string {
  let bin = "";
  for (const b of new Uint8Array(buffer)) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** The browser's refusal as the page tells it: cancelling is not an error to show */
function explain(err: unknown): never {
  if (err instanceof DOMException && (err.name === "NotAllowedError" || err.name === "AbortError")) throw new PasskeyCancelled();
  throw err;
}

/** Asks the browser for a passkey to sign in with (`o.allow` empty: any passkey of this site) */
export async function getPasskey(o: PasskeyRequestOptions): Promise<PasskeyAnswer> {
  let credential: Credential | null;
  try {
    credential = await navigator.credentials.get({
      publicKey: {
        challenge: toBytes(o.challenge),
        rpId: o.rp_id,
        timeout: o.timeout,
        userVerification: o.user_verification,
        allowCredentials: o.allow.map((c) => ({ type: "public-key", id: toBytes(c.id), transports: c.transports as AuthenticatorTransport[] })),
      },
    });
  } catch (err) {
    explain(err);
  }
  if (!(credential instanceof PublicKeyCredential) || !(credential.response instanceof AuthenticatorAssertionResponse)) throw new PasskeyCancelled();
  const r = credential.response;
  return {
    credential_id: toBase64Url(credential.rawId),
    client_data_json: toBase64Url(r.clientDataJSON),
    authenticator_data: toBase64Url(r.authenticatorData),
    signature: toBase64Url(r.signature),
    user_handle: r.userHandle ? toBase64Url(r.userHandle) : undefined,
  };
}

/** Asks the browser to make a passkey (a device remembers it; it is discoverable, so it can sign in without a user name) */
export async function createPasskey(o: PasskeyCreationOptions): Promise<PasskeyCreated> {
  let credential: Credential | null;
  try {
    credential = await navigator.credentials.create({
      publicKey: {
        challenge: toBytes(o.challenge),
        rp: o.rp,
        user: { id: toBytes(o.user.id), name: o.user.name, displayName: o.user.display_name },
        pubKeyCredParams: o.algorithms.map((alg) => ({ type: "public-key", alg })),
        timeout: o.timeout,
        excludeCredentials: o.exclude.map((c) => ({ type: "public-key", id: toBytes(c.id), transports: c.transports as AuthenticatorTransport[] })),
        authenticatorSelection: { residentKey: "required", requireResidentKey: true, userVerification: "required" },
        attestation: "none",
      },
    });
  } catch (err) {
    explain(err);
  }
  if (!(credential instanceof PublicKeyCredential) || !(credential.response instanceof AuthenticatorAttestationResponse)) throw new PasskeyCancelled();
  const r = credential.response;
  return {
    client_data_json: toBase64Url(r.clientDataJSON),
    attestation_object: toBase64Url(r.attestationObject),
    transports: r.getTransports?.() ?? [],
  };
}
