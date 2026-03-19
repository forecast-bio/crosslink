import { describe, it, expect } from "vitest";
import { parseHsl, formatHsl, hslToHex, hexToHsl } from "../lib/color";

describe("parseHsl", () => {
  it("parses a typical HSL CSS variable string", () => {
    const result = parseHsl("224 71% 4%");
    expect(result.h).toBe(224);
    expect(result.s).toBe(71);
    expect(result.l).toBe(4);
  });

  it("handles extra whitespace", () => {
    const result = parseHsl("  180  50%  50%  ");
    expect(result.h).toBe(180);
    expect(result.s).toBe(50);
    expect(result.l).toBe(50);
  });
});

describe("formatHsl", () => {
  it("formats HSL values back to the CSS variable string format", () => {
    expect(formatHsl(224, 71, 4)).toBe("224 71% 4%");
  });

  it("rounds float values", () => {
    expect(formatHsl(224.6, 71.4, 4.9)).toBe("225 71% 5%");
  });
});

describe("hslToHex round-trip via hexToHsl", () => {
  it("converts a known dark background colour", () => {
    // The dark background CSS variable is "224 71% 4%" → "#0a1628" (approx)
    const hex = hslToHex("224 71% 4%");
    expect(hex).toMatch(/^#[0-9a-f]{6}$/);
  });

  it("round-trips: hex → hsl → hex produces a value within 1 LSB per channel", () => {
    // Due to floating-point rounding in the HSL ↔ hex conversion, the
    // round-tripped value may differ by ±1 in each channel.
    const original = "#3b82f6"; // Tailwind blue-500
    const hsl = hexToHsl(original);
    const roundTripped = hslToHex(hsl);

    const parse = (h: string) => ({
      r: parseInt(h.slice(1, 3), 16),
      g: parseInt(h.slice(3, 5), 16),
      b: parseInt(h.slice(5, 7), 16),
    });
    const o = parse(original);
    const rt = parse(roundTripped);

    expect(Math.abs(o.r - rt.r)).toBeLessThanOrEqual(1);
    expect(Math.abs(o.g - rt.g)).toBeLessThanOrEqual(1);
    expect(Math.abs(o.b - rt.b)).toBeLessThanOrEqual(1);
  });

  it("returns #000000 for an invalid hex string", () => {
    // hexToHsl returns "0 0% 0%" for an invalid hex
    const result = hexToHsl("not-a-hex");
    expect(result).toBe("0 0% 0%");
  });
});
