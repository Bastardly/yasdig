import { test } from 'node:test';
import assert from 'node:assert/strict';
import { linesText, parseLines } from './lines.js';

test('the lines read back as they were written', () => {
  const lines = [
    { who: 'Mara', words: 'Who\'s there?', language: '', voiceover: false },
    { who: 'Leo', words: 'Hej.', language: 'Danish', voiceover: true },
  ];
  assert.equal(linesText(lines), "Mara: Who's there?\nLeo (voiceover, Danish): Hej.");
  assert.deepEqual(parseLines(linesText(lines) + '\n\n  '), lines);
  assert.deepEqual(parseLines('Just words'), [{ who: '', words: 'Just words', language: '', voiceover: false }], 'the server says why');
  assert.equal(linesText(), '');
});
