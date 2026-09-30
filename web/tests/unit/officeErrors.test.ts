// The Office code throws coded errors; the app says them in words (lib/officeErrors.ts)
import { describe, expect, test } from "vitest";
import JSZip from "jszip";
import { OoxmlError } from "@/ooxml/core/errors";
import { readXlsx } from "@/ooxml/xlsx/workbook";
import { officeErrorMessage } from "@/lib/officeErrors";

describe("officeErrorMessage", () => {
  test("says each code in words, with its detail", () => {
    expect(officeErrorMessage(new OoxmlError("no-sheets"), "fallback")).toBe("The workbook has no editable sheets");
    expect(officeErrorMessage(new OoxmlError("bad-cell-reference", "ZZZ0"), "fallback")).toBe("Invalid cell reference: ZZZ0");
  });

  test("keeps other errors' messages, and uses the fallback without an error", () => {
    expect(officeErrorMessage(new Error("Request failed (500)"), "fallback")).toBe("Request failed (500)");
    expect(officeErrorMessage("nope", "fallback")).toBe("fallback");
  });

  test("a file that isn't a ZIP package is refused as not an Office document", async () => {
    const e = await readXlsx(new TextEncoder().encode("plain text").buffer).catch((e: unknown) => e);
    expect(e).toBeInstanceOf(OoxmlError);
    expect(officeErrorMessage(e, "fallback")).toMatch(/isn't a valid Office document/);
  });

  test("a package without a workbook says so", async () => {
    const zip = new JSZip();
    zip.file("word/document.xml", "<document/>");
    const e = await readXlsx(await zip.generateAsync({ type: "arraybuffer" })).catch((e: unknown) => e);
    expect(officeErrorMessage(e, "fallback")).toBe("Couldn't find the workbook contents. This may not be an Excel file.");
  });
});
