// `node --test web/js/` — the MP4 and WebM writers (`mp4mux.js`,
// `webmmux.js`), fed made-up chunks (Node has no WebCodecs) and read back
// box by box, element by element. Not in `web::WEB_FILES`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mp4Muxer } from './mp4mux.js';
import { webmMuxer } from './webmmux.js';

const dv = (b) => new DataView(b.buffer, b.byteOffset, b.byteLength);
const type = (b, at) => String.fromCharCode(...b.subarray(at + 4, at + 8));

// The boxes inside `b[from..to)`, as {type, at, size}.
function boxes(b, from = 0, to = b.length) {
  const out = [];
  for (let at = from; at < to;) {
    const size = dv(b).getUint32(at);
    out.push({ type: type(b, at), at, size });
    assert.ok(size >= 8, `box at ${at} has size ${size}`);
    at += size;
  }
  return out;
}

// The `nth` box of `type` inside `parent` (the whole file when null).
function child(b, parent, t, nth = 0) {
  const from = parent ? parent.at + 8 : 0;
  const to = parent ? parent.at + parent.size : b.length;
  const hit = boxes(b, from, to).filter((x) => x.type === t)[nth];
  assert.ok(hit, `no ${t}`);
  return hit;
}

// A track's sample table.
const stblOf = (b, trak) => ['mdia', 'minf', 'stbl'].reduce((p, t) => child(b, p, t), trak);

const frame = (k) => new Uint8Array(100 + k).fill(k);

test('an MP4 is ftyp, mdat, moov, with every sample where its table says', () => {
  const m = mp4Muxer({
    video: { codec: 'avc1', width: 640, height: 360, fps: 24, description: new Uint8Array([1, 0x42, 0, 0x1f, 0xff]) },
    audio: { sampleRate: 48000, channels: 2, preSkip: 312 },
  });
  for (let k = 0; k < 48; k++) m.add('video', frame(k), (k * 1e6) / 24, k % 24 === 0);
  for (let k = 0; k < 100; k++) m.add('audio', new Uint8Array(40).fill(200), k * 20000);
  const f = m.finish();
  assert.deepEqual(boxes(f).map((x) => x.type), ['ftyp', 'mdat', 'moov']);

  const moov = child(f, null, 'moov');
  const sb = stblOf(f, child(f, moov, 'trak', 0));
  const inside = (t) => boxes(f, sb.at + 8, sb.at + sb.size).find((x) => x.type === t);
  // Sizes and offsets: sample k's bytes are where stco says.
  const stsz = inside('stsz');
  assert.equal(dv(f).getUint32(stsz.at + 16), 48);
  const stco = inside('stco');
  for (const k of [0, 1, 47]) {
    const off = dv(f).getUint32(stco.at + 16 + 4 * k);
    assert.equal(dv(f).getUint32(stsz.at + 20 + 4 * k), 100 + k);
    assert.equal(f[off], k, `sample ${k} at ${off}`);
  }
  // One frame a tick at 24 a second: a single stts run.
  const stts = inside('stts');
  assert.deepEqual([dv(f).getUint32(stts.at + 12), dv(f).getUint32(stts.at + 16), dv(f).getUint32(stts.at + 20)], [1, 48, 1]);
  // Keyframes 1 and 25.
  const stss = inside('stss');
  assert.deepEqual([dv(f).getUint32(stss.at + 12), dv(f).getUint32(stss.at + 16), dv(f).getUint32(stss.at + 20)], [2, 1, 25]);
  assert.ok(!inside('ctts'), 'no reordering, no offsets');

  // The movie is 2 s long.
  const mvhd = child(f, moov, 'mvhd');
  assert.equal(dv(f).getUint32(mvhd.at + 20), 1000);
  assert.equal(dv(f).getUint32(mvhd.at + 24), 2000);
  // The sound track's entry is Opus, with a dOps.
  const at = child(f, moov, 'trak', 1);
  assert.ok(child(f, stblOf(f, at), 'stsd'));
  assert.ok(new TextDecoder().decode(f.subarray(at.at, at.at + at.size)).includes('dOps'));
});

