import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { t } from "@/lib/i18n";

function localTime(seconds: number | null) {
  if (seconds === null) return "";
  const date = new Date(seconds * 1000);
  return new Date(date.getTime() - date.getTimezoneOffset() * 60000).toISOString().slice(0, 16);
}

export function GrantExpiry({ expiry, guest, maxDays, pending, onSave }: { expiry: number | null; guest: boolean; maxDays: number; pending: boolean; onSave(value: number | null): void }) {
  const [text, setText] = useState(localTime(expiry));
  const next = text ? Math.floor(new Date(text).getTime() / 1000) : null;
  const now = Math.floor(Date.now() / 1000);
  const invalid = next === null ? guest : !Number.isFinite(next) || next <= now || (guest && next > now + maxDays * 86400);
  return (
    <form
      className="flex w-full flex-wrap items-center justify-end gap-2 text-xs"
      onSubmit={(e) => {
        e.preventDefault();
        if (!invalid) onSave(next);
      }}
    >
      <Input
        type="datetime-local"
        aria-label={t("Access end date")}
        className="h-8 w-auto min-w-0 max-w-full text-xs"
        value={text}
        required={guest}
        max={guest ? localTime(now + maxDays * 86400) : undefined}
        aria-invalid={invalid}
        onChange={(e) => setText(e.target.value)}
      />
      <Button type="submit" size="sm" variant="outline" disabled={pending || invalid || next === expiry}>
        {t("Save end date")}
      </Button>
      {guest && <span className="basis-full text-right text-muted-foreground">{t("Required within {n} days", { n: maxDays })}</span>}
    </form>
  );
}
