import { request, get, post } from "@/api/client";
import type { Me, Passkeys, PasskeyAnswer, PasskeyCreated, PasskeyCreationOptions, PasskeyRequestOptions } from "@/api/types";

/** Passkeys: signing in with one, the second step after a password, and my own list */
export const passkeysApi = {
  /** The challenge for signing in with a passkey, without a user name */
  passkeyOptions: () => post<PasskeyRequestOptions>("/auth/passkey/options"),
  passkeyLogin: (answer: PasskeyAnswer) => post<Me>("/auth/passkey/login", answer),
  /** After a correct password: the challenge for the passkeys of that account, then the answer */
  passkeySecondOptions: (ticket: string) => post<PasskeyRequestOptions>("/auth/passkey/second/options", { ticket }),
  passkeySecond: (ticket: string, answer: PasskeyAnswer) => post<Me>("/auth/passkey/second", { ticket, ...answer }),
  passkeys: () => get<Passkeys>("/auth/passkeys"),
  /** Adding one asks for the password (and a code, with two-factor sign-in) first; accounts without a password, nothing */
  passkeyAddOptions: (password?: string, code?: string) => post<PasskeyCreationOptions>("/auth/passkeys/options", { password, code }),
  addPasskey: (name: string, created: PasskeyCreated) => post<{ id: number }>("/auth/passkeys", { name, ...created }),
  renamePasskey: (id: number, name: string) => request("PATCH", `/auth/passkeys/${id}`, { name }),
  removePasskey: (id: number, password?: string, code?: string) => request("DELETE", `/auth/passkeys/${id}`, { password, code }),
};
