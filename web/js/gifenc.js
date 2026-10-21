// ---- Animated GIF -------------------------------------------------------------
//
// The clip dialog's GIF download: the one format that loops inline anywhere,
// at the cost of sound and colour. One palette for the whole clip (median
// cut over a histogram of every frame), so colours don't shimmer from frame
// to frame the way a palette per frame makes them; LZW as GIF89a defines it.
// Pure, for `gifenc.test.js`: frames come in as RGBA bytes.

// A colour's histogram bin: 5 bits per channel.
const bin = (r, g, b) => ((r >> 3) << 10) | ((g >> 3) << 5) | (b >> 3);

// Count every frame's colours, a pixel in `step` at a time.
export function histogram(frames, step = 1) {
  const h = new Uint32Array(32768);
  for (const f of frames) {
    for (let i = 0; i < f.length; i += 4 * step) h[bin(f[i], f[i + 1], f[i + 2])]++;
  }
  return h;
}

// Median cut: the box with the widest channel split at its median, until
// there are `n` boxes; each box's colour is its pixels' mean.
export function palette(hist, n = 256) {
  const colours = [];
  for (let k = 0; k < hist.length; k++) if (hist[k]) colours.push(k);
  const chan = (k, c) => (k >> (10 - 5 * c)) & 31;
  let boxes = [colours];
  while (boxes.length < n) {
    let best = -1;
    let bestRange = 0;
    let bestChan = 0;
    boxes.forEach((b, i) => {
      if (b.length < 2) return;
      for (let c = 0; c < 3; c++) {
        let lo = 31;
        let hi = 0;
        for (const k of b) { const v = chan(k, c); if (v < lo) lo = v; if (v > hi) hi = v; }
        if (hi - lo > bestRange) { bestRange = hi - lo; best = i; bestChan = c; }
      }
    });
    if (best < 0) break;
    const b = boxes[best].sort((x, y) => chan(x, bestChan) - chan(y, bestChan));
    let total = 0;
    for (const k of b) total += hist[k];
    let acc = 0;
    let cut = 1;
    for (; cut < b.length - 1; cut++) { acc += hist[b[cut - 1]]; if (acc * 2 >= total) break; }
    boxes.splice(best, 1, b.slice(0, cut), b.slice(cut));
  }
  return boxes.map((b) => {
    let r = 0; let g = 0; let bl = 0; let w = 0;
    for (const k of b) {
      const c = hist[k];
      r += (chan(k, 0) * 8 + 4) * c; g += (chan(k, 1) * 8 + 4) * c; bl += (chan(k, 2) * 8 + 4) * c; w += c;
    }
    return [Math.round(r / w), Math.round(g / w), Math.round(bl / w)];
  });
}

// Each histogram bin's nearest palette entry, found once per bin used.
function mapper(pal) {
  const memo = new Int16Array(32768).fill(-1);
  return (r, g, b) => {
    const k = bin(r, g, b);
    if (memo[k] >= 0) return memo[k];
    let best = 0;
    let bestD = Infinity;
    const cr = (r >> 3) * 8 + 4; const cg = (g >> 3) * 8 + 4; const cb = (b >> 3) * 8 + 4;
    for (let i = 0; i < pal.length; i++) {
      const [pr, pg, pb] = pal[i];
      const d = (pr - cr) ** 2 * 2 + (pg - cg) ** 2 * 4 + (pb - cb) ** 2 * 3;
      if (d < bestD) { bestD = d; best = i; }
    }
    memo[k] = best;
    return best;
  };
}

// GIF's LZW over palette indices, variable code width from `minSize + 1` up
// to 12 bits, a clear code when the table fills; packed LSB first.
export function lzw(indices, minSize) {
  const clear = 1 << minSize;
  const eoi = clear + 1;
  const out = [];
  let acc = 0;
  let bits = 0;
  let size = minSize + 1;
  const put = (code) => {
    acc |= code << bits;
    bits += size;
    while (bits >= 8) { out.push(acc & 255); acc >>>= 8; bits -= 8; }
  };
  let dict = new Map();
  let next = eoi + 1;
  put(clear);
  let prefix = indices[0];
  for (let i = 1; i < indices.length; i++) {
    const c = indices[i];
    const key = prefix * 4096 + c;
    const hit = dict.get(key);
    if (hit !== undefined) { prefix = hit; continue; }
    put(prefix);
    if (next < 4096) {
      dict.set(key, next++);
      if (next > (1 << size) && size < 12) size++;
    } else {
      put(clear);
      dict = new Map();
      next = eoi + 1;
      size = minSize + 1;
    }
    prefix = c;
  }
  put(prefix);
  put(eoi);
  if (bits > 0) out.push(acc & 255);
  return Uint8Array.from(out);
}

// Data cut into GIF's sub-blocks of up to 255 bytes, then the terminator.
function blocks(data) {
  const out = [];
  for (let i = 0; i < data.length; i += 255) {
    const part = data.subarray(i, i + 255);
    out.push(part.length, ...part);
  }
  out.push(0);
  return out;
}

// A GIF written a frame at a time — a long clip's frames needn't all be in
// memory — over one palette made beforehand (`palette(histogram(...))`).
// `fps` sets each frame's delay in hundredths, carried so the clip keeps its
// length (24 fps is 4,4,4,5,…).
export function gifEncoder(width, height, fps, pal) {
  const colours = pal.slice(0, 256);
  while (colours.length < 256) colours.push([0, 0, 0]);
  const near = mapper(colours);
  const enc = new TextEncoder();
  const parts = [Uint8Array.from([
    ...enc.encode('GIF89a'),
    width & 255, width >> 8, height & 255, height >> 8,
    0xf7, 0, 0, // a global table of 256 colours
    ...colours.flat(),
    // NETSCAPE2.0: loop for ever.
    0x21, 0xff, 11, ...enc.encode('NETSCAPE2.0'), 3, 1, 0, 0, 0,
  ])];
  const idx = new Uint8Array(width * height);
  let k = 0;
  return {
    add(rgba) {
      for (let i = 0, p = 0; p < idx.length; i += 4, p++) idx[p] = near(rgba[i], rgba[i + 1], rgba[i + 2]);
      // Browsers play a delay under 2 hundredths as 10, so 2 is the floor.
      const delay = Math.max(2, Math.round(((k + 1) * 100) / fps) - Math.round((k * 100) / fps));
      k++;
      parts.push(Uint8Array.from([
        0x21, 0xf9, 4, 0, delay & 255, delay >> 8, 0, 0,
        0x2c, 0, 0, 0, 0, width & 255, width >> 8, height & 255, height >> 8, 0,
        8,
      ]), Uint8Array.from(blocks(lzw(idx, 8))));
    },
    finish() {
      parts.push(Uint8Array.from([0x3b]));
      let n = 0;
      for (const p of parts) n += p.length;
      const out = new Uint8Array(n);
      let at = 0;
      for (const p of parts) { out.set(p, at); at += p.length; }
      return out;
    },
  };
}

// The whole clip at once: `frames` RGBA, all `width`×`height`.
export function encodeGif(frames, width, height, fps) {
  const g = gifEncoder(width, height, fps, palette(histogram(frames, frames.length > 30 ? 3 : 1)));
  for (const f of frames) g.add(f);
  return g.finish();
}
