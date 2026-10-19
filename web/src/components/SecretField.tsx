//! Fields that hold a secret (a share link's password, a private key, a password shown once): hidden until their Show
//! button is pressed, so a secret isn't on the screen by accident while it is typed, pasted or saved (or the screen is
//! shared). Hidden again whenever the field is shown anew: a dialog opened again, or another item's.

import { useState, type ComponentProps } from "react";
import { EyeIcon, EyeOffIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";

/** The Show button: one name, and pressed while the secret is shown */
function Reveal({ shown, label, controls, disabled, onToggle }: { shown: boolean; label: string; controls?: string; disabled?: boolean; onToggle(): void }) {
  const Icon = shown ? EyeOffIcon : EyeIcon;
  return (
    <Button
      type="button"
      size="icon-sm"
      variant="ghost"
      className="absolute top-0.5 right-0.5 text-muted-foreground"
      aria-label={label}
      title={label}
      aria-pressed={shown}
      aria-controls={controls}
      disabled={disabled}
      onClick={onToggle}
    >
      <Icon />
    </Button>
  );
}

/**
 * A password field with a Show button. `revealLabel`: the button's name (Show password, Show Secret Key…). Typed
 * secrets give `autoComplete="new-password"`, so the browser doesn't fill in the account's own password
 */
export function SecretInput({ revealLabel, className, ...props }: Omit<ComponentProps<"input">, "type"> & { revealLabel: string }) {
  const [shown, setShown] = useState(false);
  return (
    <div className="relative min-w-0 flex-1">
      <Input {...props} type={shown ? "text" : "password"} className={cn("pr-9", className)} />
      <Reveal shown={shown} label={revealLabel} controls={props.id} disabled={props.disabled} onToggle={() => setShown((s) => !s)} />
    </div>
  );
}

/**
 * A secret of several lines (a private key), with a Show button: its text drawn as dots until shown (blurred where
 * the browser can't draw dots), but kept as it is, line breaks and all, to paste, edit and save
 */
export function SecretTextarea({ revealLabel, className, ...props }: ComponentProps<"textarea"> & { revealLabel: string }) {
  const [shown, setShown] = useState(false);
  return (
    <div className="relative min-w-0">
      <Textarea {...props} className={cn("pr-9", !shown && "tf-secret-text", className)} />
      <Reveal shown={shown} label={revealLabel} controls={props.id} disabled={props.disabled} onToggle={() => setShown((s) => !s)} />
    </div>
  );
}
