// `node --test 'web/js/*.test.js'` — the Agent tab's message list
// (`chatview.js`). Not in `web::WEB_FILES`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { castHtml, castLabel, framesLabel, renderChat, liveReply, replyHtml, rowKeyHtml, sessionHtml, valueText, stateText } from './chatview.js';

const chat = (over = {}) => ({
  id: '1-0',
  messages: [],
  changes: [],
  actions: [],
  session: { state: 'idle', goal: '', checks: [], round: 0 },
  ...over,
});

test('what the model wrote is escaped, never markup', () => {
  const html = renderChat(chat({
    messages: [{ role: 'agent', n: 0, mode: 'chat', reply: '<img src=x onerror=alert(1)>', notes: '"quoted"' }],
  }));
  assert.ok(!html.includes('<img src=x'));
  assert.ok(html.includes('&lt;img src=x'));
});

test('an applied change offers Undo, and nothing asks for an answer', () => {
  const c = chat({
    changes: [
      { id: 1, by: 'agent', state: 'applied', set: { cfg_scale: 7 }, before: { cfg_scale: 6 }, reason: 'harder' },
      { id: 2, by: 'agent', state: 'applied', set: { prompt: 'a dog' }, before: { prompt: 'a cat' } },
    ],
    messages: [{ role: 'agent', n: 0, mode: 'chat', reply: 'ok', changes: [1, 2] }],
  });
  const html = renderChat(c);
  assert.ok(html.includes('data-undo="1"') && html.includes('data-undo="2"'));
  assert.ok(!/data-approve|data-reject/.test(html), 'no Approve or Reject: what is allowed applies');
  assert.ok(html.includes('6 → 7'));
});

test("a round's pictures carry the critique that followed it", () => {
  const ref = (id) => ({ kind: 'item', id, file: `${id}.png`, seed: 4 });
  const c = chat({
    messages: [
      { role: 'batch', n: 0, round: 1, summary: 'prompt "x"', images: [ref('a'), ref('b')] },
      { role: 'agent', n: 1, mode: 'critique', reply: '', checks: [{ image: 1, results: [true, false] }, { image: 2, results: [true, true] }],
        keepers: [2], passed: [2] },
    ],
  });
  const html = renderChat(c);
  assert.ok(html.includes('✓✗') && html.includes('✓✓'));
  assert.equal(html.match(/Keeper</g).length, 1);
  assert.ok(html.includes('data-open-item="a"'));
  assert.ok(html.includes('data-ask="item:a"') && !html.includes('data-describe'), 'a picture is asked about, not described by a button');
});

test('a round of one picture is drawn large, a round of several is a row', () => {
  const ref = (id) => ({ kind: 'item', id, file: `${id}.png` });
  const round = (images) => renderChat(chat({ messages: [{ role: 'batch', n: 0, round: 1, summary: '', images }] }));
  assert.ok(round([ref('a')]).includes('class="chatpics large"'));
  assert.ok(round([ref('a'), ref('b')]).includes('class="chatpics"'));
});

test('the reply shows while it is still being written', () => {
  assert.equal(liveReply('{"notes":"thinking'), 'thinking');
  assert.equal(liveReply('{"notes":"x","reply":"Here is \\"one\\"'), 'Here is "one"');
  assert.equal(liveReply('{"no'), '');
});

test("an older chat's unanswered proposal reads as set aside", () => {
  const c = chat({
    changes: [{ id: 1, by: 'agent', state: 'rejected', set: { prompt: 'a dog' } }],
    messages: [{ role: 'agent', n: 0, mode: 'chat', reply: '', changes: [1] }],
  });
  const html = renderChat(c);
  assert.ok(html.includes('Set aside') && !html.includes('data-undo'));
});

test('values and states read as words', () => {
  assert.equal(valueText('size', { width: 1024, height: 768 }), '1024×768');
  assert.equal(valueText('cfg_scale', 5.5), '5.5');
  assert.equal(stateText(chat({ session: { state: 'running', goal: 'g', round: 2, checks: [] } })), 'Working · round 2 of 3',
    'three rounds when the plan said none');
  assert.equal(stateText(chat({ session: { state: 'done', goal: 'g', round: 2, rounds: 2, checks: [] } })), 'Done · round 2 of 2');
  assert.equal(stateText(chat({ session: { state: 'running', goal: 'g', round: 5, first_round: 3, checks: [] } })), 'Working · round 2 of 3',
    'a session counts its own rounds');
});

