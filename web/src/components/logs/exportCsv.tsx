//! Exporting a log as a CSV file. The server sends the matching records as recorded (action and event codes, Unix
//! times); the page writes them with the names it shows, in its language and time zone, so a name exists in one place.

import { useState } from "react";
import { DownloadIcon, Loader2Icon } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { saveBlob } from "@/downloads";
import { t } from "@/lib/i18n";
import { errorMessage } from "@/lib/utils";

/**
 * A cell as CSV: quoted when it holds a comma, a quote or a line break, and with a single quote before text that
 * spreadsheets would run as a formula (= + - @, also after a tab or carriage return)
 */
export function csvField(s: string): string {
  const safe = /^[=+\-@\t\r]/.test(s) ? `'${s}` : s;
  return /[",\n\r]/.test(safe) ? `"${safe.replaceAll('"', '""')}"` : safe;
}

const two = (n: number) => String(n).padStart(2, "0");

/** YYYY-MM-DD HH:MM:SS in the page's time zone, which spreadsheets read as a date and time in any language */
export function csvTime(ts: number): string {
  const d = new Date(ts * 1000);
  return `${d.getFullYear()}-${two(d.getMonth() + 1)}-${two(d.getDate())} ${two(d.getHours())}:${two(d.getMinutes())}:${two(d.getSeconds())}`;
}

export interface CsvColumn<T> {
  header: string;
  value(row: T): string | number | null | undefined;
}

/** The file's text: a byte order mark first, so Excel reads it as UTF-8, then the headers and one line per row */
export function toCsv<T>(columns: CsvColumn<T>[], rows: T[]): string {
  const line = (cells: (string | number | null | undefined)[]) => cells.map((c) => csvField(c === null || c === undefined ? "" : String(c))).join(",");
  return `﻿${[line(columns.map((c) => c.header)), ...rows.map((r) => line(columns.map((c) => c.value(r))))].join("\n")}\n`;
}

/** `<name>-YYYYMMDD.csv`, with today's date */
export function csvFileName(name: string, now = new Date()): string {
  return `${name}-${now.getFullYear()}${two(now.getMonth() + 1)}${two(now.getDate())}.csv`;
}

/** Export CSV: loads the records that match the filters (up to 100,000) and saves them */
export function ExportCsvButton<T>({ name, load, columns, disabled }: { name: string; load(): Promise<T[]>; columns: CsvColumn<T>[]; disabled?: boolean }) {
  const [busy, setBusy] = useState(false);
  const run = async () => {
    setBusy(true);
    try {
      const rows = await load();
      saveBlob(new Blob([toCsv(columns, rows)], { type: "text/csv;charset=utf-8" }), csvFileName(name));
    } catch (e) {
      toast.error(errorMessage(e, t("Export failed")));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Button variant="outline" size="sm" className="h-8 text-xs" disabled={disabled || busy} title={t("Export records that match the filters (up to 100,000)")} onClick={run}>
      {busy ? <Loader2Icon className="animate-spin" /> : <DownloadIcon />} {t("Export CSV")}
    </Button>
  );
}
