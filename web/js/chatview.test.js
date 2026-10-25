// `node --test 'web/js/*.test.js'` — the Agent tab's message list
// (`chatview.js`). Not in `web::WEB_FILES`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { renderChat, liveReply, replyHtml, sessionHtml, valueText, stateText } from './chatview.js';

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