test("a comparison's counts keep their lines, and its verdict offers Undo", () => {
  const html = renderChat(chat({
    changes: [{ id: 3, by: 'agent', state: 'applied', set: { cfg_scale: 5 }, before: { cfg_scale: 6 }, reason: 'the comparison’s best' }],
    messages: [
      { role: 'note', n: 0, text: 'Comparing CFG scale. Checks:\n  1. Is it red?\n1. CFG scale 5: 2 of 2 passed every check' },
      { role: 'agent', n: 1, mode: 'verdict', reply: 'CFG 5.', changes: [3] },
    ],
  }));
  assert.ok(html.includes('class="chatnote table"'));
  assert.ok(html.includes('data-undo="3"') && html.includes('6 → 5'));
});

test('the Session panel names what a comparison draws and the picture to match', () => {
  const html = sessionHtml(chat({
    session: { state: 'running', goal: 'Best CFG', checks: ['Is it red?'], round: 1, first_round: 0, rounds: 2,
      sweep: { knob: 'cfg_scale', values: [3, 7], before: 6 }, reference: [{ kind: 'attach', id: 'a.png', file: 'a.png' }] },
  }));
  assert.ok(html.includes('Comparing CFG') && html.includes('3 · 7'), html);
  assert.ok(html.includes('/chats/1-0/att/a.png'));
  assert.ok(html.includes('Round 1 of 2'));
  assert.ok(sessionHtml(chat()).includes('No session yet'));
});

test('bold is drawn, and only after escaping', () => {
  assert.equal(replyHtml('CFG **3, 5, 7** on <b>x</b>'), 'CFG <b>3, 5, 7</b> on &lt;b&gt;x&lt;/b&gt;');
  assert.equal(replyHtml('**<img>**'), '<b>&lt;img&gt;</b>');
});

const film = (over = {}) => ({
  title: 'Rain <b>', every: 'cinematic', video_model: 'wan5b', references: false, phase: 'ready',
  cast: [
    { name: 'Mara', kind: 'character', look: 'a red coat', picture: { kind: 'output', id: 'm', file: 'u0-0.png' }, stem: 'm' },
    { name: 'Leo"', kind: 'character', look: 'a raincoat', picture: { kind: 'item', id: '7', file: 'c1-1.png' }, stem: '7-s0' },
  ],
  shots: [
    { text: 'Mara opens the door.', cast: ['Mara'], cut: false, keyframe: { kind: 'item', id: '8', file: 'k1-0.png' } },
    { text: 'Leo waits.', cast: ['Leo"'], cut: true },
  ],
  ...over,
});

test('the cast is escaped and locks while the agent works; the shots are the Shots rows', () => {
  const html = castHtml(chat({ film: film() }));
  assert.ok(html.includes('Rain &lt;b&gt;') && html.includes('value="Leo&quot;"'));
  assert.ok(html.includes('Draw one') && html.includes('Draw again'), 'the person’s picture is drawn only if asked');
  assert.ok(!html.includes('Mara opens the door'), 'no shots here');
  assert.ok(!html.includes(' disabled'));
  assert.ok(castHtml(chat({ film: film() }), { busy: true }).includes(' disabled'));
  assert.ok(castHtml(chat()).includes('No film yet'));
  assert.ok(castHtml(null).includes('No film yet'), 'no chat yet');
  assert.ok(html.includes('2 shots for wan5b'));
});

test('a storyboard reply is a card that opens the panel, and the header says what is drawn', () => {
  const c = chat({ film: film(), messages: [{ role: 'agent', n: 0, mode: 'story', reply: 'Two shots.' }] });
  assert.ok(renderChat(c).includes('data-open-story'));
  const drawing = film({ phase: 'board', cast: [{ ...film().cast[0], stem: null, picture: null }] });
  assert.equal(stateText(chat({ film: drawing, session: { state: 'running', next: 'film', goal: '', checks: [], round: 0 } })), 'Drawing the cast');
  assert.equal(stateText(chat({ film: film(), session: { state: 'running', next: 'film', goal: '', checks: [], round: 0 } })), 'Drawing the first frames');
});

// What the panel offers to draw next: the cast's button, else the frames'.
const drawLabel = (f) => castLabel(f) ?? framesLabel(f);

test('the panel offers to draw only what the board lacks, and the card shows the cast', () => {
  const whole = film({ shots: film().shots.map((x) => ({ ...x, key_stem: 'k' })) });
  assert.equal(drawLabel(whole), null, 'everything is there');
  const bare = (m) => ({ ...m, stem: null, picture: null });
  assert.equal(drawLabel(film({ phase: 'board', cast: film().cast.map(bare) })), 'Draw the cast');
  assert.equal(drawLabel(film({ phase: 'board', cast: [film().cast[0], bare(film().cast[1])] })), 'Draw the rest of the cast');
  assert.equal(drawLabel(film({ phase: 'board' })), 'Draw the first frames', 'shot 2 is a cut on a model without references');
  assert.equal(drawLabel(film({ phase: 'board', references: true })), null, 'H3 cuts need none');
  const c = chat({ film: film(), messages: [{ role: 'agent', n: 0, mode: 'story', reply: '' }] });
  assert.ok(renderChat(c).includes('/chats/1-0/img/c1-1.png'));
});

