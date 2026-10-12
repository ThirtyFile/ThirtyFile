//! Simple dialogs and their parts: an error under a form, asking for a name, and confirming an action

import { useId, useRef, useState, type FocusEvent, type ReactNode } from "react";
import { Loader2Icon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { t } from "@/lib/i18n";
import { useSubmit } from "@/lib/useSubmit";

/** An error under a form, read out when it appears; give it an `id` and point the field at it with `errorProps` */
export function ErrorText({ children, id }: { children: ReactNode; id?: string }) {
  return children ? (
    <p id={id} role="alert" className="text-sm text-destructive">
      {children}
    </p>
  ) : null;
}

/** A field's link to the ErrorText below it, while there is an error */
export function errorProps(error: unknown, id: string) {
  return error ? { "aria-invalid": true, "aria-describedby": id } : {};
}

/**
 * Selects a dialog's name field (up to `end` of its text) the first time it gets the focus: give the dialog
 * `initialFocus={ref}` too. The dialog focuses the field as it opens, but not once the person is in another of its
 * fields; a timer that selected the field later could take the focus from there and put what they type into the name
 */
export function useSelectOnFirstFocus(end?: (value: string) => number) {
  const done = useRef(false);
  return (e: FocusEvent<HTMLInputElement>) => {
    if (done.current) return;
    done.current = true;
    const el = e.currentTarget;
    el.setSelectionRange(0, end?.(el.value) ?? el.value.length);
  };
}

/** When renaming, only the base name is selected, so it can be typed over directly */
const baseName = (name: string) => {
  const dot = name.lastIndexOf(".");
  return dot > 0 ? dot : name.length;
};

export function NameDialog(props: { title: string; label?: string; initial?: string; confirmText?: string; onSubmit(name: string): Promise<void>; onClose(): void }) {
  const [name, setName] = useState(props.initial ?? "");
  const ref = useRef<HTMLInputElement>(null);
  const { busy, error, run } = useSubmit(() => props.onSubmit(name.trim()));
  const errorId = useId();
  const select = useSelectOnFirstFocus(baseName);

  return (
    <Dialog open onOpenChange={(o) => !o && props.onClose()}>
      <DialogContent initialFocus={ref}>
        <form onSubmit={run} className="grid gap-4">
          <DialogHeader>
            <DialogTitle>{props.title}</DialogTitle>
          </DialogHeader>
          <div className="grid gap-2">
            {props.label && <Label htmlFor="name-input">{props.label}</Label>}
            <Input id="name-input" ref={ref} value={name} onChange={(e) => setName(e.target.value)} onFocus={select} autoComplete="off" {...errorProps(error, errorId)} />
            <ErrorText id={errorId}>{error}</ErrorText>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={props.onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={busy || !name.trim()}>
              {busy && <Loader2Icon className="animate-spin" />}
              {props.confirmText ?? t("OK")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

export function ConfirmDialog(props: {
  title: string;
  description?: ReactNode;
  confirmText?: string;
  destructive?: boolean;
  /** Can't be undone (e.g. deleting permanently): Cancel has the focus, so pressing Enter doesn't do it */
  irreversible?: boolean;
  /** More to fill in before confirming (such as the administrator's password), with `ready` once it is */
  children?: ReactNode;
  ready?: boolean;
  onConfirm(): Promise<void>;
  onClose(): void;
}) {
  const { busy, error, run } = useSubmit(props.onConfirm);
  return (
    <Dialog open onOpenChange={(o) => !o && props.onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{props.title}</DialogTitle>
          {props.description && <DialogDescription>{props.description}</DialogDescription>}
        </DialogHeader>
        {props.children}
        <ErrorText>{error}</ErrorText>
        <DialogFooter>
          <Button variant="outline" onClick={props.onClose} autoFocus={props.irreversible}>
            {t("Cancel")}
          </Button>
          <Button variant={props.destructive ? "destructive" : "default"} disabled={busy || props.ready === false} onClick={() => run()} autoFocus={!props.irreversible && !props.children}>
            {busy && <Loader2Icon className="animate-spin" />}
            {props.confirmText ?? t("OK")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
