import { test } from 'node:test';
import assert from 'node:assert/strict';
import { hueWeight, oklch, oklchToSrgb, weight } from './colorrange.js';

// The same table as `the_fixture_table_the_page_checks_too` in
// src/colorrange.rs: change both or neither.
const RED = { hue: { a: 340, b: 350, c: 400, d: 410 }, lightness: null, saturation: null };
const SHADOWS = { hue: null, lightness: { a: 0, b: 0, c: 30, d: 60 }, saturation: null };

test('the fixture table the server checks too', () => {
  const table = [
    [RED, [220, 30, 40], 1.0],
    [RED, [30, 60, 220], 0.0],
    [RED, [128, 128, 128], 0.0],
    [SHADOWS, [10, 10, 10], 1.0],
    [SHADOWS, [250, 250, 250], 0.0],
    [SHADOWS, [80, 80, 80], 0.593],
  ];
  for (const [range, rgb, want] of table) {
    const got = weight(range, ...rgb);
    assert.ok(Math.abs(got - want) < 0.005, `${rgb}: ${got} not ${want}`);
  }
});

test('a hue range wraps past 360', () => {
  const r = RED.hue;
  assert.equal(hueWeight(r, 355), 1);
  assert.equal(hueWeight(r, 30), 1);
  assert.equal(hueWeight(r, 45), 0.5);
  assert.equal(hueWeight(r, 345), 0.5);
  assert.equal(hueWeight(r, 180), 0);
});

test('drawing colours read back as the hue they were drawn at', () => {
  for (const h of [0, 90, 180, 270]) {
    const [hue] = oklch(...oklchToSrgb(70, 30, h));
    const d = Math.abs(((hue - h + 540) % 360) - 180);
    assert.ok(d < 3, `${h}° came back as ${hue}°`);
  }
});
