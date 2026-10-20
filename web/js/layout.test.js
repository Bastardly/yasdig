import { test } from 'node:test';
import assert from 'node:assert/strict';
import { MIN_SIDE, bboxOf, moved, rectFrom, resized } from './layout.js';

// The same numbers as `src/layout.rs`'s tests: the model card's 1536×1024
// cup, and the README's 1024² title at 1024² and at 2048².
test('a box becomes the pixels the server sends', () => {
  const r = (x0, y0, x1, y1, w, h) => [x0 / w, y0 / h, x1 / w, y1 / h];
  assert.deepEqual(bboxOf(r(176, 224, 800, 864, 1536, 1024), 1536, 1024), [-288, -592, 352, 32]);
  const title = r(128, 96, 896, 224, 1024, 1024);
  assert.deepEqual(bboxOf(title, 1024, 1024), [-416, -384, -288, 384]);
  assert.deepEqual(bboxOf(title, 2048, 2048), [-832, -768, -576, 768]);
  assert.deepEqual(bboxOf([0.1001, 0.5, 0.1002, 0.5001], 1000, 1000), [0, -400, 1, -399]);
});

test('a drag spans its box whichever way it went, inside the frame', () => {
  assert.deepEqual(rectFrom([0.8, 0.6], [0.2, 0.1]), [0.2, 0.1, 0.8, 0.6]);
  assert.deepEqual(rectFrom([0.5, 0.5], [1.4, -0.2]), [0.5, 0, 1, 0.5]);
});

test('a moved box keeps its size and stops at the edge', () => {
  const near = (a, b) => a.every((v, i) => Math.abs(v - b[i]) < 1e-9);
  assert.ok(near(moved([0.1, 0.1, 0.3, 0.4], 0.2, 0.1), [0.3, 0.2, 0.5, 0.5]));
  assert.ok(near(moved([0.1, 0.1, 0.3, 0.4], 0.9, -0.5), [0.8, 0, 1, 0.3]));
});

test('a resize never turns a box inside out', () => {
  assert.deepEqual(resized([0.2, 0.2, 0.6, 0.6], 'se', 0.8, 0.9), [0.2, 0.2, 0.8, 0.9]);
  assert.deepEqual(resized([0.2, 0.2, 0.6, 0.6], 'nw', 0.1, 0.0), [0.1, 0, 0.6, 0.6]);
  // Dragged past the opposite corner: held at the smallest side.
  const [x0, y0, x1, y1] = resized([0.2, 0.2, 0.6, 0.6], 'nw', 0.9, 0.9);
  assert.ok(Math.abs(x1 - x0 - MIN_SIDE) < 1e-9 && Math.abs(y1 - y0 - MIN_SIDE) < 1e-9);
  assert.deepEqual(resized([0.2, 0.2, 0.6, 0.6], 'ne', 2, -1), [0.2, 0, 1, 0.6]);
});
