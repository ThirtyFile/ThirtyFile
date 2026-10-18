import { useId, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { t } from "@/lib/i18n";

function localTime(seconds: number | null) {
  if (seconds === null) return "";
  const date = new Date(seconds * 1000);
  return new Date(date.getTime() - date.getTimezoneOffset() * 60000).toISOString().slice(0, 16);
}

/** A grant's end date, with Save; `name`: whose grant, so each row's field says whose end date it is */
export function GrantExpiry({
  expiry,
  guest,
  maxDays,
  pending,
  name,
  onSave,
}: {
  expiry: number | null;
  guest: boolean;
  maxDays: number;
  pending: boolean;
  name: string;
  onSave(value: number | null): void;
}) {
  const id = useId();
  const initial = localTime(expiry);
  const [text, setText] = useState(initial);
  const next = text ? Math.floor(new Date(text).getTime() / 1000) : null;
  const now = Math.floor(Date.now() / 1000);
  const latest = now + maxDays * 86400;
  const reason =
    next === null
      ? guest
        ? t("Choose an end within {n} days", { n: maxDays })
        : null
      : !Number.isFinite(next) || next <= now
        ? t("Choose a time in the future")
        : guest && next > latest
          ? t("Choose an end within {n} days", { n: maxDays })
          : null;
  // Unchanged as long as the field shows what it opened with (it shows minutes: the seconds of the end date aren't a
  // change)
  const unchanged = text === initial;
  return (
    <form
      className="flex w-full flex-wrap items-center justify-end gap-2 text-xs"
      onSubmit={(e) => {
        e.preventDefault();
        if (!reason && !unchanged) onSave(next);
      }}
    >
      <Input
        type="datetime-local"
        aria-label={t("Access end date for {name}", { name })}
        className="h-8 w-auto min-w-0 max-w-full text-xs"
        value={text}
        required={guest}
        max={guest ? localTime(latest) : undefined}
        aria-invalid={!!reason && !unchanged}
        aria-describedby={reason && !unchanged ? `${id}-why` : undefined}
        onChange={(e) => setText(e.target.value)}
      />
      <Button type="submit" size="sm" variant="outline" disabled={pending || !!reason || unchanged}>
        {t("Save end date")}
      </Button>
      {reason && !unchanged && (
        <span id={`${id}-why`} className="basis-full text-right text-destructive">
          {reason}
        </span>
      )}
      {guest && <span className="basis-full text-right text-muted-foreground">{t("Required within {n} days", { n: maxDays })}</span>}
    </form>
  );
}