test('an encoder that reorders frames gets composition offsets', () => {
  const m = mp4Muxer({ video: { codec: 'avc1', width: 64, height: 64, fps: 10, description: new Uint8Array(5) } });
  // Decode order I P B: presentation 0, 2, 1.
  for (const [k, pts] of [[0, 0], [1, 2], [2, 1]]) m.add('video', frame(k), pts * 1e5, k === 0);
  const f = m.finish();
  assert.ok(new TextDecoder().decode(f).includes('ctts'));
});

// ---- WebM -------------------------------------------------------------------

function vint(b, at) {
  const first = b[at];
  let len = 1;
  while (len <= 8 && !(first & (0x80 >> (len - 1)))) len++;
  let v = first & ((0x80 >> (len - 1)) - 1);
  for (let i = 1; i < len; i++) v = v * 256 + b[at + i];
  return { v, len };
}

function elements(b, from = 0, to = b.length) {
  const out = [];
  for (let at = from; at < to;) {
    const first = b[at];
    let idLen = 1;
    while (!(first & (0x80 >> (idLen - 1)))) idLen++;
    let id = 0;
    for (let i = 0; i < idLen; i++) id = id * 256 + b[at + i];
    const size = vint(b, at + idLen);
    const data = at + idLen + size.len;
    out.push({ id, at, data, size: size.v });
    at = data + size.v;
  }
  return out;
}

test('a WebM is a header and a segment of clusters, a cluster per keyframe, with cues', () => {
  const m = webmMuxer({ video: { codec: 'V_VP9', width: 640, height: 360 }, audio: { sampleRate: 48000, channels: 1 } });
  for (let k = 0; k < 48; k++) m.add('video', frame(k), (k * 1e6) / 24, k % 24 === 0);
  for (let k = 0; k < 100; k++) m.add('audio', new Uint8Array(40), k * 20000);
  const f = m.finish();
  const top = elements(f);
  assert.deepEqual(top.map((e) => e.id), [0x1a45dfa3, 0x18538067]);
  assert.equal(top[1].data + top[1].size, f.length, 'the segment runs to the end');
  const seg = elements(f, top[1].data, top[1].data + top[1].size);
  const ids = seg.map((e) => e.id);
  assert.equal(ids.filter((id) => id === 0x1f43b675).length, 2, 'two keyframes, two clusters');
  assert.equal(ids[0], 0x114d9b74);
  assert.equal(ids.at(-1), 0x1c53bb6b);
  // Every block is counted once, and the second cluster starts at 1 s.
  let blocks = 0;
  const clusters = seg.filter((e) => e.id === 0x1f43b675);
  for (const c of clusters) blocks += elements(f, c.data, c.data + c.size).filter((e) => e.id === 0xa3).length;
  assert.equal(blocks, 148);
  const tc = elements(f, clusters[1].data, clusters[1].data + clusters[1].size)[0];
  assert.equal(tc.id, 0xe7);
  assert.equal(f[tc.data] * 256 + f[tc.data + 1], 1000);
  // The SeekHead points at the Cues, counted from the segment's data.
  const cues = seg.at(-1);
  const seeks = elements(f, seg[0].data, seg[0].data + seg[0].size);
  const positions = seeks.map((sk) => {
    const [sid, spos] = elements(f, sk.data, sk.data + sk.size);
    let id = 0;
    for (let i = 0; i < sid.size; i++) id = id * 256 + f[sid.data + i];
    let pos = 0;
    for (let i = 0; i < spos.size; i++) pos = pos * 256 + f[spos.data + i];
    return [id, pos];
  });
  const cuesPos = positions.find(([id]) => id === 0x1c53bb6b)[1];
  assert.equal(top[1].data + cuesPos, cues.at);
});
