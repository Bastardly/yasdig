// `node --test 'web/js/*.test.js'` — the Agent tab's message list
// (`chatview.js`). Not in `web::WEB_FILES`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { renderChat, liveReply, valueText, stateText } from './chatview.js';

const chat = (over = {}) => ({
  id: '1-0',
  messages: [],
  changes: [],
  actions: [],
  permissions: { rounds: 3 },
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

test('a pending change asks, an applied one offers Undo', () => {
  const c = chat({
    changes: [
      { id: 1, by: 'agent', state: 'pending', set: { cfg_scale: 7 }, before: { cfg_scale: 6 }, reason: 'harder' },
      { id: 2, by: 'agent', state: 'applied', set: { prompt: 'a dog' }, before: { prompt: 'a cat' } },
    ],
    messages: [{ role: 'agent', n: 0, mode: 'chat', reply: 'ok', changes: [1, 2] }],
  });
  const html = renderChat(c);
  assert.ok(html.includes('data-approve-change="1"') && html.includes('data-reject-change="1"'));
  assert.ok(html.includes('data-undo="2"'));
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
});

test('the reply shows while it is still being written', () => {
  assert.equal(liveReply('{"notes":"thinking'), 'thinking');
  assert.equal(liveReply('{"notes":"x","reply":"Here is \\"one\\"'), 'Here is "one"');
  assert.equal(liveReply('{"no'), '');
});

test('values and states read as words', () => {
  assert.equal(valueText('size', { width: 1024, height: 768 }), '1024×768');
  assert.equal(valueText('cfg_scale', 5.5), '5.5');
  assert.equal(stateText(chat({ session: { state: 'running', goal: 'g', round: 2, checks: [] } })), 'Working · round 2 of 3');
});
