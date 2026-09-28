// The size pre-check before opening an Office file reads the sizes JSZip keeps in a private field (`_data.uncompressedSize`).
// If a JSZip update renames or drops that field, every size reads as 0 and the pre-check passes silently: these tests fail instead.
import JSZip from "jszip";
import { describe, expect, test } from "vitest";
import { checkZipSizes, MAX_XML_PART, TOO_LARGE } from "@/lib/office/ooxml";

const CONTENT = "<root>" + "x".repeat(1000) + "</root>";

async function zipBytes() {
  const zip = new JSZip();
  zip.file("xl/workbook.xml", CONTENT);
  return zip.generateAsync({ type: "uint8array", compression: "DEFLATE" });
}

/** Change the uncompressed size declared for every entry in the central directory, as a zip bomb would declare its real size */
function declareSize(bytes: Uint8Array, size: number) {
  const out = bytes.slice();
  const view = new DataView(out.buffer);
  for (let i = 0; i + 46 <= out.length; i++) {
    if (view.getUint32(i, true) === 0x02014b50) view.setUint32(i + 24, size, true);
  }
  return out;
}

describe("zip size pre-check", () => {
  test("JSZip still exposes the declared size where the pre-check reads it", async () => {
    const zip = await JSZip.loadAsync(await zipBytes());
    const f = zip.file("xl/workbook.xml")!;
    expect((f as unknown as { _data?: { uncompressedSize?: number } })._data?.uncompressedSize).toBe(CONTENT.length);
  });

  test("a part declared larger than the limit is refused before decompressing", async () => {
    const ok = await JSZip.loadAsync(await zipBytes());
    expect(() => checkZipSizes(ok)).not.toThrow();
    const bomb = await JSZip.loadAsync(declareSize(await zipBytes(), MAX_XML_PART + 1));
    expect(() => checkZipSizes(bomb)).toThrow(TOO_LARGE);
  });
});
