// `node --test web/js/` — the Prompts panel's done marks against the clip on
// the stage (`shots.js`). Not in `web::WEB_FILES`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { shotState } from './shots.js';

const clip = (...texts) => ({ segments: texts.map((text, k) => ({ text, start: k * 33 })) });

test('a clip made from the panel has every row done, and nothing to extend', () => {
  const s = shotState(['a crab walks', 'it lifts a claw'], clip('a crab walks', 'it lifts a claw'), true);
  assert.equal(s.done, 2);
  assert.ok(s.whole);
  assert.ok(!s.canExtend);
  assert.equal(s.dropLast, 1);
});

test('a row added under a whole clip is pending, and Extend draws it', () => {
  const s = shotState(['a crab walks', 'it lifts a claw', ' it waves '], clip('a crab walks', 'it lifts a claw'), true);
  assert.equal(s.done, 2);
  assert.deepEqual(s.pending, ['it waves']);
  assert.ok(s.canExtend);
});

test('editing a done row makes it and every row after it pending, and Extend goes', () => {
  const s = shotState(['a crab walks', 'it lifts both claws', 'it waves'], clip('a crab walks', 'it lifts a claw'), true);
  assert.equal(s.done, 1);
  assert.ok(!s.whole);
  assert.ok(!s.canExtend, 'a clip is only extended at its end');
  assert.equal(s.dropLast, -1);
});

test('shot 1 is the clip: it can never be cut, only redrawn by Generate', () => {
  assert.equal(shotState(['a crab walks'], clip('a crab walks'), true).dropLast, -1);
});

test('no clip, or one that cannot chain, never extends', () => {
  assert.ok(!shotState(['a crab', 'it waves'], null, true).canExtend);
  assert.ok(!shotState(['a crab', 'it waves'], clip('a crab'), false).canExtend);
  assert.equal(shotState(['a crab'], null, true).done, 0);
});

test('blank rows are not shots', () => {
  const s = shotState(['a crab', '', '  '], clip('a crab'), true);
  assert.deepEqual(s.pending, []);
  assert.ok(!s.canExtend);
});
