/** Asking before an action that can't be taken back, as a promise: `if (!(await confirm({ … }))) return;` */
import { useCallback, useRef, useState, type ReactNode } from "react";
import { ConfirmDialog } from "@/components/dialogs";
import { createStore, useStore } from "@/lib/store";

export interface ConfirmOptions {
  title: string;
  description?: ReactNode;
  confirmText?: string;
  destructive?: boolean;
  irreversible?: boolean;
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
      irreversible={req.irreversible}
      onConfirm={async () => onDone(true)}
      onClose={() => onDone(false)}
    />
  );
}

const current = createStore<Request | null>(null);

/** For code outside components (tabs, transfer lists): shown by <ConfirmHost />. A new question answers an open one with "no". */
export function confirm(options: ConfirmOptions): Promise<boolean> {
  current.get()?.resolve(false);
  return new Promise((resolve) => current.set({ ...options, resolve }));
}

/** Shows the questions asked with confirm(); mounted once for the whole app */
export function ConfirmHost() {
  const req = useStore(current);
  if (!req) return null;
  const done = (ok: boolean) => {
    if (current.get() === req) current.set(null);
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
