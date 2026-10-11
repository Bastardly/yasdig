// `node --test web/js/` — Node's built-in runner, so no npm, no package.json and
// no build step. This file is deliberately *not* in `web::WEB_FILES`, which is a
// whole-name whitelist, so the browser cannot fetch it.
//
// The whole point of prompt-weight.js being pure is this file: the boundary
// cases below are all things that read as "the key did nothing" or "the key ate
// my prompt" in a browser, and every one of them is an off-by-one in a string.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { adjust, STEP } from './prompt-weight.js';

const UP = STEP;
const DOWN = -STEP;

// Drive it the way a keypress does: a caret at the first occurrence of `at`,
// or a selection of it when `sel` is given, and return just the new text.
function press(text, at, delta, sel = false) {
  const i = text.indexOf(at);
  assert.notEqual(i, -1, `fixture does not contain ${at}`);
  const r = adjust(text, i, sel ? i + at.length : i, delta);
  return r && r.text;
}

test('a bare word wraps at 1.1', () => {
  assert.equal(press('a lovely crab', 'crab', UP), 'a lovely (crab:1.1)');
});

test('down from plain text goes to 0.9', () => {
  assert.equal(press('a lovely crab', 'crab', DOWN), 'a lovely (crab:0.9)');
});

test('the caret anywhere in the word finds the same word', () => {
  const t = 'a lovely crab';
  for (let i = 9; i <= 13; i++) {
    assert.equal(adjust(t, i, i, UP).text, 'a lovely (crab:1.1)', `caret ${i}`);
  }
});

test('an existing weight is retuned, not nested', () => {
  assert.equal(press('a (crab:1.2) here', 'crab', UP), 'a (crab:1.3) here');
  assert.equal(press('a (crab:1.2) here', 'crab', DOWN), 'a (crab:1.1) here');
});

test('exactly 1.0 unwraps back to plain text', () => {
  assert.equal(press('a (crab:1.1) here', 'crab', DOWN), 'a crab here');
  assert.equal(press('a (crab:0.9) here', 'crab', UP), 'a crab here');
});

test('up and down is a round trip, wrapper and all', () => {
  const start = 'a lovely crab, in focus';
  const up = adjust(start, 9, 9, UP);
  const back = adjust(up.text, up.start, up.end, DOWN);
  assert.equal(back.text, start);
});

test('a bare (x) is read as 1.1 rather than re-wrapped', () => {
  assert.equal(press('a (crab) here', 'crab', UP), 'a (crab:1.2) here');
  assert.equal(press('a (crab) here', 'crab', DOWN), 'a crab here');
});

test('[x] is read as 0.9, not nested into a cancelling 1.0', () => {
  assert.equal(press('a [crab] here', 'crab', DOWN), 'a (crab:0.8) here');
  assert.equal(press('a [crab] here', 'crab', UP), 'a crab here');
});

test('a selection is weighted as one phrase', () => {
  assert.equal(
    press('crab, cinematic lighting, sharp', 'cinematic lighting', UP, true),
    'crab, (cinematic lighting:1.1), sharp',
  );
});

test('a selection that took the brackets with it retunes the group', () => {
  assert.equal(
    press('a (crab:1.2) here', '(crab:1.2)', UP, true),
    'a (crab:1.3) here',
  );
});

test('a word inside a longer group nests, because nesting is what was asked', () => {
  // sd.cpp multiplies a nested weight (util.cpp:890-899), so this is 1.32 on
  // `crab` and 1.2 on `red` — which is the point of pointing at one word.
  assert.equal(
    press('(a red crab:1.2)', 'crab', UP),
    '(a red (crab:1.1):1.2)',
  );
});

test('the selection comes back as the inner text, so a held key is idempotent', () => {
  let t = 'a lovely crab';
  let r = adjust(t, 9, 9, UP);
  assert.equal(r.text.slice(r.start, r.end), 'crab');
  for (let n = 0; n < 4; n++) r = adjust(r.text, r.start, r.end, UP);
  assert.equal(r.text, 'a lovely (crab:1.5)');
  assert.equal(r.text.slice(r.start, r.end), 'crab');
});

test('a held key through 1.0 keeps hold of the word', () => {
  let r = adjust('a lovely (crab:1.2)', 9, 19, DOWN);
  for (let n = 0; n < 3; n++) r = adjust(r.text, r.start, r.end, DOWN);
  assert.equal(r.text, 'a lovely (crab:0.8)');
  assert.equal(r.text.slice(r.start, r.end), 'crab');
});

test('the weight never leaves the 0.1 grid', () => {
  // 1.1 + 0.1 is 1.2000000000000002, and unsnapped that is what lands in the
  // box — and then every later step is built on the wrong number.
  let r = adjust('crab', 0, 4, UP);
  for (let n = 0; n < 12; n++) r = adjust(r.text, r.start, r.end, UP);
  assert.match(r.text, /^\(crab:\d\.\d\)$/);
  assert.equal(r.text, '(crab:2.3)');
});

test('it is unclamped, and prints no negative zero', () => {
  let r = adjust('crab', 0, 4, DOWN);
  for (let n = 0; n < 9; n++) r = adjust(r.text, r.start, r.end, DOWN);
  assert.equal(r.text, '(crab:0.0)');
  r = adjust(r.text, r.start, r.end, DOWN);
  assert.equal(r.text, '(crab:-0.1)');
});

test('a caret with a delimiter on both sides is not our keystroke', () => {
  assert.equal(adjust('a  crab', 2, 2, UP), null);       // between two spaces
  assert.equal(adjust('crab, sharp', 5, 5, UP), null);   // between , and space
  assert.equal(adjust('', 0, 0, UP), null);
});

test('a caret at either end of a word is still in the word', () => {
  // Where every text editor puts it, and where the caret lands after
  // double-clicking or arrowing to the end of a word.
  assert.equal(press('a lovely crab', 'crab', UP), 'a lovely (crab:1.1)');
  assert.equal(adjust('a lovely crab', 8, 8, UP).text, 'a (lovely:1.1) crab');
  assert.equal(adjust('crab, sharp', 4, 4, UP).text, '(crab:1.1), sharp');
});

test('an escaped paren is text, not a group', () => {
  // `\(` is the one escape sd.cpp honours (util.cpp:976), so what is inside is
  // literal and must not be mistaken for a weighted group.
  assert.equal(press('a \\(crab\\) here', 'crab', UP), 'a \\((crab:1.1)\\) here');
});

test('an unbalanced bracket terminates rather than matching', () => {
  assert.equal(press('a (crab here', 'crab', UP), 'a ((crab:1.1) here');
  assert.equal(press('a crab) here', 'crab', UP), 'a (crab:1.1)) here');
});

test('a hand-typed weight off the grid is snapped onto it', () => {
  assert.equal(press('(crab:1.25)', 'crab', UP), '(crab:1.4)');
});

test('the negative prompt is the same code path', () => {
  assert.equal(press('watermark, blurry', 'watermark', UP), '(watermark:1.1), blurry');
});
