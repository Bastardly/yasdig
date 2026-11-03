import { test } from 'node:test';
import assert from 'node:assert/strict';
import { namedIn } from './castnames.js';

const cast = [
  { id: 'a', name: 'Elias' },
  { id: 'b', name: 'Sally' },
  { id: 'c', name: 'Rain' },
  { id: 'd', name: 'Zoë' },
  { id: 'e', name: 'Magic Sword' },
];
const ids = (list) => list.map((e) => e.id);

test('a name is found as a whole word, in the order the prompt names them', () => {
  assert.deepEqual(ids(namedIn('Sally watches Elias light a cigar.', cast)), ['b', 'a']);
  assert.deepEqual(ids(namedIn('Eliason and Sallyanne', cast)), []);
  assert.deepEqual(ids(namedIn("Elias's hat", cast)), ['a']);
});

test('case is kept, so an ordinary word never reads as a name', () => {
  assert.deepEqual(ids(namedIn('rain on the window', cast)), []);
  assert.deepEqual(ids(namedIn('Rain stands at the window', cast)), ['c']);
});

test('names of any script and of several words', () => {
  assert.deepEqual(ids(namedIn('Zoë holds the Magic Sword.', cast)), ['d', 'e']);
  assert.deepEqual(ids(namedIn('Zoëy', cast)), []);
});

test('picked ones are not offered again', () => {
  assert.deepEqual(ids(namedIn('Elias and Sally', cast, ['a'])), ['b']);
});
