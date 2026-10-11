// `node --test web/js/` — the out-of-memory box's choices, which are pure: what
// it offers depends only on the server's `oom` object. Not in `web::WEB_FILES`.
//
// The case that made this exist: a 17.3 GB fp16 Klein 9B plus its 8.3 GB text
// encoder, on a machine with 26 GB free. The box offered System RAM, which then
// held 25.9 GB and died of system memory at every size.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { gttShort, headline, options, ramFits } from './memory.js';

const GB = 1e9;
const gpuOom = (extra) => ({ kind: 'gpu', phase: 'drawing', step: 2, steps: 4, mode: 'auto', ...extra });
const labels = (o) => options(o, {}).map(([label, , act]) => (act ? label : `(${label})`));

test('System RAM is offered with its figure when it fits', () => {
  const o = gpuOom({ ram_needed: 17.6 * GB, ram_free: 26 * GB });
  assert.ok(ramFits(o));
  const [label, why, act] = options(o, {})[0];
  assert.equal(label, 'Keep weights in system RAM');
  assert.ok(act);
  assert.match(why, /17\.6 GB of RAM; 26\.0 GB free/);
});

test('System RAM that cannot fit is greyed out, and the SSD offered instead', () => {
  const o = gpuOom({ ram_needed: 25.9 * GB, ram_free: 26 * GB });
  assert.ok(!ramFits(o));
  assert.deepEqual(labels(o), ['(Keep weights in system RAM)', 'Stream from SSD']);
});

test('the headroom is sd.cpp’s: the larger of 2 GiB and a tenth', () => {
  // 26 GB free keeps 2.6 GB back, so 23.4 GB fits and 23.5 does not.
  assert.ok(ramFits({ ram_needed: 23.3 * GB, ram_free: 26 * GB }));
  assert.ok(!ramFits({ ram_needed: 23.5 * GB, ram_free: 26 * GB }));
  // On a small machine the 2 GiB floor is what binds.
  assert.ok(!ramFits({ ram_needed: 6 * GB, ram_free: 8 * GB }));
});

test('unknown figures count as fitting, and say nothing they don’t know', () => {
  assert.ok(ramFits(gpuOom({})));
  const [, why] = options(gpuOom({}), {})[0];
  assert.doesNotMatch(why, /GB/);
});

test('out of system memory offers the SSD; a decode failure offers no memory mode', () => {
  assert.deepEqual(labels({ kind: 'ram', phase: 'drawing', mode: 'ram' }), ['Stream from SSD']);
  assert.deepEqual(labels(gpuOom({ phase: 'decoding' })), []);
});

test('a lost device offers a 3 GB reserve first, once, and never steps it up', () => {
  // The run this was found on: System RAM, RADV "Not enough memory for command
  // submission" — sd.cpp's own check had passed.
  const lost = { kind: 'gpu', phase: 'drawing', step: 0, steps: 0, mode: 'ram', lost: true };
  assert.equal(labels(lost)[0], 'Keep 3 GB of the card free');
  // Stepping up walked a real run to 16 GB, where it starved the budget instead.
  assert.ok(!labels({ ...lost, reserve_gb: 3 }).some((l) => l.includes('card free')));
  assert.ok(!labels(gpuOom({})).some((l) => l.includes('card free')), 'only for a lost device');
});

test('a reserve that starved the budget is lowered, not raised', () => {
  // The 16 GB run: 21.8 GB free on the card, a 5.8 GB budget, 6.3 GB needed.
  const starved = gpuOom({
    reserve_gb: 16, need_device_mb: 6337.42, avail_device_mb: 21786.55,
    need_budget_mb: 6151.92, avail_budget_mb: 5759.43,
    encoder_on_card_mb: 7669.77, references: 4,
  });
  const [label, why] = options(starved, {})[0];
  assert.equal(label, 'Stop keeping the card free');
  assert.match(why, /21\.8 GB free.*16 GB reserve.*5\.8 GB/);
  assert.ok(!labels(starved).some((l) => /Keep \d+ GB/.test(l)));
  // A full card is not a starved budget.
  assert.ok(!labels(gpuOom({ reserve_gb: 3, need_device_mb: 1254, avail_device_mb: 1145,
    need_budget_mb: 6333, avail_budget_mb: 6903 })).includes('Stop keeping the card free'));
});

test('the encoder Auto kept on the card, and the references, are named', () => {
  const o = gpuOom({ encoder_on_card_mb: 7669.77, references: 4 });
  const [, why] = options(o, {})[0];
  assert.match(why, /7\.7 GB text encoder Auto left on the card/);
  assert.equal(headline(o), 'Ran out of GPU memory while drawing (step 2 of 4), with 4 reference images.');
  // Offered only where the page can open the References dialog.
  assert.ok(!options(o, {}).some(([l]) => l === 'Fewer reference images'));
  assert.ok(options(o, { references: () => {} }).some(([l]) => l === 'Fewer reference images'));
});

test('a run that died before its first step says so, not \u201cstep 0 of 0\u201d', () => {
  assert.equal(headline(gpuOom({ step: 0, steps: 0 })), 'Ran out of GPU memory at the start of drawing.');
  assert.equal(headline(gpuOom({})), 'Ran out of GPU memory while drawing (step 2 of 4).');
  assert.equal(headline(gpuOom({ phase: 'references' })), 'Ran out of GPU memory while reading the reference images.');
});

test('a model auto-fit had already moved off the card is named in the RAM option', () => {
  const [, why] = options(gpuOom({ placement: 'model in system RAM, text encoder streamed from the SSD' }), {})[0];
  assert.match(why, /already off the card/);
});

test('a model bigger than the GPU\u2019s window onto RAM goes to the SSD, not to System RAM', () => {
  // The run that found it: fp16 Klein 9B, 17.5 GB to hold, a 15.2 GiB GTT,
  // System RAM, a lost device — and plenty of free RAM.
  const o = { kind: 'gpu', phase: 'drawing', step: 0, steps: 0, mode: 'ram', lost: true,
    ram_needed: 17.48 * GB, ram_free: 27.9 * GB, gtt_free: 16.1 * GB };
  assert.ok(gttShort(o) && !ramFits(o));
  assert.deepEqual(labels(o), ['Stream from SSD']);
  assert.match(options(o, {})[0][1], /16\.1 GB window onto RAM \(needs 17\.5 GB\)/);
  // In Auto, whose plan had put the model in RAM, the same: SSD first, RAM greyed.
  const auto = { ...o, mode: 'auto', lost: false, placement: 'model in system RAM' };
  assert.deepEqual(labels(auto), ['Stream from SSD', '(Keep weights in system RAM)']);
  // A Q8 that fits the window is still offered System RAM.
  assert.ok(ramFits({ ...o, ram_needed: 10 * GB }));
});
