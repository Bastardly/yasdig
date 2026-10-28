// `node --test web/js/` — the Shots panel's done marks against the clip on
// the stage (`shots.js`). Not in `web::WEB_FILES`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { shotState, sentRow } from './shots.js';

const seg = (text, k) => ({ text, seed: 10 + k, frames: 33, start: k * 32 });
const clip = (...texts) => ({ segments: texts.map(seg) });
// The rows a drawn clip fills the panel with.
const rows = (...texts) => texts.map((text, k) => ({ text, seed: 10 + k, frames: 33, cut: false, refs: [] }));

test('a clip made from the panel has every row done, and nothing to draw', () => {
  const s = shotState(rows('a crab walks', 'it lifts a claw'), clip('a crab walks', 'it lifts a claw'), true);
  assert.equal(s.done, 2);
  assert.ok(s.whole);
  assert.equal(s.action, 'none');
  assert.equal(s.dropLast, 1);
});

test('a row added under a whole clip is pending, and Extend draws it', () => {
  const r = [...rows('a crab walks', 'it lifts a claw'), { text: ' it waves ', seed: -1, frames: 33 }];
  const s = shotState(r, clip('a crab walks', 'it lifts a claw'), true);
  assert.equal(s.done, 2);
  assert.equal(s.pending.length, 1);
  assert.equal(s.action, 'extend');
});

test('a new seed, a new length or new words on a done row redo from it', () => {
  const c = clip('a crab walks', 'it lifts a claw', 'it waves');
  for (const change of [{ seed: -1 }, { seed: 99 }, { frames: 49 }, { text: 'it lifts both claws' }]) {
    const r = rows('a crab walks', 'it lifts a claw', 'it waves');
    Object.assign(r[1], change);
    const s = shotState(r, c, true);
    assert.equal(s.done, 1, JSON.stringify(change));
    assert.equal(s.action, 'redo');
    assert.equal(s.pending.length, 2, 'the changed row and every one after it');
    assert.equal(s.dropLast, -1);
  }
});

test('a cut, or other references, is a different shot', () => {
  const c = clip('a dwarf walks', 'he comes back');
  const r = rows('a dwarf walks', 'he comes back');
  r[1].cut = true;
  assert.equal(shotState(r, c, true).action, 'redo');
  r[1].refs = ['100-7-s0'];
  assert.equal(shotState(r, { segments: [seg('a dwarf walks', 0), { ...seg('he comes back', 1), cut: true }] }, true).action, 'redo');
  // Shot 1's row can say cut: it starts fresh either way.
  const first = rows('a dwarf walks');
  first[0].cut = true;
  assert.equal(shotState(first, clip('a dwarf walks'), true).action, 'none');
});

test('other sounds are a different shot', () => {
  const c = { segments: [seg('a dwarf walks', 0), { ...seg('he sings', 1), cut: true, audio: ['100-1-sound'] }] };
  const r = rows('a dwarf walks', 'he sings');
  r[1].cut = true;
  r[1].audio = ['100-1-sound'];
  assert.equal(shotState(r, c, true).action, 'none');
  r[1].audio = [];
  assert.equal(shotState(r, c, true).action, 'redo');
});

test('shot 1 is the clip: changing it is a Generate, and it is never cut', () => {
  const r = rows('a crab runs', 'it lifts a claw');
  assert.equal(shotState(r, clip('a crab walks', 'it lifts a claw'), true).action, 'generate');
  assert.equal(shotState(rows('a crab walks'), clip('a crab walks'), true).dropLast, -1);
});

test('no clip, or one that cannot chain, never extends or redoes', () => {
  assert.equal(shotState(rows('a crab', 'it waves'), null, true).action, 'generate');
  assert.equal(shotState(rows('a crab', 'it waves'), clip('a crab'), false).action, 'none');
  assert.equal(shotState(rows('a crab', 'it runs'), clip('a crab', 'it waves'), false).action, 'generate');
  assert.equal(shotState(rows('a crab'), null, true).done, 0);
});

test('blank rows are not shots', () => {
  const r = [...rows('a crab'), { text: '', seed: -1, frames: 33 }, { text: '  ', seed: -1, frames: 33 }];
  const s = shotState(r, clip('a crab'), true);
  assert.deepEqual(s.pending, []);
  assert.equal(s.action, 'none');
});

