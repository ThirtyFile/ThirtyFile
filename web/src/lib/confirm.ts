/** Asking before an action that can't be taken back, as a promise: `if (!(await confirm({ … }))) return;` */
import type { ReactNode } from "react";
import { createStore } from "@/lib/store";

export interface ConfirmOptions {
  title: string;
  description?: ReactNode;
  confirmText?: string;
  destructive?: boolean;
  irreversible?: boolean;
}

/** A question asked with confirm(), shown by <ConfirmHost /> (components/confirm.tsx) */
export interface ConfirmRequest extends ConfirmOptions {
  resolve(ok: boolean): void;
}

/** The question asked now */
export const confirmRequest = createStore<ConfirmRequest | null>(null);

/** For code outside components (tabs, transfer lists): shown by <ConfirmHost />. A new question answers an open one with "no". */
export function confirm(options: ConfirmOptions): Promise<boolean> {
  confirmRequest.get()?.resolve(false);
  return new Promise((resolve) => confirmRequest.set({ ...options, resolve }));
}
