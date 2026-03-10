/**
 * HSL ↔ Hex color conversion utilities.
 *
 * The CSS variables use space-separated HSL format: "224 71% 4%"
 * Color inputs use hex format: "#0a1628"
 */

/** Parse "224 71% 4%" → { h: 224, s: 71, l: 4 } */
export function parseHsl(hsl: string): { h: number; s: number; l: number } {
  const parts = hsl
    .trim()
    .split(/\s+/)
    .map((p) => parseFloat(p));
  return { h: parts[0] ?? 0, s: parts[1] ?? 0, l: parts[2] ?? 0 };
}

/** Format { h, s, l } → "224 71% 4%" */
export function formatHsl(h: number, s: number, l: number): string {
  return `${Math.round(h)} ${Math.round(s)}% ${Math.round(l)}%`;
}

/** Convert HSL string "224 71% 4%" → hex "#0a1628" */
export function hslToHex(hsl: string): string {
  const { h, s, l } = parseHsl(hsl);
  return hslValuesToHex(h, s, l);
}

/** Convert HSL values to hex */
function hslValuesToHex(h: number, s: number, l: number): string {
  const sNorm = s / 100;
  const lNorm = l / 100;
  const c = (1 - Math.abs(2 * lNorm - 1)) * sNorm;
  const x = c * (1 - Math.abs(((h / 60) % 2) - 1));
  const m = lNorm - c / 2;

  let r = 0,
    g = 0,
    b = 0;
  if (h < 60) {
    r = c; g = x; b = 0;
  } else if (h < 120) {
    r = x; g = c; b = 0;
  } else if (h < 180) {
    r = 0; g = c; b = x;
  } else if (h < 240) {
    r = 0; g = x; b = c;
  } else if (h < 300) {
    r = x; g = 0; b = c;
  } else {
    r = c; g = 0; b = x;
  }

  const toHex = (v: number) =>
    Math.round((v + m) * 255)
      .toString(16)
      .padStart(2, "0");

  return `#${toHex(r)}${toHex(g)}${toHex(b)}`;
}

/** Convert hex "#0a1628" → HSL string "224 71% 4%" */
export function hexToHsl(hex: string): string {
  const result = /^#?([a-f\d]{2})([a-f\d]{2})([a-f\d]{2})$/i.exec(hex);
  if (!result) return "0 0% 0%";

  const r = parseInt(result[1], 16) / 255;
  const g = parseInt(result[2], 16) / 255;
  const b = parseInt(result[3], 16) / 255;

  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  const d = max - min;

  if (d === 0) return formatHsl(0, 0, l * 100);

  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);

  let h = 0;
  if (max === r) h = ((g - b) / d + (g < b ? 6 : 0)) / 6;
  else if (max === g) h = ((b - r) / d + 2) / 6;
  else h = ((r - g) / d + 4) / 6;

  return formatHsl(h * 360, s * 100, l * 100);
}
