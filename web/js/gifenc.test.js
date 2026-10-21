// `node --test web/js/` — the GIF encoder (`gifenc.js`): its LZW decodes
// back to the indices it was given, and the file has the shape a GIF reader
// walks. Not in `web::WEB_FILES`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { lzw, palette, histogram, encodeGif } from './gifenc.js';

// GIF's LZW, the reader's side, written from the spec rather than the encoder.
function unlzw(data, minSize) {
  const clear = 1 << minSize;
  const eoi = clear + 1;
  let size = minSize + 1;
  let table = [];
  const reset = () => {
    table = Array.from({ length: clear }, (_, i) => [i]);
    table.push(null, null);
    size = minSize + 1;
  };
  reset();
  let pos = 0;
  const read = () => {
    let v = 0;
    for (let i = 0; i < size; i++, pos++) v |= ((data[pos >> 3] >> (pos & 7)) & 1) << i;
    return v;
  };
  const out = [];
  let prev = null;
  for (;;) {
    const code = read();
    if (code === clear) { reset(); prev = null; continue; }
    if (code === eoi) break;
    let entry;
    if (code < table.length && table[code]) entry = table[code];
    else entry = [...prev, prev[0]];
    out.push(...entry);
    if (prev) table.push([...prev, entry[0]]);
    prev = entry;
    if (table.length === 1 << size && size < 12) size++;
  }
  return out;
}

test('LZW decodes back to its indices, past a full table and its clear code', () => {
  // A long, varied run: enough distinct strings to fill 4096 codes.
  const idx = Array.from({ length: 60000 }, (_, i) => (i * 7919 + (i >> 5) * 31) % 256);
  assert.deepEqual(unlzw(lzw(idx, 8), 8), idx);
  const flat = new Array(5000).fill(3);
  assert.deepEqual(unlzw(lzw(flat, 8), 8), flat);
});

test('median cut keeps the colours a picture has', () => {
  const f = new Uint8Array(4 * 64);
  for (let p = 0; p < 64; p++) f.set(p < 32 ? [250, 10, 10, 255] : [10, 10, 250, 255], p * 4);
  const pal = palette(histogram([f]));
  assert.equal(pal.length, 2);
  assert.ok(pal.some(([r, , b]) => r > 240 && b < 20) && pal.some(([r, , b]) => b > 240 && r < 20), JSON.stringify(pal));
});

test('a GIF is a loop of frames with delays that keep the clip’s length', () => {
  const w = 4; const h = 2;
  const frames = Array.from({ length: 24 }, (_, k) => new Uint8Array(w * h * 4).fill(k * 10));
  const gif = encodeGif(frames, w, h, 24);
  assert.equal(new TextDecoder().decode(gif.subarray(0, 6)), 'GIF89a');
  assert.equal(gif[6] | (gif[7] << 8), w);
  assert.equal(gif.at(-1), 0x3b);
  // Graphic control extensions, one per frame: their delays add up to 1 s.
  let delays = 0;
  let count = 0;
  for (let i = 0; i < gif.length - 5; i++) {
    if (gif[i] === 0x21 && gif[i + 1] === 0xf9 && gif[i + 2] === 4) {
      delays += gif[i + 4] | (gif[i + 5] << 8);
      count++;
    }
  }
  assert.equal(count, 24);
  assert.equal(delays, 100);
});