test('a cut from a picture is done only with that picture', () => {
  const c = { segments: [seg('a hall', 0), { ...seg('the hall at night', 1), cut: true, keyframe: 'k1' }] };
  const r = rows('a hall', 'the hall at night');
  Object.assign(r[1], { cut: true, key: 'k1' });
  assert.equal(shotState(r, c, true).done, 2);
  r[1].key = 'k2';
  const s = shotState(r, c, true);
  assert.equal(s.done, 1);
  assert.equal(s.action, 'redo');
});

test('what is said, heard or scored, or who is pictured, changed on a done row redoes it', () => {
  const c = clip('a crab walks', 'it lifts a claw');
  c.segments[1].lines = [{ who: 'Crab', words: 'Hello' }];
  const r = rows('a crab walks', 'it lifts a claw');
  r[1].lines = [{ who: 'Crab', words: 'Hello', language: '', voiceover: false }];
  r[1].sound = '';
  assert.equal(shotState(r, c, true).done, 2, 'absent and empty alike');
  for (const change of [{ lines: [] }, { sound: 'waves' }, { music: 'a harp' }, { cast: ['Crab'] }]) {
    const s = shotState([r[0], { ...r[1], ...change }], c, true);
    assert.equal(s.done, 1, JSON.stringify(change));
  }
});

test("shot 1's image is the clip's first frame: another one makes it again", () => {
  const c = { ...clip('a crab walks'), source: '100-1' };
  const r = rows('a crab walks');
  assert.equal(shotState(r, c, true).action, 'generate', 'no image where the clip had one');
  r[0].key = '100-1';
  assert.equal(shotState(r, c, true).action, 'none');
});

test('a name goes with the reference or sound it names, and changing it redoes the shot', () => {
  const pic = { references: ['100-2'], subjects: [{ name: 'Crab', look: 'red' }], audio: ['100-3'], voices: ['Crab'] };
  const c = { segments: [seg('a crab walks', 0), { ...seg('it sings', 1), cut: true, ...pic }] };
  const r = rows('a crab walks', 'it sings');
  Object.assign(r[1], { cut: true, refs: ['100-2'], subjects: pic.subjects, audio: ['100-3'], voices: ['Crab'] });
  assert.equal(shotState(r, c, true).action, 'none');
  for (const change of [{ subjects: [{ name: 'Lobster', look: 'red' }] }, { voices: ['Lobster'] }]) {
    assert.equal(shotState([r[0], { ...r[1], ...change }], c, true).done, 1, JSON.stringify(change));
  }
  // A name with nothing to name isn't sent.
  assert.deepEqual(sentRow({ ...r[1], refs: [], audio: [] }, 1).subjects, []);
});

test("a row holds what every model takes, and sends its model's part", () => {
  const row = { text: 'she turns', cut: true, refs: ['100-2'], subjects: [{ name: 'Mara', look: '' }], audio: ['100-3'],
    voices: ['Mara'], key: '100-4', lines: [{ who: 'Mara', words: 'Hi' }], sound: 'rain', cast: ['Mara'] };
  const wan = sentRow(row, 1, { references: false, sounds: false });
  assert.deepEqual([wan.refs, wan.audio, wan.key, wan.subjects, wan.lines, wan.sound], [[], [], '100-4', [], [], '']);
  const h3 = sentRow(row, 1, { references: true, sounds: true });
  assert.deepEqual([h3.refs, h3.audio, h3.key, h3.voices], [['100-2'], ['100-3'], null, ['Mara']], 'Ref2VA, never beside -i');
  const quiet = sentRow(row, 1, { references: true, sounds: false });
  assert.deepEqual([quiet.refs, quiet.audio, quiet.key], [['100-2'], [], null]);
  const plain = sentRow({ ...row, refs: [], audio: [] }, 1, { references: true, sounds: true });
  assert.equal(plain.key, '100-4', 'with nothing to follow, its image');
  assert.deepEqual(sentRow(row, 1).refs, ['100-2']);
  const cont = sentRow({ ...row, cut: false }, 1, { references: true, sounds: true });
  assert.deepEqual([cont.refs, cont.key], [[], null], 'a continue goes on from the last frame');
  // A Wan clip drawn from such a row is done: what it couldn't take is no change.
  const c = { segments: [seg('a', 0), { ...seg('she turns', 1), cut: true, keyframe: '100-4' }] };
  const rs = [{ ...rows('a')[0] }, { ...row, seed: 11, frames: 33 }];
  assert.equal(shotState(rs, c, true, { references: false, sounds: false }).action, 'none');
  assert.equal(shotState(rs, c, true).action, 'redo', 'on H3 it would follow the references instead');
});
