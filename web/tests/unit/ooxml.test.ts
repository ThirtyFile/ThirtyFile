// The size pre-check before opening an Office file reads the sizes the ZIP archive's central directory declares, and
// reading an entry stops as soon as it grows past its limit
import JSZip from "jszip";
import { describe, expect, test } from "vitest";
import { checkZipSizes, declaredEntries, MAX_XML_PART, readEntry, TOO_LARGE } from "@/ooxml/core/package";

const CONTENT = "<root>" + "x".repeat(1000) + "</root>";

async function zipBytes() {
  const zip = new JSZip();
  zip.file("xl/workbook.xml", CONTENT);
  zip.folder("xl/media");
  return zip.generateAsync({ type: "uint8array", compression: "DEFLATE", comment: "a comment after the directory" });
}

const buffer = (bytes: Uint8Array) => bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;

/** Change the uncompressed size declared for every entry in the central directory, as a zip bomb would declare its real size */
function declareSize(bytes: Uint8Array, size: number) {
  const out = bytes.slice();
  const view = new DataView(out.buffer);
  for (let i = 0; i + 46 <= out.length; i++) {
    if (view.getUint32(i, true) === 0x02014b50) view.setUint32(i + 24, size, true);
  }
  return out;
}

/** The central directory and end records of a ZIP64 archive with one entry of `size` bytes (no entry data: only the directory is read) */
function zip64Directory(name: string, size: number) {
  const nameBytes = new TextEncoder().encode(name);
  const entry = new DataView(new ArrayBuffer(46 + nameBytes.length + 20));
  entry.setUint32(0, 0x02014b50, true);
  entry.setUint32(20, 0xffffffff, true); // compressed size: in the extra field
  entry.setUint32(24, 0xffffffff, true); // uncompressed size: in the extra field
  entry.setUint16(28, nameBytes.length, true);
  entry.setUint16(30, 20, true);
  new Uint8Array(entry.buffer).set(nameBytes, 46);
  const extra = 46 + nameBytes.length;
  entry.setUint16(extra, 1, true);
  entry.setUint16(extra + 2, 16, true);
  entry.setBigUint64(extra + 4, BigInt(size), true);
  entry.setBigUint64(extra + 12, BigInt(size), true);

  const lead = 16; // bytes before the directory, standing in for the entries' data
  const record = lead + entry.byteLength;
  const end = new DataView(new ArrayBuffer(56 + 20 + 22));
  end.setUint32(0, 0x06064b50, true); // ZIP64 end of central directory
  end.setBigUint64(24, 1n, true);
  end.setBigUint64(32, 1n, true);
  end.setBigUint64(40, BigInt(entry.byteLength), true);
  end.setBigUint64(48, BigInt(lead), true);
  end.setUint32(56, 0x07064b50, true); // its locator
  end.setBigUint64(56 + 8, BigInt(record), true);
  end.setUint32(76, 0x06054b50, true); // the classic end record, pointing to ZIP64
  end.setUint16(76 + 8, 0xffff, true);
  end.setUint16(76 + 10, 0xffff, true);
  end.setUint32(76 + 16, 0xffffffff, true);

  const out = new Uint8Array(lead + entry.byteLength + end.byteLength);
  out.set(new Uint8Array(entry.buffer), lead);
  out.set(new Uint8Array(end.buffer), record);
  return out;
}

describe("zip size pre-check", () => {
  test("reads each entry's declared size from the central directory", async () => {
    expect([...declaredEntries(buffer(await zipBytes()))]).toEqual([
      { name: "xl/", size: 0 },
      { name: "xl/workbook.xml", size: CONTENT.length },
      { name: "xl/media/", size: 0 },
    ]);
  });

  test("reads ZIP64 sizes, and nothing from bytes that aren't an archive", () => {
    expect([...declaredEntries(buffer(zip64Directory("ppt/media/image1.png", 5_000_000_000)))]).toEqual([
      { name: "ppt/media/image1.png", size: 5_000_000_000 },
    ]);
    expect([...declaredEntries(buffer(new TextEncoder().encode("not a zip")))]).toEqual([]);
  });

  test("a part declared larger than the limit is refused before decompressing", async () => {
    const ok = buffer(await zipBytes());
    const bomb = buffer(declareSize(await zipBytes(), MAX_XML_PART + 1));
    expect(() => checkZipSizes(ok)).not.toThrow();
    expect(() => checkZipSizes(bomb)).toThrow(TOO_LARGE);
    expect(() => checkZipSizes(buffer(zip64Directory("ppt/media/image1.png", 5_000_000_000)))).toThrow(TOO_LARGE);
  });
});

describe("reading an entry", () => {
  test("gives its content, and stops at the limit", async () => {
    const zip = await JSZip.loadAsync(await zipBytes());
    expect(await readEntry(zip, "xl/workbook.xml", "string")).toBe(CONTENT);
    expect(await readEntry(zip, "xl/missing.xml", "string")).toBeNull();
    const other = await JSZip.loadAsync(await zipBytes());
    await expect(readEntry(other, "xl/workbook.xml", "string", 100)).rejects.toThrow(TOO_LARGE);
  });
});