test('a first frame drawn with a cast picture since redrawn is drawn again', () => {
  const marks = (f, s) => s.cast.map((n) => `${f.cast.find((m) => m.name === n).stem}|`);
  const f0 = film({ phase: 'ready' });
  const drawn = { ...f0, shots: f0.shots.map((x) => ({ ...x, key_stem: 'k', key_cast: marks(f0, x) })) };
  assert.equal(drawLabel(drawn), null);
  assert.equal(drawLabel({ ...drawn, shots: drawn.shots.map(({ key_cast, ...x }) => x) }), null, 'an old board is not stale');
  const redrawn = { ...drawn, cast: drawn.cast.map((m) => ({ ...m, stem: `${m.stem}-2` })) };
  assert.equal(drawLabel(redrawn), 'Draw the first frames');
  const words = { ...drawn, shots: drawn.shots.map((x, k) => (k ? x : { ...x, no_key: true, key_stem: null, keyframe: null })) };
  assert.equal(drawLabel(words), null, 'shot 1 from its words needs nothing drawn');
});

test('a storyboard row offers its first frame drawn again, or one drawn, on a model whose cuts start from one', () => {
  const row = { fresh: true, key: 'k0', key_drawn: true, key_stale: false, no_key: false };
  assert.ok(rowKeyHtml(row, 0, false).includes('data-board-redraw="0"'));
  assert.equal(rowKeyHtml({ ...row, key_drawn: false }, 0, false), '', 'the person’s own is theirs to change');
  assert.ok(rowKeyHtml({ ...row, key_stale: true }, 0, false).includes('Its cast changed'));
  assert.ok(rowKeyHtml({ ...row, key: null, no_key: true }, 2, false).includes('data-board-addkey="2"'));
  assert.equal(rowKeyHtml(row, 0, true), '', 'H3 follows the cast instead');
  assert.equal(rowKeyHtml({ ...row, fresh: false }, 1, false), '', 'a continue goes on from the last frame');
  assert.equal(rowKeyHtml(undefined, 3, false), '', 'a row the board doesn’t have yet');
});

test('on a model that follows references a character is whole only with its turnaround', () => {
  const sheets = film({ sheets: true, shots: film().shots.map((x) => ({ ...x, key_stem: 'k' })) });
  assert.equal(drawLabel(sheets), 'Draw the cast', 'no sheets yet');
  const withSheets = { ...sheets, cast: sheets.cast.map((m) => ({ ...m, sheet: { kind: 'item', id: 's', file: `t-${m.stem}.png` }, sheet_stem: 's' })) };
  assert.equal(drawLabel(withSheets), null);
  const html = castHtml(chat({ film: withSheets }), { models: [{ name: 'klein', label: 'FLUX.2 Klein 4B', references: true }] });
  assert.ok(html.includes('t-m.png') && html.includes('(turnarounds)') && html.includes('turnaround sheet'));
  assert.ok(renderChat(chat({ film: withSheets, messages: [{ role: 'agent', n: 0, mode: 'story', reply: '' }] })).includes('t-m.png'),
    'the card shows the turnaround');
});

test('drawing the cast and the first frames is optional', () => {
  const whole = film({ shots: film().shots.map((x) => ({ ...x, key_stem: 'k' })) });
  const bare = (m) => ({ ...m, stem: null, picture: null });
  const nothing = film({ phase: 'board', cast: film().cast.map(bare) });
  assert.equal(framesLabel(nothing), 'Draw the first frames', 'offered beside the cast’s, which it draws first');
  assert.equal(castLabel(nothing), 'Draw the cast');
  assert.equal(castLabel(whole), null, 'the cast is drawn');
  assert.ok(!castHtml(chat({ film: whole })).includes(' disabled'), 'still editable once ready');
  // An older chat's filmed board reads as ready; its clip card stays.
  const card = renderChat(chat({ messages: [{ role: 'clip', n: 0, id: 'c<1', title: 'Rain' }] }));
  assert.ok(card.includes('data-open-clip="c&lt;1"') && card.includes('/videos/c%3C1/thumb.png'));
});

test("an H3 film's characters take a voice; a Wan film's don't", () => {
  const cast = [{ ...film().cast[0], voice: '100-1-sound' }, film().cast[1]];
  const html = castHtml(chat({ film: film({ references: true, cast }) }));
  assert.ok(html.includes('Voice: 100-1-sound') && html.includes('data-film-voiceplay="100-1-sound"') && html.includes('data-film-novoice="0"'));
  assert.ok(html.includes('Voice: H3 picks one') && html.includes('data-film-voicepick="1"'));
  assert.ok(!castHtml(chat({ film: film({ cast }) })).includes('data-film-voicepick'));
});
