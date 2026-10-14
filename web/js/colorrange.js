// ---- Selecting by colour: the maths --------------------------------------------
//
// `src/colorrange.rs` again, for the colour wheel and its live preview: the
// preview runs here on the displayed picture so a drag is instant, and the
// server computes the same thing on the full one when the drag lets go. One
// fixture table, in that file's tests and in colorrange.test.js, keeps the two
// from drifting.
//
// OKLCh, the grade's own space: hue is atan2(b, a) in degrees, lightness
// L × 100, saturation chroma / FULL_CHROMA × 100. A range is {a, b, c, d}:
// fully selected from b to c, easing (smoothstep) to nothing at a and d. Hue ranges are
// kept unwrapped — a in 0..360, the rest counting on past 360 — so a red
// range is {340, 350, 400, 410}.
//
// Pure: no DOM, so `node --test` can load it.

export const FULL_CHROMA = 0.32;
const HUE_FROM_SATURATION = 10;

const LINEAR = new Float32Array(256);
for (let i = 0; i < 256; i++) {
  const c = i / 255;
  LINEAR[i] = c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
}

// An sRGB pixel as [hue°, lightness 0..100, saturation 0..100].
export function oklch(r, g, b) {
  const R = LINEAR[r], G = LINEAR[g], B = LINEAR[b];
  const l = Math.cbrt(0.4122214708 * R + 0.5363325363 * G + 0.0514459929 * B);
  const m = Math.cbrt(0.2119034982 * R + 0.6806995451 * G + 0.1073969566 * B);
  const s = Math.cbrt(0.0883024619 * R + 0.2817188376 * G + 0.6299787005 * B);
  const L = 0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s;
  const A = 1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s;
  const Bb = 0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s;
  const hue = ((Math.atan2(Bb, A) * 180) / Math.PI + 360) % 360;
  return [hue, L * 100, Math.min(100, (Math.hypot(A, Bb) / FULL_CHROMA) * 100)];
}

// 1 from b to c, a smoothstep down to 0 at a and d.
export function rampWeight(r, v) {
  let t;
  if (v < r.a || v > r.d) t = 0;
  else if (v < r.b) t = r.b > r.a ? (v - r.a) / (r.b - r.a) : 1;
  else if (v <= r.c) t = 1;
  else t = r.d > r.c ? (r.d - v) / (r.d - r.c) : 1;
  return t * t * (3 - 2 * t);
}

// The feather's σ at 1, as a share of the picture's short side.
export const FEATHER_REACH = 0.02;

export const hueWeight = (r, h) => rampWeight(r, r.a + (((h - r.a) % 360) + 360) % 360);

// How selected an sRGB pixel is, 0..1, before the feather (a blur of the
// weights, `range.feather`). A channel left null selects all.
export function weight(range, r, g, b) {
  const [h, l, s] = oklch(r, g, b);
  const hue = range.hue ? hueWeight(range.hue, h) * Math.min(1, s / HUE_FROM_SATURATION) : 1;
  const light = range.lightness ? rampWeight(range.lightness, l) : 1;
  const sat = range.saturation ? rampWeight(range.saturation, s) : 1;
  return hue * light * sat;
}

// OKLCh (lightness 0..100, chroma as saturation 0..100, hue°) to sRGB bytes,
// clamped — for drawing the wheel's ring and the bars in the colours the
// selection reads, not HSL's.
export function oklchToSrgb(lPct, satPct, hDeg) {
  const L = lPct / 100, C = (satPct / 100) * FULL_CHROMA, h = (hDeg * Math.PI) / 180;
  const A = C * Math.cos(h), B = C * Math.sin(h);
  const l = (L + 0.3963377774 * A + 0.2158037573 * B) ** 3;
  const m = (L - 0.1055613458 * A - 0.0638541728 * B) ** 3;
  const s = (L - 0.0894841775 * A - 1.2914855480 * B) ** 3;
  const lin = [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
  ];
  return lin.map((v) => {
    const c = Math.min(1, Math.max(0, v));
    const e = c <= 0.0031308 ? c * 12.92 : 1.055 * c ** (1 / 2.4) - 0.055;
    return Math.round(e * 255);
  });
}

const round = (v) => Math.round(v);
// The range in words, for the Area fold's line: what is fully selected.
export function describe(range) {
  if (!range) return 'every colour';
  const hue = range.hue ? `hue ${round(range.hue.b % 360)}°–${round(range.hue.c % 360)}°` : 'any hue';
  const light = range.lightness ? `light ${round(range.lightness.b)}–${round(range.lightness.c)}` : 'any lightness';
  const sat = range.saturation ? `saturation ${round(range.saturation.b)}–${round(range.saturation.c)}` : 'any saturation';
  return `${hue} · ${light} · ${sat}`;
}

export const narrows = (range) => !!range && !!(range.hue || range.lightness || range.saturation);
