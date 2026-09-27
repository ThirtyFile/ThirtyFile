/** Asking before an action that can't be taken back, as a promise: `if (!(await confirm({ … }))) return;` */
import { useCallback, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import { ConfirmDialog } from "@/components/dialogs";

export interface ConfirmOptions {
  title: string;
  description?: ReactNode;
  confirmText?: string;
  destructive?: boolean;
}

interface Request extends ConfirmOptions {
  resolve(ok: boolean): void;
}

function RequestDialog({ req, onDone }: { req: Request; onDone(ok: boolean): void }) {
  return (
    <ConfirmDialog
      title={req.title}
      description={req.description}
      confirmText={req.confirmText}
      destructive={req.destructive}
      onConfirm={async () => onDone(true)}
      onClose={() => onDone(false)}
    />
  );
}

let current: Request | null = null;
const listeners = new Set<() => void>();
const emit = () => listeners.forEach((l) => l());

/** For code outside components (tabs, transfer lists): shown by <ConfirmHost />. A new question answers an open one with "no". */
export function confirm(options: ConfirmOptions): Promise<boolean> {
  current?.resolve(false);
  return new Promise((resolve) => {
    current = { ...options, resolve };
    emit();
  });
}

/** Shows the questions asked with confirm(); mounted once for the whole app */
export function ConfirmHost() {
  const req = useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => current,
  );
  if (!req) return null;
  const done = (ok: boolean) => {
    if (current === req) {
      current = null;
      emit();
    }
    req.resolve(ok);
  };
  return <RequestDialog req={req} onDone={done} />;
}

/**
 * The same from a component, for questions asked inside another dialog: render the returned element in that dialog's
 * content, so the question opens as a nested dialog above it (and answering doesn't close the dialog underneath)
 */
export function useConfirm(): [(options: ConfirmOptions) => Promise<boolean>, ReactNode] {
  const [req, setReq] = useState<Request | null>(null);
  const open = useRef<Request | null>(null);
  const ask = useCallback((options: ConfirmOptions) => {
    open.current?.resolve(false);
    return new Promise<boolean>((resolve) => {
      open.current = { ...options, resolve };
      setReq(open.current);
    });
  }, []);
  const element = req && (
    <RequestDialog
      req={req}
      onDone={(ok) => {
        if (open.current === req) open.current = null;
        setReq(null);
        req.resolve(ok);
      }}
    />
  );
  return [ask, element];
}
