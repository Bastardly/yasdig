import { test } from 'node:test';
import assert from 'node:assert/strict';
import { encodeWav, fitSound, MAX_SOUND_S } from './wavenc.js';

test('a WAV is 16-bit PCM with its sizes right and samples interleaved', () => {
  const left = Float32Array.from([0, 1, -1]);
  const right = Float32Array.from([0.5, -0.5, 2]);
  const w = encodeWav([left, right], 32000);
  const v = new DataView(w.buffer);
  const tag = (at) => String.fromCharCode(...w.slice(at, at + 4));
  assert.equal(tag(0), 'RIFF');
  assert.equal(tag(8), 'WAVE');
  assert.equal(v.getUint32(4, true), w.length - 8);
  assert.equal(v.getUint16(20, true), 1);
  assert.equal(v.getUint16(22, true), 2);
  assert.equal(v.getUint32(24, true), 32000);
  assert.equal(v.getUint16(34, true), 16);
  assert.equal(v.getUint32(40, true), 3 * 2 * 2);
  const s = (i) => v.getInt16(44 + i * 2, true);
  assert.deepEqual([s(0), s(1), s(2), s(3), s(4), s(5)], [0, 16384, 32767, -16384, -32768, 32767]);
});

test("a sound is H3's length: too short refused, too long trimmed", () => {
  assert.match(fitSound(1.2).error, /at least 2 seconds; this one is 1.2 s/);
  assert.ok(fitSound(NaN).error);
  assert.deepEqual(fitSound(4), { seconds: 4, trimmed: false });
  assert.deepEqual(fitSound(40), { seconds: MAX_SOUND_S, trimmed: true });
});
