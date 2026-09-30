/** The questions asked with confirm() (lib/confirm.ts), and the same asked from inside another dialog */
import { useCallback, useRef, useState, type ReactNode } from "react";
import { ConfirmDialog } from "@/components/dialogs";
import { confirmRequest, type ConfirmOptions, type ConfirmRequest } from "@/lib/confirm";
import { useStore } from "@/lib/store";

function RequestDialog({ req, onDone }: { req: ConfirmRequest; onDone(ok: boolean): void }) {
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

/** Shows the questions asked with confirm(); mounted once for the whole app */
export function ConfirmHost() {
  const req = useStore(confirmRequest);
  if (!req) return null;
  const done = (ok: boolean) => {
    if (confirmRequest.get() === req) confirmRequest.set(null);
    req.resolve(ok);
  };
  return <RequestDialog req={req} onDone={done} />;
}

/**
 * The same from a component, for questions asked inside another dialog: render the returned element in that dialog's
 * content, so the question opens as a nested dialog above it (and answering doesn't close the dialog underneath)
 */
export function useConfirm(): [(options: ConfirmOptions) => Promise<boolean>, ReactNode] {
  const [req, setReq] = useState<ConfirmRequest | null>(null);
  const open = useRef<ConfirmRequest | null>(null);
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
