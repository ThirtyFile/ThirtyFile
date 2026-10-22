// The palette's colors have names for screen readers: Office's theme colors with their variants, and its standard colors
import { describe, expect, test } from "vite-plus/test";
import { PALETTE, paletteName } from "@/components/ColorMenu";

describe("palette color names", () => {
  test("every color of the palette has a name of its own, and other colors none", () => {
    expect(paletteName("#000000")).toBe("Black");
    expect(paletteName("#4472c4")).toBe("Blue, accent 1");
    expect(paletteName("#D9E1F2")).toBe("Blue, accent 1, variant 1");
    expect(paletteName("#0070C0")).toBe("Blue");
    expect(paletteName("#C00000")).toBe("Dark red");
    expect(paletteName("#123456")).toBeNull();
    const names = PALETTE.flat().map((c) => paletteName(c));
    expect(names.every((n) => !!n)).toBe(true);
    // No two colors share a name (Office's palette has #FFC000 twice: one color, one name)
    expect(new Set(names).size).toBe(new Set(PALETTE.flat()).size);
  });
});
