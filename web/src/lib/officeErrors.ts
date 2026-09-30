//! What the app says when the Office code (src/ooxml) can't read or write a file: it throws coded errors, said here in
//! the interface's language

import { OoxmlError, type OoxmlErrorCode } from "@/ooxml/core/errors";
import { t } from "@/lib/i18n";

const MESSAGES: Record<OoxmlErrorCode, (detail?: string) => string> = {
  "not-ooxml": () =>
    t("This file isn't a valid Office document (it may be damaged, or wasn't created by Office), so it can't be opened online. Download it to check."),
  "too-large": () => t("The file's content is too large to open. Download it and open it in Excel."),
  "no-workbook": () => t("Couldn't find the workbook contents. This may not be an Excel file."),
  "no-sheets": () => t("The workbook has no editable sheets"),
  "unknown-sheet": () => t("The sheet format isn't recognized"),
  "bad-cell-reference": (ref) => t("Invalid cell reference: {ref}", { ref: ref ?? "" }),
};

/** An error's message for the person: the Office code's in words, others' as they are, and `fallback` without one */
export function officeErrorMessage(e: unknown, fallback: string): string {
  if (e instanceof OoxmlError) return MESSAGES[e.code](e.detail);
  return e instanceof Error ? e.message : fallback;
}
